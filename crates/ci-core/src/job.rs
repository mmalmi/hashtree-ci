//! Job definitions and status tracking.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A CI job to be executed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    /// Unique job identifier
    pub id: Uuid,

    /// Repository root hash (htree://...)
    pub repo_hash: String,

    /// Git commit SHA being built
    pub commit: String,

    /// Workflow file path (e.g., ".github/workflows/ci.yml")
    pub workflow: String,

    /// Specific job name within workflow
    pub job_name: String,

    /// Required runner tags (from `runs-on`)
    pub runs_on: Vec<String>,

    /// Job steps to execute
    pub steps: Vec<JobStep>,

    /// When the job was queued
    pub queued_at: DateTime<Utc>,

    /// Environment variables
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

/// A single step within a job
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobStep {
    /// Step name (for display)
    pub name: String,

    /// Step type
    #[serde(flatten)]
    pub action: StepAction,

    /// Working directory
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,

    /// Environment variables for this step
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,

    /// Continue on error
    #[serde(default)]
    pub continue_on_error: bool,

    /// Timeout in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_minutes: Option<u32>,
}

/// Type of action to execute
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepAction {
    /// Run a shell command
    Run(String),

    /// Use a pre-built action (e.g., actions/checkout@v4)
    Uses {
        action: String,
        #[serde(default)]
        with: std::collections::HashMap<String, String>,
    },
}

/// Current status of a job
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    /// Waiting for a runner
    Queued,
    /// Currently executing
    Running,
    /// Completed successfully
    Success,
    /// Completed with failure
    Failure,
    /// Cancelled by user
    Cancelled,
    /// Skipped (conditional)
    Skipped,
}

impl Job {
    pub fn new(
        repo_hash: String,
        commit: String,
        workflow: String,
        job_name: String,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            repo_hash,
            commit,
            workflow,
            job_name,
            runs_on: vec![],
            steps: vec![],
            queued_at: Utc::now(),
            env: std::collections::HashMap::new(),
        }
    }
}
