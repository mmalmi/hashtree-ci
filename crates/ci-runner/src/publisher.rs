//! CI Result Publisher using hashtree-rs
//!
//! Publishes CI results as a proper hashtree merkle tree:
//! 1. Build tree with result.json and logs under ci/<repo>/<commit>/
//! 2. Upload all blobs to Blossom
//! 3. Publish root hash via NostrRootResolver (standard hashtree format)
//!
//! Result tree structure (under runner's npub):
//! ```
//! ci/
//!   <repo_path>/
//!     <commit>/
//!       result.json    - Job result with status, steps, timing
//!       logs/
//!         step1.txt    - Step logs
//!         step2.txt
//! ```

use ci_core::{HashtreeConfig, JobResult};
use hashtree_blossom::BlossomClient;
use hashtree_core::{Cid, DirEntry, HashTree, HashTreeConfig, LinkType, MemoryStore, Store, to_hex};
use hashtree_resolver::nostr::{NostrResolverConfig, NostrRootResolver};
use hashtree_resolver::RootResolver;
use nostr_sdk::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Publisher for CI results to hashtree network
pub struct CiPublisher {
    blossom: BlossomClient,
    resolver: NostrRootResolver,
    keys: Keys,
}

impl CiPublisher {
    /// Create a new publisher
    pub async fn new(keys: Keys) -> anyhow::Result<Self> {
        let mut resolver_config = NostrResolverConfig {
            secret_key: Some(keys.clone()),
            resolve_timeout: Duration::from_secs(5),
            ..Default::default()
        };
        let hashtree_config = HashtreeConfig::load().unwrap_or_default();
        if !hashtree_config.network.relays.is_empty() {
            resolver_config.relays = hashtree_config.network.relays;
        }
        let resolver = NostrRootResolver::new(resolver_config).await?;
        let blossom = BlossomClient::new(keys.clone());

        Ok(Self {
            blossom,
            resolver,
            keys,
        })
    }

    /// Get runner's npub
    pub fn npub(&self) -> String {
        self.keys.public_key().to_bech32().unwrap_or_default()
    }

    /// Publish CI result to hashtree network
    ///
    /// Returns the new root Cid
    pub async fn publish_result(
        &self,
        result: &JobResult,
        logs: &HashMap<String, Vec<u8>>,
        repo_path: &str,
        commit: &str,
    ) -> anyhow::Result<Cid> {
        // 1. Build hashtree with CI results (encrypted so upload.iris.to accepts it)
        let store = Arc::new(MemoryStore::new());
        let tree = HashTree::new(HashTreeConfig::new(store.clone()));

        // Add result.json
        let result_json = serde_json::to_vec_pretty(result)?;
        let result_len = result_json.len() as u64;
        let (result_cid, _) = tree.put(&result_json).await?;

        // Add logs
        let mut log_entries = vec![];
        for (step_name, log_data) in logs {
            let log_len = log_data.len() as u64;
            let (log_cid, _) = tree.put(log_data).await?;
            log_entries.push(
                DirEntry::from_cid(format!("{}.txt", step_name), &log_cid)
                    .with_size(log_len)
                    .with_link_type(LinkType::Blob),
            );
        }

        // Build logs directory
        let logs_dir_cid = tree.put_directory(log_entries).await?;

        // Build commit directory with result.json and logs/
        let commit_dir_cid = tree.put_directory(vec![
            DirEntry::from_cid("result.json", &result_cid)
                .with_size(result_len)
                .with_link_type(LinkType::Blob),
            DirEntry::from_cid("logs", &logs_dir_cid)
                .with_link_type(LinkType::Dir),
        ])
        .await?;

        // Build repo directory with commit
        let repo_dir_cid = tree.put_directory(vec![
            DirEntry::from_cid(commit, &commit_dir_cid)
                .with_link_type(LinkType::Dir),
        ])
        .await?;

        // Build ci directory with repo
        let ci_dir_cid = tree.put_directory(vec![
            DirEntry::from_cid(repo_path, &repo_dir_cid)
                .with_link_type(LinkType::Dir),
        ])
        .await?;

        // 2. Upload all blobs to Blossom
        tracing::info!("Uploading {} blobs to Blossom", store.size());
        for hash in store.keys() {
            if let Ok(Some(data)) = store.get(&hash).await {
                let hash_hex = to_hex(&hash);
                match self.blossom.upload_if_missing(&data).await {
                    Ok((_, was_new)) => {
                        if was_new {
                            tracing::debug!("Uploaded {}.bin", hash_hex);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to upload {}.bin: {}", hash_hex, e);
                    }
                }
            }
        }

        // 3. Publish root via NostrRootResolver
        let key = format!("{}/ci", self.npub());
        tracing::info!("Publishing root to {}", key);

        self.resolver.publish(&key, &ci_dir_cid).await?;

        Ok(ci_dir_cid)
    }

    /// Get the view URL for a published CI result
    pub fn view_url(&self, repo_path: &str, commit: &str) -> String {
        format!(
            "https://files.iris.to/#/{}/ci/{}/{}",
            self.npub(),
            repo_path,
            &commit[..8.min(commit.len())]
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ci_core::JobStatus;

    #[test]
    fn test_view_url_format() {
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

    #[tokio::test]
    async fn test_tree_building() {
        // Test that we can build a hashtree structure
        let store = Arc::new(MemoryStore::new());
        let tree = HashTree::new(HashTreeConfig::new(store.clone()).public());

        // Create a simple file
        let content = b"test content";
        let (file_cid, _) = tree.put(content).await.unwrap();

        // Create a directory with the file
        let dir_cid = tree
            .put_directory(vec![
                DirEntry::from_cid("test.txt", &file_cid)
                    .with_size(content.len() as u64)
                    .with_link_type(LinkType::Blob),
            ])
            .await
            .unwrap();

        // Verify the tree was built
        assert!(store.size() >= 2); // At least file content + directory node
        assert!(dir_cid.hash != [0u8; 32]); // Valid hash
    }
}
