//! Job executor - runs CI jobs and captures output.
//!
//! Supports both direct execution and container isolation via Docker/Podman.

use crate::action::{execute_builtin_action, ActionRef};
use ci_core::{ContainerConfig, Job, JobResult, JobStatus, RunnerIdentity, StepAction, StepResult};
use chrono::Utc;
use sha2::Digest;
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

/// Result of job execution including logs
pub struct ExecutionResult {
    pub result: JobResult,
    /// Step name -> log bytes
    pub step_logs: HashMap<String, Vec<u8>>,
}

struct ContainerSession {
    id: String,
    runtime: String,
    image: String,
    rootless: bool,
}

const SETUP_STEP_NAME: &str = "container setup";

fn record_step_result(
    result: &mut JobResult,
    step_logs: &mut HashMap<String, Vec<u8>>,
    all_logs: &mut Vec<u8>,
    name: String,
    status: JobStatus,
    exit_code: Option<i32>,
    duration_secs: u64,
    logs: Vec<u8>,
    error: Option<String>,
) {
    let logs_hash = hex::encode(sha2::Sha256::digest(&logs));
    all_logs.extend_from_slice(&logs);
    all_logs.push(b'\n');
    step_logs.insert(name.clone(), logs);
    result.steps.push(StepResult {
        name,
        status,
        exit_code,
        duration_secs,
        logs_hash,
        error,
    });
}

fn finalize_job_result(result: &mut JobResult, job_failed: bool, all_logs: &[u8]) {
    result.status = if job_failed {
        JobStatus::Failure
    } else {
        JobStatus::Success
    };
    result.finished_at = Utc::now();
    result.logs_hash = hex::encode(sha2::Sha256::digest(all_logs));
}

fn shell_escape(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }

    let mut escaped = String::from("'");
    for ch in value.chars() {
        if ch == '\'' {
            escaped.push_str("'\\''");
        } else {
            escaped.push(ch);
        }
    }
    escaped.push('\'');
    escaped
}

fn build_clone_script(git_url: &str, commit: &str) -> String {
    let git_url = shell_escape(git_url);
    let commit = shell_escape(commit);

    format!(
        "set -e\n\
if ! command -v git >/dev/null 2>&1; then\n\
  echo \"git not found in container image; install git or use an image with git preinstalled\" >&2\n\
  exit 127\n\
fi\n\
git clone --no-checkout {git_url} /workspace\n\
cd /workspace\n\
git checkout --detach {commit}\n"
    )
}

fn workspace_tmpfs_options(config: &ContainerConfig) -> String {
    let mut opts = vec!["rw".to_string(), "exec".to_string(), "nosuid".to_string()];
    if config.rootless {
        opts.push("uid=1000".to_string());
        opts.push("gid=1000".to_string());
    }
    let size = config.memory_limit.as_deref().unwrap_or("2g");
    opts.push(format!("size={}", size));
    opts.join(",")
}

/// Execute a CI job and return the result (convenience wrapper without container)
#[allow(dead_code)]
pub async fn execute_job(
    job: &Job,
    runner: &RunnerIdentity,
    work_dir: &Path,
) -> anyhow::Result<ExecutionResult> {
    execute_job_with_container(job, runner, work_dir, &ContainerConfig::default(), None).await
}

