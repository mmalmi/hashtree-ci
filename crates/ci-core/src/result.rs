//! CI job results stored in hashtree.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::JobStatus;

/// Complete result of a CI job, stored in hashtree.
///
/// Authenticity is verified via the Nostr event that publishes the merkle root.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobResult {
    /// Job identifier
    pub job_id: Uuid,

    /// Runner's npub that executed this job
    pub runner_npub: String,

    /// Repository root hash
    pub repo_hash: String,

    /// Git commit SHA
    pub commit: String,

    /// Workflow file path
    pub workflow: String,

    /// Job name within workflow
    pub job_name: String,

    /// Final status
    pub status: JobStatus,

    /// When execution started
    pub started_at: DateTime<Utc>,

    /// When execution finished
    pub finished_at: DateTime<Utc>,

    /// Hashtree hash of combined logs
    pub logs_hash: String,

    /// Hashtree hash of artifacts directory (if any)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifacts_hash: Option<String>,

    /// Results of individual steps
    pub steps: Vec<StepResult>,
}

/// Result of a single step
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    /// Step name
    pub name: String,

    /// Step status
    pub status: JobStatus,

    /// Exit code (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,

    /// Duration in seconds
    pub duration_secs: u64,

    /// Hashtree hash of this step's logs
    pub logs_hash: String,

    /// Error message (if failed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Index entry for querying job results.
/// Stored separately for efficient lookups.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobResultIndex {
    /// Hashtree hash of the full JobResult
    pub result_hash: String,

    /// Runner's npub
    pub runner_npub: String,

    /// Repository hash
    pub repo_hash: String,

    /// Git commit
    pub commit: String,

    /// Workflow path
    pub workflow: String,

    /// Job name
    pub job_name: String,

    /// Final status
    pub status: JobStatus,

    /// Completion time (for sorting)
    pub finished_at: DateTime<Utc>,
}

impl JobResult {
    /// Create a new job result
    pub fn new(
        job_id: Uuid,
        runner_npub: String,
        repo_hash: String,
        commit: String,
        workflow: String,
        job_name: String,
    ) -> Self {
        let now = Utc::now();
        Self {
            job_id,
            runner_npub,
            repo_hash,
            commit,
            workflow,
            job_name,
            status: JobStatus::Running,
            started_at: now,
            finished_at: now,
            logs_hash: String::new(),
            artifacts_hash: None,
            steps: vec![],
        }
    }

    /// Create index entry for this result
    pub fn to_index(&self, result_hash: String) -> JobResultIndex {
        JobResultIndex {
            result_hash,
            runner_npub: self.runner_npub.clone(),
            repo_hash: self.repo_hash.clone(),
            commit: self.commit.clone(),
            workflow: self.workflow.clone(),
            job_name: self.job_name.clone(),
            status: self.status,
            finished_at: self.finished_at,
        }
    }
}
