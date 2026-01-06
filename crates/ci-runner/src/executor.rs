//! Job executor - runs CI jobs and captures output.

use ci_core::{Job, JobResult, JobStatus, RunnerIdentity, StepAction, StepResult};
use chrono::Utc;
use sha2::Digest;
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

/// Execute a CI job and return the result
pub async fn execute_job(
    job: &Job,
    runner: &RunnerIdentity,
    work_dir: &Path,
) -> anyhow::Result<JobResult> {
    let mut result = JobResult::new(
        job.id,
        runner.npub(),
        job.repo_hash.clone(),
        job.commit.clone(),
        job.workflow.clone(),
        job.job_name.clone(),
    );

    let mut all_logs = Vec::new();
    let mut job_failed = false;

    for step in &job.steps {
        let step_start = std::time::Instant::now();

        let (status, exit_code, logs, error) = match &step.action {
            StepAction::Run(cmd) => execute_shell_step(cmd, work_dir, &job.env).await,
            StepAction::Uses { action, with: _ } => {
                // TODO: Implement action support
                let msg = format!("Action '{}' not yet supported", action);
                (JobStatus::Skipped, None, msg.as_bytes().to_vec(), Some(msg))
            }
        };

        let duration = step_start.elapsed().as_secs();

        // Store step logs
        let logs_hash = hex::encode(sha2::Sha256::digest(&logs));
        all_logs.extend_from_slice(&logs);
        all_logs.push(b'\n');

        let step_result = StepResult {
            name: step.name.clone(),
            status,
            exit_code,
            duration_secs: duration,
            logs_hash,
            error,
        };

        result.steps.push(step_result);

        // Check if we should continue
        if status == JobStatus::Failure && !step.continue_on_error {
            job_failed = true;
            break;
        }
    }

    // Set final status
    result.status = if job_failed {
        JobStatus::Failure
    } else {
        JobStatus::Success
    };

    result.finished_at = Utc::now();
    result.logs_hash = hex::encode(sha2::Sha256::digest(&all_logs));

    // Sign the result
    result.sign(&runner.nsec())?;

    Ok(result)
}

/// Execute a shell command step
async fn execute_shell_step(
    cmd: &str,
    work_dir: &Path,
    env: &std::collections::HashMap<String, String>,
) -> (JobStatus, Option<i32>, Vec<u8>, Option<String>) {
    let mut logs = Vec::new();

    // Log the command being run
    logs.extend_from_slice(format!("$ {}\n", cmd).as_bytes());

    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(cmd)
        .current_dir(work_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Add environment variables
    for (key, value) in env {
        command.env(key, value);
    }

    let child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            let error = format!("Failed to spawn command: {}", e);
            logs.extend_from_slice(error.as_bytes());
            return (JobStatus::Failure, None, logs, Some(error));
        }
    };

    let output = match child.wait_with_output().await {
        Ok(o) => o,
        Err(e) => {
            let error = format!("Failed to wait for command: {}", e);
            logs.extend_from_slice(error.as_bytes());
            return (JobStatus::Failure, None, logs, Some(error));
        }
    };

    // Capture stdout and stderr
    logs.extend_from_slice(&output.stdout);
    if !output.stderr.is_empty() {
        logs.extend_from_slice(b"\n[stderr]\n");
        logs.extend_from_slice(&output.stderr);
    }

    let exit_code = output.status.code();
    let status = if output.status.success() {
        JobStatus::Success
    } else {
        JobStatus::Failure
    };

    let error = if !output.status.success() {
        Some(format!("Exit code: {:?}", exit_code))
    } else {
        None
    };

    (status, exit_code, logs, error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ci_core::{Job, JobStep, StepAction};
    use std::collections::HashMap;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_execute_simple_command() {
        let (status, exit_code, logs, error) =
            execute_shell_step("echo 'hello world'", Path::new("/tmp"), &HashMap::new()).await;

        assert_eq!(status, JobStatus::Success);
        assert_eq!(exit_code, Some(0));
        assert!(String::from_utf8_lossy(&logs).contains("hello world"));
        assert!(error.is_none());
    }

    #[tokio::test]
    async fn test_execute_failing_command() {
        let (status, exit_code, _logs, error) =
            execute_shell_step("exit 1", Path::new("/tmp"), &HashMap::new()).await;

        assert_eq!(status, JobStatus::Failure);
        assert_eq!(exit_code, Some(1));
        assert!(error.is_some());
    }

    #[tokio::test]
    async fn test_execute_job() {
        let temp = tempdir().unwrap();
        let runner = RunnerIdentity::generate("test".to_string(), vec![]);

        let mut job = Job::new(
            "npub1test/repos/myproject".to_string(),
            "abc123".to_string(),
            ".github/workflows/ci.yml".to_string(),
            "build".to_string(),
        );

        job.steps = vec![
            JobStep {
                name: "Echo hello".to_string(),
                action: StepAction::Run("echo 'Hello, CI!'".to_string()),
                working_directory: None,
                env: HashMap::new(),
                continue_on_error: false,
                timeout_minutes: None,
            },
            JobStep {
                name: "Check pwd".to_string(),
                action: StepAction::Run("pwd".to_string()),
                working_directory: None,
                env: HashMap::new(),
                continue_on_error: false,
                timeout_minutes: None,
            },
        ];

        let result = execute_job(&job, &runner, temp.path()).await.unwrap();

        assert_eq!(result.status, JobStatus::Success);
        assert_eq!(result.steps.len(), 2);
        assert_eq!(result.steps[0].status, JobStatus::Success);
        assert_eq!(result.steps[1].status, JobStatus::Success);
        assert!(result.verify().unwrap());
    }

    #[tokio::test]
    async fn test_execute_job_with_failure() {
        let temp = tempdir().unwrap();
        let runner = RunnerIdentity::generate("test".to_string(), vec![]);

        let mut job = Job::new(
            "npub1test/repos/myproject".to_string(),
            "abc123".to_string(),
            ".github/workflows/ci.yml".to_string(),
            "build".to_string(),
        );

        job.steps = vec![
            JobStep {
                name: "This will fail".to_string(),
                action: StepAction::Run("exit 1".to_string()),
                working_directory: None,
                env: HashMap::new(),
                continue_on_error: false,
                timeout_minutes: None,
            },
            JobStep {
                name: "This should not run".to_string(),
                action: StepAction::Run("echo 'should not see this'".to_string()),
                working_directory: None,
                env: HashMap::new(),
                continue_on_error: false,
                timeout_minutes: None,
            },
        ];

        let result = execute_job(&job, &runner, temp.path()).await.unwrap();

        assert_eq!(result.status, JobStatus::Failure);
        assert_eq!(result.steps.len(), 1); // Second step should not run
        assert_eq!(result.steps[0].status, JobStatus::Failure);
    }
}
