//! In-memory CI store for testing.

use async_trait::async_trait;
use ci_core::{JobResult, JobResultIndex};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::RwLock;

use crate::CiStore;

pub struct MemoryStore {
    results: RwLock<HashMap<String, JobResult>>,
    logs: RwLock<HashMap<String, Vec<u8>>>,
    index: RwLock<Vec<JobResultIndex>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self {
            results: RwLock::new(HashMap::new()),
            logs: RwLock::new(HashMap::new()),
            index: RwLock::new(Vec::new()),
        }
    }
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CiStore for MemoryStore {
    async fn store_result(&self, result: &JobResult) -> anyhow::Result<String> {
        let json = serde_json::to_vec(result)?;
        let hash = hex::encode(Sha256::digest(&json));
        self.results.write().unwrap().insert(hash.clone(), result.clone());
        Ok(hash)
    }

    async fn get_result(&self, hash: &str) -> anyhow::Result<Option<JobResult>> {
        Ok(self.results.read().unwrap().get(hash).cloned())
    }

    async fn store_logs(&self, logs: &[u8]) -> anyhow::Result<String> {
        let hash = hex::encode(Sha256::digest(logs));
        self.logs.write().unwrap().insert(hash.clone(), logs.to_vec());
        Ok(hash)
    }

    async fn get_logs(&self, hash: &str) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(self.logs.read().unwrap().get(hash).cloned())
    }

    async fn store_artifacts(&self, _path: &std::path::Path) -> anyhow::Result<String> {
        // TODO: implement with hashtree-fs
        Ok("not-implemented".to_string())
    }

    async fn index_result(&self, index: &JobResultIndex) -> anyhow::Result<()> {
        self.index.write().unwrap().push(index.clone());
        Ok(())
    }

    async fn query_by_repo(&self, repo_hash: &str, limit: usize) -> anyhow::Result<Vec<JobResultIndex>> {
        let index = self.index.read().unwrap();
        Ok(index
            .iter()
            .filter(|i| i.repo_hash == repo_hash)
            .take(limit)
            .cloned()
            .collect())
    }

    async fn query_by_commit(&self, repo_hash: &str, commit: &str) -> anyhow::Result<Vec<JobResultIndex>> {
        let index = self.index.read().unwrap();
        Ok(index
            .iter()
            .filter(|i| i.repo_hash == repo_hash && i.commit == commit)
            .cloned()
            .collect())
    }

    async fn query_by_runner(&self, runner_npub: &str, limit: usize) -> anyhow::Result<Vec<JobResultIndex>> {
        let index = self.index.read().unwrap();
        Ok(index
            .iter()
            .filter(|i| i.runner_npub == runner_npub)
            .take(limit)
            .cloned()
            .collect())
    }
}
