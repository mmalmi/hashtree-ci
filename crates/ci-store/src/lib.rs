//! Hashtree storage backend for CI results.
//!
//! Stores job results at: npub1runner.../ci/npub1owner/repo/path/<commit>/

use async_trait::async_trait;
use ci_core::{JobResult, JobResultIndex};

/// Storage interface for CI results
#[async_trait]
pub trait CiStore: Send + Sync {
    /// Store a job result, returns its hashtree hash
    async fn store_result(&self, result: &JobResult) -> anyhow::Result<String>;

    /// Get a job result by hash
    async fn get_result(&self, hash: &str) -> anyhow::Result<Option<JobResult>>;

    /// Store raw log data, returns hash
    async fn store_logs(&self, logs: &[u8]) -> anyhow::Result<String>;

    /// Get logs by hash
    async fn get_logs(&self, hash: &str) -> anyhow::Result<Option<Vec<u8>>>;

    /// Store artifacts directory, returns hash
    async fn store_artifacts(&self, path: &std::path::Path) -> anyhow::Result<String>;

    /// Index a result for querying
    async fn index_result(&self, index: &JobResultIndex) -> anyhow::Result<()>;

    /// Query results by repo
    async fn query_by_repo(&self, repo_hash: &str, limit: usize) -> anyhow::Result<Vec<JobResultIndex>>;

    /// Query results by commit
    async fn query_by_commit(&self, repo_hash: &str, commit: &str) -> anyhow::Result<Vec<JobResultIndex>>;

    /// Query results by runner
    async fn query_by_runner(&self, runner_npub: &str, limit: usize) -> anyhow::Result<Vec<JobResultIndex>>;
}

/// In-memory store for testing
pub mod memory;

/// Hashtree-based store for production
pub mod hashtree;

pub use memory::MemoryStore;
pub use hashtree::{HashtreeStore, NpubPathStore};
