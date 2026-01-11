//! Hashtree-based CI store implementation.
//!
//! Stores CI results at: npub1runner.../ci/npub1owner/path/to/repo/<commit>/
//!
//! Uses hashtree-fs for content-addressable blob storage, compatible with
//! the hashtree network and Blossom servers.

use async_trait::async_trait;
use ci_core::{JobResult, JobResultIndex};
use hashtree_fs::FsBlobStore;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::collections::HashMap;
use std::path::PathBuf;

use crate::CiStore;

/// Artifact manifest - lists all files in an artifact directory
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactManifest {
    /// Map of relative path -> content hash (hex)
    pub files: HashMap<String, ArtifactFile>,
    /// Total size in bytes
    pub total_size: u64,
    /// When the artifacts were stored
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// An artifact file entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactFile {
    /// SHA256 hash of file content (hex)
    pub hash: String,
    /// File size in bytes
    pub size: u64,
}

/// Hashtree-backed CI store using hashtree-fs for blob storage
pub struct HashtreeStore {
    /// Base path for CI-specific data (results, logs)
    base_path: PathBuf,
    /// Hashtree blob store for content-addressable storage
    blob_store: FsBlobStore,
}

impl HashtreeStore {
    /// Create a new store with CI data at base_path and blobs in base_path/blobs
    pub fn new(base_path: PathBuf, _runner_npub: String) -> Self {
        let blobs_path = base_path.join("blobs");
        let blob_store = FsBlobStore::new(&blobs_path)
            .expect("Failed to create blob store");
        Self {
            base_path,
            blob_store,
        }
    }

    /// Get the path for CI results for a repo/commit
    /// Format: base_path/ci/<owner_npub>/<repo_path>/<commit>/
    fn result_dir(&self, repo_npub: &str, repo_path: &str, commit: &str) -> PathBuf {
        self.base_path
            .join("ci")
            .join(repo_npub)
            .join(repo_path)
            .join(commit)
    }

    /// Get path to result.json
    fn result_file(&self, repo_npub: &str, repo_path: &str, commit: &str) -> PathBuf {
        self.result_dir(repo_npub, repo_path, commit)
            .join("result.json")
    }

    /// Get path to logs directory
    fn logs_dir(&self, repo_npub: &str, repo_path: &str, commit: &str) -> PathBuf {
        self.result_dir(repo_npub, repo_path, commit).join("logs")
    }

    /// Store a blob using hashtree-fs, returns hex hash
    fn store_blob(&self, content: &[u8]) -> anyhow::Result<String> {
        let hash_bytes: [u8; 32] = sha2::Sha256::digest(content).into();
        self.blob_store.put_sync(hash_bytes, content)?;
        Ok(hex::encode(hash_bytes))
    }

    /// Get a blob by hex hash using hashtree-fs
    fn get_blob(&self, hash_hex: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let hash_bytes: [u8; 32] = hex::decode(hash_hex)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("Invalid hash length"))?;
        Ok(self.blob_store.get_sync(&hash_bytes)?)
    }

    /// Store artifacts from a directory, returning the manifest hash
    async fn store_artifacts_internal(&self, path: &std::path::Path) -> anyhow::Result<String> {
        use tokio::fs;

        let mut files = HashMap::new();
        let mut total_size = 0u64;

        // Recursively process directory
        let mut stack = vec![(path.to_path_buf(), String::new())];

        while let Some((current_path, prefix)) = stack.pop() {
            let mut entries = fs::read_dir(&current_path).await?;

            while let Some(entry) = entries.next_entry().await? {
                let entry_path = entry.path();
                let file_name = entry.file_name().to_string_lossy().to_string();
                let relative_path = if prefix.is_empty() {
                    file_name.clone()
                } else {
                    format!("{}/{}", prefix, file_name)
                };

                let metadata = fs::metadata(&entry_path).await?;

                if metadata.is_dir() {
                    stack.push((entry_path, relative_path));
                } else if metadata.is_file() {
                    let content = fs::read(&entry_path).await?;
                    let size = content.len() as u64;
                    let hash = self.store_blob(&content)?;

                    files.insert(
                        relative_path,
                        ArtifactFile { hash, size },
                    );
                    total_size += size;
                }
            }
        }

        // Create and store manifest
        let manifest = ArtifactManifest {
            files,
            total_size,
            created_at: chrono::Utc::now(),
        };

        let manifest_json = serde_json::to_vec_pretty(&manifest)?;
        let manifest_hash = self.store_blob(&manifest_json)?;

        Ok(manifest_hash)
    }
}

