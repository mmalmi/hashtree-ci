//! CI Result Publisher
//!
//! Publishes CI results to the hashtree network:
//! 1. Upload result JSON and logs to Blossom servers
//! 2. Build a merkle tree of the CI results
//! 3. Publish Nostr event announcing the CI result tree
//!
//! Result tree structure:
//! ```
//! runner_npub/ci/<repo_path>/<commit>/
//!   result.json    - Job result with status, steps, timing
//!   logs/
//!     step1.txt    - Step logs
//!     step2.txt
//! ```

use ci_core::{HashtreeConfig, JobResult, JobStatus};
use nostr_sdk::prelude::*;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

/// CI result tree entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiResultTree {
    /// Merkle root hash of the CI result tree
    pub root_hash: String,
    /// Result JSON hash
    pub result_hash: String,
    /// Map of step name -> log hash
    pub log_hashes: HashMap<String, String>,
    /// Overall status
    pub status: JobStatus,
    /// Repo path (e.g., "hashtree-ts")
    pub repo_path: String,
    /// Commit SHA
    pub commit: String,
}

/// Publisher for CI results to Blossom and Nostr
pub struct CiPublisher {
    /// HTTP client for Blossom uploads
    http: Client,
    /// Nostr client for event publishing
    nostr: nostr_sdk::Client,
    /// Blossom servers for uploads
    blossom_servers: Vec<String>,
    /// Runner's keys for signing
    keys: Keys,
}

/// Blossom auth event for uploads (NIP-98 style, kind 24242)
fn create_blossom_auth(keys: &Keys, hash: &str, method: &str) -> Result<String, anyhow::Error> {
    let expiration = Timestamp::now().as_u64() + 300; // 5 min

    let tags = vec![
        Tag::parse(&["t", method])?,
        Tag::parse(&["x", hash])?,
        Tag::parse(&["expiration", &expiration.to_string()])?,
    ];

    let event = EventBuilder::new(Kind::Custom(24242), format!("{} {}", method, hash), tags)
        .to_event(keys)?;

    let json = serde_json::to_string(&event)?;
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, json);
    Ok(format!("Nostr {}", encoded))
}

impl CiPublisher {
    /// Create a new publisher
    pub async fn new(keys: Keys) -> anyhow::Result<Self> {
        let config = HashtreeConfig::load().unwrap_or_default();

        // Create Nostr client
        let nostr = nostr_sdk::Client::new(keys.clone());

        // Add relays
        for relay in &config.network.relays {
            nostr.add_relay(relay).await?;
        }
        nostr.connect().await;

        Ok(Self {
            http: Client::new(),
            nostr,
            blossom_servers: config.network.blossom_servers,
            keys,
        })
    }

    /// Upload a blob to Blossom servers
    /// Returns the SHA256 hash on success
    pub async fn upload_blob(&self, data: &[u8], content_type: &str) -> anyhow::Result<String> {
        let hash_bytes: [u8; 32] = Sha256::digest(data).into();
        let hash_hex = hex::encode(hash_bytes);

        let auth = create_blossom_auth(&self.keys, &hash_hex, "upload")?;

        // Try each server until one succeeds
        let mut last_error = None;
        for server in &self.blossom_servers {
            tracing::debug!("Uploading {} bytes to {}", data.len(), server);
            match self
                .http
                .put(format!("{}/upload", server))
                .header("Authorization", &auth)
                .header("Content-Type", content_type)
                .header("X-SHA-256", &hash_hex)
                .body(data.to_vec())
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() || resp.status().as_u16() == 409 => {
                    // 409 = already exists, that's fine
                    tracing::info!("Uploaded to {}: {}", server, hash_hex);
                    return Ok(hash_hex);
                }
                Ok(resp) => {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    tracing::warn!("Upload failed to {}: {} {}", server, status, text);
                    last_error = Some(anyhow::anyhow!("{}: {} {}", server, status, text));
                }
                Err(e) => {
                    tracing::warn!("Upload error to {}: {}", server, e);
                    last_error = Some(anyhow::anyhow!("{}: {}", server, e));
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("No Blossom servers configured")))
    }

