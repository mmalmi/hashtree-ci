//! Hashtree-based CI store implementation.
//!
//! Stores CI results at: npub1runner.../ci/npub1owner/path/to/repo/<commit>/

use async_trait::async_trait;
use ci_core::{JobResult, JobResultIndex};
use sha2::Digest;
use std::path::PathBuf;

use crate::CiStore;

/// Hashtree-backed CI store
pub struct HashtreeStore {
    /// Base path for local storage (e.g., ~/.local/share/hashtree-ci/)
    base_path: PathBuf,
    /// This runner's npub
    runner_npub: String,
}

impl HashtreeStore {
    pub fn new(base_path: PathBuf, runner_npub: String) -> Self {
        Self {
            base_path,
            runner_npub,
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
        use sha2::Digest;
        let hash = hex::encode(sha2::Sha256::digest(logs));
        Ok(hash)
    }

    async fn get_logs(&self, hash: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let _ = hash;
        Ok(None)
    }

    async fn store_artifacts(&self, _path: &std::path::Path) -> anyhow::Result<String> {
        Ok("not-implemented".to_string())
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
}