#[async_trait]
impl CiStore for HashtreeStore {
    async fn store_result(&self, result: &JobResult) -> anyhow::Result<String> {
        // Parse repo info from result
        let (repo_npub, repo_path) = parse_repo_identity(&result.repo_hash)?;

        let dir = self.result_dir(&repo_npub, &repo_path, &result.commit);
        tokio::fs::create_dir_all(&dir).await?;

        let file = self.result_file(&repo_npub, &repo_path, &result.commit);
        let json = serde_json::to_vec_pretty(result)?;
        tokio::fs::write(&file, &json).await?;

        // Return the "path" as the hash
        let hash = hex::encode(sha2::Sha256::digest(&json));
        Ok(hash)
    }

    async fn get_result(&self, hash: &str) -> anyhow::Result<Option<JobResult>> {
        // Hash lookup not directly supported - need repo/commit info
        // This is a limitation of the path-based design
        let _ = hash;
        Ok(None)
    }

    async fn store_logs(&self, logs: &[u8]) -> anyhow::Result<String> {
        // Store logs in hashtree-fs blob store
        self.store_blob(logs)
    }

    async fn get_logs(&self, hash: &str) -> anyhow::Result<Option<Vec<u8>>> {
        self.get_blob(hash)
    }

    async fn store_artifacts(&self, path: &std::path::Path) -> anyhow::Result<String> {
        if !path.exists() {
            anyhow::bail!("Artifacts path does not exist: {}", path.display());
        }

        if path.is_file() {
            // Single file - store directly as blob
            let content = tokio::fs::read(path).await?;
            self.store_blob(&content)
        } else {
            // Directory - store as manifest with all files
            self.store_artifacts_internal(path).await
        }
    }

    async fn index_result(&self, _index: &JobResultIndex) -> anyhow::Result<()> {
        // No separate index needed - path-based lookup
        Ok(())
    }

