//! Job executor - runs CI jobs and captures output.
//!
//! Supports both direct execution and container isolation via Docker/Podman.

use crate::action::{execute_builtin_action, ActionRef};
use ci_core::{ContainerConfig, Job, JobResult, JobStatus, RunnerIdentity, StepAction, StepResult};
use chrono::Utc;
use sha2::Digest;
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

/// Execute a CI job and return the result (convenience wrapper without container)
#[allow(dead_code)]
pub async fn execute_job(
    job: &Job,
    runner: &RunnerIdentity,
    work_dir: &Path,
) -> anyhow::Result<JobResult> {
    execute_job_with_container(job, runner, work_dir, &ContainerConfig::default()).await
}

/// Execute a CI job with optional container isolation
pub async fn execute_job_with_container(
    job: &Job,
    runner: &RunnerIdentity,
    work_dir: &Path,
    container_config: &ContainerConfig,
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
            StepAction::Run(cmd) => {
                if container_config.enabled {
                    execute_container_step(cmd, work_dir, &job.env, container_config).await
                } else {
                    execute_shell_step(cmd, work_dir, &job.env).await
                }
            }
            StepAction::Uses { action, with } => {
                execute_action_step(action, with, work_dir).await
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

/// Execute a shell command inside a container
async fn execute_container_step(
    cmd: &str,
    work_dir: &Path,
    env: &std::collections::HashMap<String, String>,
    config: &ContainerConfig,
) -> (JobStatus, Option<i32>, Vec<u8>, Option<String>) {
    let mut logs = Vec::new();

    // Log the command being run
    logs.extend_from_slice(format!("$ {} (in container: {})\n", cmd, config.default_image).as_bytes());

    // Build the container run command
    let mut command = Command::new(&config.runtime);
    command
        .arg("run")
        .arg("--rm");

    // Network isolation
    match config.network.as_str() {
        "none" => { command.arg("--network=none"); }
        "host" => { command.arg("--network=host"); }
        "bridge" => { /* default, no flag needed */ }
        _ => { command.arg("--network=none"); }
    }

    // Resource limits
    if let Some(ref mem) = config.memory_limit {
        command.arg(format!("--memory={}", mem));
    }
    if let Some(ref cpu) = config.cpu_limit {
        command.arg(format!("--cpus={}", cpu));
    }

    // Run as non-root if configured
    if config.rootless {
        command.arg("--user=1000:1000");
    }

    // Mount the work directory
    let work_dir_abs = work_dir.canonicalize().unwrap_or_else(|_| work_dir.to_path_buf());
    command.arg("-v").arg(format!("{}:/workspace:rw", work_dir_abs.display()));
    command.arg("-w").arg("/workspace");

    // Additional volume mounts
    for vol in &config.volumes {
        command.arg("-v").arg(vol);
    }

    // Environment variables
    for (key, value) in env {
        command.arg("-e").arg(format!("{}={}", key, value));
    }

    // Security hardening
    command.arg("--read-only");
    command.arg("--tmpfs=/tmp:rw,noexec,nosuid,size=512m");
    command.arg("--security-opt=no-new-privileges");
    command.arg("--cap-drop=ALL");

    // Image and command
    command.arg(&config.default_image);
    command.arg("sh").arg("-c").arg(cmd);

    command.stdout(Stdio::piped()).stderr(Stdio::piped());

    let child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            let error = format!("Failed to spawn container: {}. Is {} installed?", e, config.runtime);
            logs.extend_from_slice(error.as_bytes());
            return (JobStatus::Failure, None, logs, Some(error));
        }
    };

    let output = match child.wait_with_output().await {
        Ok(o) => o,
        Err(e) => {
            let error = format!("Failed to wait for container: {}", e);
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
        Some(format!("Container exit code: {:?}", exit_code))
    } else {
        None
    };

    (status, exit_code, logs, error)
}

/// Execute an action step (uses:)
async fn execute_action_step(
    action: &str,
    inputs: &std::collections::HashMap<String, String>,
    work_dir: &Path,
) -> (JobStatus, Option<i32>, Vec<u8>, Option<String>) {
    let mut logs = Vec::new();
    logs.extend_from_slice(format!("Running action: {}\n", action).as_bytes());

    // Parse the action reference
    let action_ref = match ActionRef::parse(action) {
        Some(r) => r,
        None => {
            let error = format!("Invalid action format: {}", action);
            logs.extend_from_slice(error.as_bytes());
            return (JobStatus::Failure, None, logs, Some(error));
        }
    };

    logs.extend_from_slice(
        format!(
            "  Owner: {}, Name: {}, Version: {}\n",
            action_ref.owner, action_ref.name, action_ref.version
        )
        .as_bytes(),
    );

    // Execute based on action type
    if action_ref.is_builtin() {
        // Built-in action
        match execute_builtin_action(&action_ref, inputs, work_dir).await {
            Ok(result) => {
                logs.extend_from_slice(result.logs.as_bytes());
                let status = if result.success {
                    JobStatus::Success
                } else {
                    JobStatus::Failure
                };
                let error = if result.success { None } else { Some(result.logs) };
                (status, Some(0), logs, error)
            }
            Err(e) => {
                logs.extend_from_slice(format!("Action error: {}\n", e).as_bytes());
                (JobStatus::Failure, None, logs, Some(e))
            }
        }
    } else {
        // Custom action from hashtree
        // For now, we don't support fetching custom actions yet
        let msg = format!(
            "Custom action '{}' not yet supported (only built-in actions are available)",
            action
        );
        logs.extend_from_slice(msg.as_bytes());
        (JobStatus::Skipped, None, logs, Some(msg))
    }
}

/// Check if a container runtime is available
pub async fn check_container_runtime(runtime: &str) -> bool {
    Command::new(runtime)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Detect available container runtime (prefers podman for rootless)
pub async fn detect_container_runtime() -> Option<String> {
    // Prefer podman for better rootless support
    if check_container_runtime("podman").await {
        return Some("podman".to_string());
    }
    if check_container_runtime("docker").await {
        return Some("docker".to_string());
    }
    None
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