    /// Publish CI result to hashtree network
    ///
    /// 1. Upload result JSON to Blossom
    /// 2. Upload logs to Blossom
    /// 3. Build tree manifest
    /// 4. Publish Nostr event with tree root
    pub async fn publish_result(
        &self,
        result: &JobResult,
        logs: &HashMap<String, Vec<u8>>,
        repo_path: &str,
        commit: &str,
    ) -> anyhow::Result<CiResultTree> {
        // 1. Upload result JSON
        let result_json = serde_json::to_vec_pretty(result)?;
        let result_hash = self.upload_blob(&result_json, "application/json").await?;

        // 2. Upload logs
        let mut log_hashes = HashMap::new();
        for (step_name, log_data) in logs {
            let hash = self.upload_blob(log_data, "text/plain").await?;
            log_hashes.insert(step_name.clone(), hash);
        }

        // 3. Build tree manifest (simple JSON for now, could be proper merkle tree)
        let manifest = CiResultManifest {
            result_hash: result_hash.clone(),
            log_hashes: log_hashes.clone(),
            status: result.status.clone(),
            repo_path: repo_path.to_string(),
            commit: commit.to_string(),
            runner_npub: self.keys.public_key().to_bech32()?,
            created_at: chrono::Utc::now().timestamp(),
        };

        let manifest_json = serde_json::to_vec_pretty(&manifest)?;
        let root_hash = self.upload_blob(&manifest_json, "application/json").await?;

        // 4. Publish Nostr event announcing CI result
        // Use kind 30078 with 'd' tag = ci/<repo_path>/<commit>
        let d_tag = format!("ci/{}/{}", repo_path, commit);

        let tags = vec![
            Tag::parse(&["d", &d_tag])?,
            Tag::parse(&["l", "hashtree-ci"])?,
            Tag::parse(&["hash", &root_hash])?,
            Tag::parse(&["status", &format!("{:?}", result.status)])?,
            Tag::parse(&["commit", commit])?,
            Tag::parse(&["repo", repo_path])?,
        ];

        let event = EventBuilder::new(Kind::Custom(30078), "", tags).to_event(&self.keys)?;

        self.nostr.send_event(event).await?;

        Ok(CiResultTree {
            root_hash,
            result_hash,
            log_hashes,
            status: result.status.clone(),
            repo_path: repo_path.to_string(),
            commit: commit.to_string(),
        })
    }

    /// Get the view URL for a published CI result
    pub fn view_url(&self, repo_path: &str, commit: &str) -> String {
        let npub = self.keys.public_key().to_bech32().unwrap_or_default();
        format!(
            "https://files.iris.to/#/{}/ci/{}/{}",
            npub,
            repo_path,
            &commit[..8.min(commit.len())]
        )
    }
}

/// CI result manifest stored in Blossom
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiResultManifest {
    pub result_hash: String,
    pub log_hashes: HashMap<String, String>,
    pub status: JobStatus,
    pub repo_path: String,
    pub commit: String,
    pub runner_npub: String,
    pub created_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_blossom_auth() {
        let keys = Keys::generate();
        let hash = "abcd1234567890abcd1234567890abcd1234567890abcd1234567890abcd1234";

        let auth = create_blossom_auth(&keys, hash, "upload").unwrap();

        // Should start with "Nostr "
        assert!(auth.starts_with("Nostr "));

        // Should be valid base64
        let encoded = &auth[6..]; // Skip "Nostr "
        let decoded = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded);
        assert!(decoded.is_ok());

        // Should be valid JSON event
        let event: serde_json::Value = serde_json::from_slice(&decoded.unwrap()).unwrap();
        assert_eq!(event["kind"], 24242);
    }

    #[test]
    fn test_ci_result_manifest_serialization() {
        let manifest = CiResultManifest {
            result_hash: "abc123".to_string(),
            log_hashes: HashMap::from([("step1".to_string(), "def456".to_string())]),
            status: JobStatus::Success,
            repo_path: "hashtree-ts".to_string(),
            commit: "abc123def".to_string(),
            runner_npub: "npub1test".to_string(),
            created_at: 1234567890,
        };

        let json = serde_json::to_string(&manifest).unwrap();
        let parsed: CiResultManifest = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.result_hash, manifest.result_hash);
        assert_eq!(parsed.status, JobStatus::Success);
    }

    #[test]
    fn test_view_url_format() {
        // Test URL format without actual publisher
        let npub = "npub1tsrng9wsz55988794jucdf3cgmh065e29sgevt8t8wqc687r7p9suu35fu";
        let repo_path = "hashtree-ts";
        let commit = "abc123def456";

        let url = format!(
            "https://files.iris.to/#/{}/ci/{}/{}",
            npub,
            repo_path,
            &commit[..8]
        );

        assert_eq!(
            url,
            "https://files.iris.to/#/npub1tsrng9wsz55988794jucdf3cgmh065e29sgevt8t8wqc687r7p9suu35fu/ci/hashtree-ts/abc123de"
        );
    }
}