    async fn query_by_repo(
        &self,
        repo_hash: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<JobResultIndex>> {
        let (repo_npub, repo_path) = parse_repo_identity(repo_hash)?;
        let repo_dir = self
            .base_path
            .join("ci")
            .join(&repo_npub)
            .join(&repo_path);

        if !repo_dir.exists() {
            return Ok(vec![]);
        }

        let mut results = Vec::new();
        let mut entries = tokio::fs::read_dir(&repo_dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            if results.len() >= limit {
                break;
            }

            let commit = entry.file_name().to_string_lossy().to_string();
            let result_file = entry.path().join("result.json");

            if result_file.exists() {
                let content = tokio::fs::read(&result_file).await?;
                if let Ok(result) = serde_json::from_slice::<JobResult>(&content) {
                    let hash = hex::encode(sha2::Sha256::digest(&content));
                    results.push(result.to_index(hash));
                }
            }
        }

        // Sort by finished_at descending
        results.sort_by(|a, b| b.finished_at.cmp(&a.finished_at));

        Ok(results)
    }

    async fn query_by_commit(
        &self,
        repo_hash: &str,
        commit: &str,
    ) -> anyhow::Result<Vec<JobResultIndex>> {
        let (repo_npub, repo_path) = parse_repo_identity(repo_hash)?;
        let result_file = self.result_file(&repo_npub, &repo_path, commit);

        if !result_file.exists() {
            return Ok(vec![]);
        }

        let content = tokio::fs::read(&result_file).await?;
        let result: JobResult = serde_json::from_slice(&content)?;
        let hash = hex::encode(sha2::Sha256::digest(&content));

        Ok(vec![result.to_index(hash)])
    }

    async fn query_by_runner(
        &self,
        runner_npub: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<JobResultIndex>> {
        // Would need to scan all repos - not efficient
        // For now, return empty
        let _ = (runner_npub, limit);
        Ok(vec![])
    }
}

/// Extended store trait for npub/path based storage
#[async_trait]
pub trait NpubPathStore: Send + Sync {
    /// Store result for a specific repo and commit
    async fn store_ci_result(
        &self,
        repo_npub: &str,
        repo_path: &str,
        commit: &str,
        result: &JobResult,
    ) -> anyhow::Result<()>;

    /// Store logs for a specific step
    async fn store_step_logs(
        &self,
        repo_npub: &str,
        repo_path: &str,
        commit: &str,
        step_name: &str,
        logs: &[u8],
    ) -> anyhow::Result<()>;

    /// Get result for a specific commit
    async fn get_ci_result(
        &self,
        repo_npub: &str,
        repo_path: &str,
        commit: &str,
    ) -> anyhow::Result<Option<JobResult>>;

    /// Check if CI has run for a commit
    async fn has_ci_result(
        &self,
        repo_npub: &str,
        repo_path: &str,
        commit: &str,
    ) -> anyhow::Result<bool>;
}

#[async_trait]
impl NpubPathStore for HashtreeStore {
    async fn store_ci_result(
        &self,
        repo_npub: &str,
        repo_path: &str,
        commit: &str,
        result: &JobResult,
    ) -> anyhow::Result<()> {
        let dir = self.result_dir(repo_npub, repo_path, commit);
        tokio::fs::create_dir_all(&dir).await?;

        let file = self.result_file(repo_npub, repo_path, commit);
        let json = serde_json::to_vec_pretty(result)?;
        tokio::fs::write(&file, &json).await?;

        Ok(())
    }

    async fn store_step_logs(
        &self,
        repo_npub: &str,
        repo_path: &str,
        commit: &str,
        step_name: &str,
        logs: &[u8],
    ) -> anyhow::Result<()> {
        let logs_dir = self.logs_dir(repo_npub, repo_path, commit);
        tokio::fs::create_dir_all(&logs_dir).await?;

        // Sanitize step name for filename
        let safe_name = step_name
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect::<String>();

        let log_file = logs_dir.join(format!("{}.txt", safe_name));
        tokio::fs::write(&log_file, logs).await?;

        Ok(())
    }

    async fn get_ci_result(
        &self,
        repo_npub: &str,
        repo_path: &str,
        commit: &str,
    ) -> anyhow::Result<Option<JobResult>> {
        let file = self.result_file(repo_npub, repo_path, commit);

        if !file.exists() {
            return Ok(None);
        }

        let content = tokio::fs::read(&file).await?;
        let result = serde_json::from_slice(&content)?;
        Ok(Some(result))
    }

    async fn has_ci_result(
        &self,
        repo_npub: &str,
        repo_path: &str,
        commit: &str,
    ) -> anyhow::Result<bool> {
        let file = self.result_file(repo_npub, repo_path, commit);
        Ok(file.exists())
    }
}

/// Parse repo identity from "npub1owner/path/to/repo" format
fn parse_repo_identity(repo_hash: &str) -> anyhow::Result<(String, String)> {
    // Format: npub1owner/repos/myproject or just the hash
    if repo_hash.starts_with("npub1") {
        if let Some(idx) = repo_hash.find('/') {
            let npub = &repo_hash[..idx];
            let path = &repo_hash[idx + 1..];
            return Ok((npub.to_string(), path.to_string()));
        }
    }

    // Fallback: treat as hash
    Ok(("unknown".to_string(), repo_hash.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CiStore;
    use tempfile::tempdir;

    #[test]
    fn test_parse_repo_identity() {
        let (npub, path) = parse_repo_identity("npub1owner/repos/myproject").unwrap();
        assert_eq!(npub, "npub1owner");
        assert_eq!(path, "repos/myproject");
    }

    #[tokio::test]
    async fn test_hashtree_store_paths() {
        let store = HashtreeStore::new("/tmp/test".into(), "npub1runner".to_string());

        let dir = store.result_dir("npub1owner", "repos/myproject", "abc123");
        assert_eq!(
            dir.to_string_lossy(),
            "/tmp/test/ci/npub1owner/repos/myproject/abc123"
        );
    }

    #[tokio::test]
    async fn test_blob_storage() {
        let temp = tempdir().unwrap();
        let store = HashtreeStore::new(temp.path().to_path_buf(), "npub1runner".to_string());

        // Store a blob using hashtree-fs
        let content = b"Hello, hashtree-fs!";
        let hash = store.store_blob(content).unwrap();

        // Verify hash is a valid SHA256 hex
        assert_eq!(hash.len(), 64);

        // Retrieve the blob
        let retrieved = store.get_blob(&hash).unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap(), content);

        // Verify blob doesn't exist for random hash
        let fake = store.get_blob("0000000000000000000000000000000000000000000000000000000000000000").unwrap();
        assert!(fake.is_none());
    }

    #[tokio::test]
    async fn test_store_logs() {
        let temp = tempdir().unwrap();
        let store = HashtreeStore::new(temp.path().to_path_buf(), "npub1runner".to_string());

        let logs = b"Build output:\n[OK] Step 1\n[OK] Step 2\n";
        let hash = store.store_logs(logs).await.unwrap();

        let retrieved = store.get_logs(&hash).await.unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap(), logs);
    }

    #[tokio::test]
    async fn test_store_artifacts_file() {
        let temp = tempdir().unwrap();
        let store = HashtreeStore::new(temp.path().to_path_buf(), "npub1runner".to_string());

        // Create a test file
        let artifact_file = temp.path().join("artifact.txt");
        tokio::fs::write(&artifact_file, b"Test artifact content").await.unwrap();

        // Store it
        let hash = store.store_artifacts(&artifact_file).await.unwrap();
        assert_eq!(hash.len(), 64);

        // Verify we can retrieve it via hashtree-fs blob store
        let retrieved = store.get_blob(&hash).unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap(), b"Test artifact content");
    }

    #[tokio::test]
    async fn test_store_artifacts_directory() {
        let temp = tempdir().unwrap();
        let store = HashtreeStore::new(temp.path().to_path_buf(), "npub1runner".to_string());

        // Create a test directory structure
        let artifacts_dir = temp.path().join("artifacts");
        tokio::fs::create_dir_all(artifacts_dir.join("subdir")).await.unwrap();
        tokio::fs::write(artifacts_dir.join("file1.txt"), b"Content 1").await.unwrap();
        tokio::fs::write(artifacts_dir.join("file2.txt"), b"Content 2").await.unwrap();
        tokio::fs::write(artifacts_dir.join("subdir/file3.txt"), b"Content 3").await.unwrap();

        // Store the directory
        let manifest_hash = store.store_artifacts(&artifacts_dir).await.unwrap();
        assert_eq!(manifest_hash.len(), 64);

        // Retrieve and parse the manifest from hashtree-fs
        let manifest_bytes = store.get_blob(&manifest_hash).unwrap().unwrap();
        let manifest: ArtifactManifest = serde_json::from_slice(&manifest_bytes).unwrap();

        // Verify manifest contents
        assert_eq!(manifest.files.len(), 3);
        assert!(manifest.files.contains_key("file1.txt"));
        assert!(manifest.files.contains_key("file2.txt"));
        assert!(manifest.files.contains_key("subdir/file3.txt"));

        // Verify file contents are retrievable from hashtree-fs
        let file1_hash = &manifest.files["file1.txt"].hash;
        let file1_content = store.get_blob(file1_hash).unwrap().unwrap();
        assert_eq!(file1_content, b"Content 1");

        // Verify total size
        assert_eq!(manifest.total_size, 9 + 9 + 9); // "Content 1" + "Content 2" + "Content 3"
    }
}