/// Execute a CI job with optional container isolation
pub async fn execute_job_with_container(
    job: &Job,
    runner: &RunnerIdentity,
    work_dir: &Path,
    container_config: &ContainerConfig,
    git_url: Option<&str>,
) -> anyhow::Result<ExecutionResult> {
    let mut result = JobResult::new(
        job.id,
        runner.npub(),
        job.repo_hash.clone(),
        job.commit.clone(),
        job.workflow.clone(),
        job.job_name.clone(),
    );

    let mut all_logs = Vec::new();
    let mut step_logs = HashMap::new();
    let mut job_failed = false;
    let mut container_session: Option<ContainerSession> = None;

    if container_config.enabled {
        let git_url = match git_url {
            Some(url) => url,
            None => {
                let error = "Container mode requires a git URL (set --owner-npub/--repo-path or configure origin)";
                let logs = format!("$ {} (in container: {})\n{}\n", SETUP_STEP_NAME, container_config.default_image, error)
                    .into_bytes();
                record_step_result(
                    &mut result,
                    &mut step_logs,
                    &mut all_logs,
                    SETUP_STEP_NAME.to_string(),
                    JobStatus::Failure,
                    None,
                    0,
                    logs,
                    Some(error.to_string()),
                );
                finalize_job_result(&mut result, true, &all_logs);
                return Ok(ExecutionResult { result, step_logs });
            }
        };

        let session = match start_container_session(container_config).await {
            Ok(session) => session,
            Err(e) => {
                let error = format!("Failed to start container: {}", e);
                let logs = format!("$ {} (in container: {})\n{}\n", SETUP_STEP_NAME, container_config.default_image, error)
                    .into_bytes();
                record_step_result(
                    &mut result,
                    &mut step_logs,
                    &mut all_logs,
                    SETUP_STEP_NAME.to_string(),
                    JobStatus::Failure,
                    None,
                    0,
                    logs,
                    Some(error),
                );
                finalize_job_result(&mut result, true, &all_logs);
                return Ok(ExecutionResult { result, step_logs });
            }
        };

        let setup_script = build_clone_script(git_url, &job.commit);
        let display_cmd = format!("container setup (clone {})", git_url);
        let (status, exit_code, logs, error) =
            exec_container_command(&session, &setup_script, &display_cmd, &job.env).await;
        if status == JobStatus::Failure {
            record_step_result(
                &mut result,
                &mut step_logs,
                &mut all_logs,
                SETUP_STEP_NAME.to_string(),
                status,
                exit_code,
                0,
                logs,
                error,
            );
            let _ = stop_container_session(&session).await;
            finalize_job_result(&mut result, true, &all_logs);
            return Ok(ExecutionResult { result, step_logs });
        }

        container_session = Some(session);
    }

    for step in &job.steps {
        let step_start = std::time::Instant::now();

        let (status, exit_code, logs, error) = match &step.action {
            StepAction::Run(cmd) => {
                if let Some(ref session) = container_session {
                    execute_container_step(cmd, session, &job.env).await
                } else {
                    execute_shell_step(cmd, work_dir, &job.env).await
                }
            }
            StepAction::Uses { action, with } => {
                execute_action_step(action, with, work_dir).await
            }
        };

        let duration = step_start.elapsed().as_secs();
        record_step_result(
            &mut result,
            &mut step_logs,
            &mut all_logs,
            step.name.clone(),
            status,
            exit_code,
            duration,
            logs,
            error,
        );

        // Check if we should continue
        if status == JobStatus::Failure && !step.continue_on_error {
            job_failed = true;
            break;
        }
    }

    if let Some(session) = container_session {
        let _ = stop_container_session(&session).await;
    }

    finalize_job_result(&mut result, job_failed, &all_logs);

    Ok(ExecutionResult { result, step_logs })
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

async fn start_container_session(config: &ContainerConfig) -> anyhow::Result<ContainerSession> {
    let mut command = Command::new(&config.runtime);
    command.arg("run").arg("--rm").arg("-d");

    // Network isolation
    match config.network.as_str() {
        "none" => {
            command.arg("--network=none");
        }
        "host" => {
            command.arg("--network=host");
        }
        "bridge" => { /* default, no flag needed */ }
        _ => {
            command.arg("--network=none");
        }
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

    // Workspace tmpfs (repo clone stays inside container)
    let workspace_tmpfs = workspace_tmpfs_options(config);
    command.arg(format!("--tmpfs=/workspace:{}", workspace_tmpfs));
    command.arg("-w").arg("/workspace");

    // Additional volume mounts
    for vol in &config.volumes {
        command.arg("-v").arg(vol);
    }

    // Security hardening
    command.arg("--read-only");
    command.arg("--tmpfs=/tmp:rw,noexec,nosuid,size=512m");
    command.arg("--security-opt=no-new-privileges");
    command.arg("--cap-drop=ALL");

    // Image and command
    command.arg(&config.default_image);
    command.arg("sh").arg("-c").arg("while true; do sleep 3600; done");

    command.stdout(Stdio::piped()).stderr(Stdio::piped());

    let output = command.output().await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!(
            "Failed to start container: {}",
            stderr.trim()
        ));
    }

    let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if id.is_empty() {
        return Err(anyhow::anyhow!("Container runtime returned empty container id"));
    }

    Ok(ContainerSession {
        id,
        runtime: config.runtime.clone(),
        image: config.default_image.clone(),
        rootless: config.rootless,
    })
}

async fn stop_container_session(session: &ContainerSession) -> anyhow::Result<()> {
    let status = Command::new(&session.runtime)
        .arg("stop")
        .arg(&session.id)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await?;

    if !status.success() {
        return Err(anyhow::anyhow!("Failed to stop container {}", session.id));
    }

    Ok(())
}

async fn exec_container_command(
    session: &ContainerSession,
    cmd: &str,
    display_cmd: &str,
    env: &std::collections::HashMap<String, String>,
) -> (JobStatus, Option<i32>, Vec<u8>, Option<String>) {
    let mut logs = Vec::new();

    // Log the command being run
    logs.extend_from_slice(format!("$ {} (in container: {})\n", display_cmd, session.image).as_bytes());

    let mut command = Command::new(&session.runtime);
    command.arg("exec");

    if session.rootless {
        command.arg("--user=1000:1000");
    }

    command.arg("-w").arg("/workspace");
    command.arg("-e").arg("GIT_TERMINAL_PROMPT=0");
    for (key, value) in env {
        command.arg("-e").arg(format!("{}={}", key, value));
    }

    command.arg(&session.id);
    command.arg("sh").arg("-c").arg(cmd);

    command.stdout(Stdio::piped()).stderr(Stdio::piped());

    let output = match command.output().await {
        Ok(o) => o,
        Err(e) => {
            let error = format!("Failed to exec in container: {}", e);
            logs.extend_from_slice(error.as_bytes());
            return (JobStatus::Failure, None, logs, Some(error));
        }
    };

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

/// Execute a shell command inside a container session
async fn execute_container_step(
    cmd: &str,
    session: &ContainerSession,
    env: &std::collections::HashMap<String, String>,
) -> (JobStatus, Option<i32>, Vec<u8>, Option<String>) {
    exec_container_command(session, cmd, cmd, env).await
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

        assert_eq!(result.result.status, JobStatus::Success);
        assert_eq!(result.result.steps.len(), 2);
        assert_eq!(result.result.steps[0].status, JobStatus::Success);
        assert_eq!(result.result.steps[1].status, JobStatus::Success);
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

        assert_eq!(result.result.status, JobStatus::Failure);
        assert_eq!(result.result.steps.len(), 1); // Second step should not run
        assert_eq!(result.result.steps[0].status, JobStatus::Failure);
    }

    #[test]
    fn test_build_clone_script_includes_git_clone_and_checkout() {
        let script = build_clone_script("htree://npub1example/hashtree", "abc123");
        assert!(script.contains("git clone --no-checkout 'htree://npub1example/hashtree' /workspace"));
        assert!(script.contains("git checkout --detach 'abc123'"));
    }

    #[test]
    fn test_shell_escape_handles_single_quote() {
        let escaped = shell_escape("we'ird");
        assert_eq!(escaped, "'we'\\''ird'");
    }
}
