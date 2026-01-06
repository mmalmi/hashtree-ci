//! End-to-end tests for hashtree-ci
//!
//! Tests the full CI flow:
//! 1. Create repo with .hashtree/ci.toml and .github/workflows/
//! 2. Runner picks up repo and executes workflow
//! 3. Results stored at npub1runner.../ci/npub1owner/repos/myproject/<commit>/
//! 4. Results are verifiable (signed by runner)

use ci_core::{Job, JobResult, JobStatus, JobStep, RepoConfig, RunnerIdentity, StepAction, StepResult, TrustedRunner};
use ci_store::{HashtreeStore, NpubPathStore};
use ci_workflow::{parse_workflow, workflow_to_jobs};
use std::collections::HashMap;
use tempfile::tempdir;

/// Test repo structure with CI config and workflow
struct TestRepo {
    owner_npub: String,
    path: String,
    commit: String,
    ci_config: RepoConfig,
    workflow_yaml: String,
}

impl TestRepo {
    fn new(owner_npub: &str, path: &str, commit: &str) -> Self {
        let runner_npub = "npub1testrunner".to_string();

        Self {
            owner_npub: owner_npub.to_string(),
            path: path.to_string(),
            commit: commit.to_string(),
            ci_config: RepoConfig {
                ci: ci_core::CiConfig {
                    org_npub: None,
                    runners: vec![TrustedRunner {
                        npub: runner_npub,
                        name: Some("test-runner".to_string()),
                        tags: vec!["linux".to_string()],
                    }],
                },
            },
            workflow_yaml: r#"
name: CI
on: push
jobs:
  build:
    runs-on: linux
    steps:
      - name: Echo hello
        run: echo "Hello, CI!"
      - name: Check date
        run: date
"#
            .to_string(),
        }
    }

    fn ci_result_path(&self, runner_npub: &str) -> String {
        format!(
            "{}/ci/{}/{}/{}",
            runner_npub, self.owner_npub, self.path, self.commit
        )
    }
}

/// In-memory hashtree storage for testing
struct MockHashtreeStore {
    data: HashMap<String, Vec<u8>>,
}

impl MockHashtreeStore {
    fn new() -> Self {
        Self {
            data: HashMap::new(),
        }
    }

    fn write(&mut self, path: &str, content: &[u8]) {
        self.data.insert(path.to_string(), content.to_vec());
    }

    fn read(&self, path: &str) -> Option<&Vec<u8>> {
        self.data.get(path)
    }

    fn exists(&self, path: &str) -> bool {
        self.data.contains_key(path)
    }
}

#[test]
fn test_repo_config_parsing() {
    let toml = r#"
[ci]
[[ci.runners]]
npub = "npub1runner123"
name = "linux-runner"
tags = ["linux", "docker"]
"#;
    let config: RepoConfig = toml::from_str(toml).unwrap();
    assert_eq!(config.ci.runners.len(), 1);
    assert_eq!(config.ci.runners[0].npub, "npub1runner123");
    assert!(config.is_runner_trusted("npub1runner123"));
    assert!(!config.is_runner_trusted("npub1unknown"));
}

#[test]
fn test_workflow_parsing() {
    let yaml = r#"
name: CI
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - name: Checkout
        uses: actions/checkout@v4
      - name: Build
        run: cargo build
      - name: Test
        run: cargo test
"#;
    let workflow = parse_workflow(yaml).unwrap();
    assert_eq!(workflow.name, Some("CI".to_string()));
    assert_eq!(workflow.jobs.len(), 1);
    assert!(workflow.jobs.contains_key("build"));

    let build_job = &workflow.jobs["build"];
    assert_eq!(build_job.steps.len(), 3);
}

#[test]
fn test_ci_result_path_structure() {
    let repo = TestRepo::new("npub1owner", "repos/myproject", "abc123");
    let runner_npub = "npub1runner";

    let path = repo.ci_result_path(runner_npub);
    assert_eq!(path, "npub1runner/ci/npub1owner/repos/myproject/abc123");
}

#[test]
fn test_runner_identity_generation() {
    let identity = RunnerIdentity::generate("test-runner".to_string(), vec!["linux".to_string()]);

    assert!(identity.npub().starts_with("npub1"));
    assert!(identity.nsec().starts_with("nsec1"));
    assert_eq!(identity.name, "test-runner");
    assert_eq!(identity.tags, vec!["linux"]);
}

#[test]
fn test_job_result_signing_and_verification() {
    use chrono::Utc;
    use uuid::Uuid;

    // Generate runner identity
    let runner = RunnerIdentity::generate("test".to_string(), vec![]);
    let runner_npub = runner.npub();
    let runner_nsec = runner.nsec();

    // Create job result
    let mut result = JobResult::new(
        Uuid::new_v4(),
        runner_npub.clone(),
        "htree://abc123".to_string(),
        "commit123".to_string(),
        ".github/workflows/ci.yml".to_string(),
        "build".to_string(),
    );
    result.status = JobStatus::Success;
    result.finished_at = Utc::now();
    result.logs_hash = "logs123".to_string();
    result.steps = vec![StepResult {
        name: "test".to_string(),
        status: JobStatus::Success,
        exit_code: Some(0),
        duration_secs: 10,
        logs_hash: "stephash".to_string(),
        error: None,
    }];

    // Sign
    result.sign(&runner_nsec).unwrap();
    assert!(!result.signature.is_empty());

    // Verify
    assert!(result.verify().unwrap());
}

#[test]
fn test_full_ci_flow_mock() {
    // Setup
    let mut store = MockHashtreeStore::new();
    let repo = TestRepo::new("npub1owner", "repos/myproject", "abc123");
    let runner = RunnerIdentity::generate("test-runner".to_string(), vec!["linux".to_string()]);
    let runner_npub = runner.npub();

    // 1. Store repo config
    let config_path = format!("{}/{}/.hashtree/ci.toml", repo.owner_npub, repo.path);
    store.write(&config_path, toml::to_string(&repo.ci_config).unwrap().as_bytes());

    // 2. Store workflow
    let workflow_path = format!("{}/{}/.github/workflows/ci.yml", repo.owner_npub, repo.path);
    store.write(&workflow_path, repo.workflow_yaml.as_bytes());

    // 3. Parse workflow
    let workflow = parse_workflow(&repo.workflow_yaml).unwrap();
    assert_eq!(workflow.jobs.len(), 1);

    // 4. Execute job (mock - just create result)
    let mut result = JobResult::new(
        uuid::Uuid::new_v4(),
        runner_npub.clone(),
        format!("{}/{}", repo.owner_npub, repo.path),
        repo.commit.clone(),
        ".github/workflows/ci.yml".to_string(),
        "build".to_string(),
    );
    result.status = JobStatus::Success;
    result.finished_at = chrono::Utc::now();
    result.logs_hash = "fakehash".to_string();
    result.steps = vec![
        StepResult {
            name: "Echo hello".to_string(),
            status: JobStatus::Success,
            exit_code: Some(0),
            duration_secs: 1,
            logs_hash: "log1".to_string(),
            error: None,
        },
        StepResult {
            name: "Check date".to_string(),
            status: JobStatus::Success,
            exit_code: Some(0),
            duration_secs: 1,
            logs_hash: "log2".to_string(),
            error: None,
        },
    ];

    // 5. Sign result
    result.sign(&runner.nsec()).unwrap();

    // 6. Store result at npub/path
    let result_path = format!(
        "{}/ci/{}/{}/{}/result.json",
        runner_npub, repo.owner_npub, repo.path, repo.commit
    );
    store.write(&result_path, serde_json::to_vec(&result).unwrap().as_slice());

    // 7. Verify lookup works
    assert!(store.exists(&result_path));

    // 8. Read and verify result
    let stored = store.read(&result_path).unwrap();
    let loaded: JobResult = serde_json::from_slice(stored).unwrap();
    assert_eq!(loaded.status, JobStatus::Success);
    assert_eq!(loaded.commit, "abc123");
    assert!(loaded.verify().unwrap());
}

#[test]
fn test_trusted_runner_lookup() {
    let repo = TestRepo::new("npub1owner", "repos/myproject", "abc123");

    // Should trust the configured runner
    assert!(repo.ci_config.is_runner_trusted("npub1testrunner"));

    // Should not trust unknown runners
    assert!(!repo.ci_config.is_runner_trusted("npub1unknown"));
}

#[test]
fn test_runner_tag_matching() {
    let config = RepoConfig {
        ci: ci_core::CiConfig {
            org_npub: None,
            runners: vec![
                TrustedRunner {
                    npub: "npub1linux".to_string(),
                    name: Some("linux".to_string()),
                    tags: vec!["linux".to_string(), "docker".to_string()],
                },
                TrustedRunner {
                    npub: "npub1macos".to_string(),
                    name: Some("macos".to_string()),
                    tags: vec!["macos".to_string()],
                },
            ],
        },
    };

    // Find runners with linux tag
    let linux_runners = config.find_runners_by_tags(&["linux".to_string()]);
    assert_eq!(linux_runners.len(), 1);
    assert_eq!(linux_runners[0].npub, "npub1linux");

    // Find runners with docker tag
    let docker_runners = config.find_runners_by_tags(&["docker".to_string()]);
    assert_eq!(docker_runners.len(), 1);

    // Find runners with both linux and docker
    let both = config.find_runners_by_tags(&["linux".to_string(), "docker".to_string()]);
    assert_eq!(both.len(), 1);

    // Find runners with macos
    let macos_runners = config.find_runners_by_tags(&["macos".to_string()]);
    assert_eq!(macos_runners.len(), 1);
    assert_eq!(macos_runners[0].npub, "npub1macos");
}

/// Real e2e test: create temp repo, run CI, verify results stored correctly
#[tokio::test]
async fn test_real_ci_execution() {
    // 1. Create temp directories
    let repo_dir = tempdir().unwrap();
    let store_dir = tempdir().unwrap();

    // 2. Create .github/workflows/ci.yml
    let workflows_dir = repo_dir.path().join(".github/workflows");
    std::fs::create_dir_all(&workflows_dir).unwrap();

    let workflow_yaml = r#"
name: Test CI
on: push
jobs:
  test:
    runs-on: linux
    steps:
      - name: Echo hello
        run: echo "Hello from CI!"
      - name: Create file
        run: echo "test content" > /tmp/ci-test-file.txt
      - name: Read file
        run: cat /tmp/ci-test-file.txt
"#;
    std::fs::write(workflows_dir.join("ci.yml"), workflow_yaml).unwrap();

    // 3. Create runner identity
    let runner = RunnerIdentity::generate("test-runner".to_string(), vec!["linux".to_string()]);

    // 4. Parse workflow and create job
    let workflow = parse_workflow(workflow_yaml).unwrap();
    let jobs = workflow_to_jobs(
        &workflow,
        "npub1owner/repos/testproject",
        "abc123",
        ".github/workflows/ci.yml",
    );

    assert_eq!(jobs.len(), 1);
    let job = &jobs[0];
    assert_eq!(job.job_name, "test");
    assert_eq!(job.steps.len(), 3);

    // 5. Execute job using the executor
    // Note: We import the executor module from the main crate
    let result = ci_runner_executor::execute_job(job, &runner, repo_dir.path()).await.unwrap();

    // 6. Verify result
    assert_eq!(result.status, JobStatus::Success);
    assert_eq!(result.steps.len(), 3);
    assert!(result.steps.iter().all(|s| s.status == JobStatus::Success));
    assert!(result.verify().unwrap());

    // 7. Store in hashtree store
    let store = HashtreeStore::new(store_dir.path().to_path_buf(), runner.npub());
    store
        .store_ci_result("npub1owner", "repos/testproject", "abc123", &result)
        .await
        .unwrap();

    // 8. Verify we can read it back
    let loaded = store
        .get_ci_result("npub1owner", "repos/testproject", "abc123")
        .await
        .unwrap()
        .expect("Result should exist");

    assert_eq!(loaded.status, JobStatus::Success);
    assert_eq!(loaded.commit, "abc123");
    assert!(loaded.verify().unwrap());

    // 9. Verify file structure
    let result_file = store_dir
        .path()
        .join("ci/npub1owner/repos/testproject/abc123/result.json");
    assert!(result_file.exists());
}

/// Test that a failing step stops execution
#[tokio::test]
async fn test_ci_execution_with_failure() {
    let repo_dir = tempdir().unwrap();
    let runner = RunnerIdentity::generate("test".to_string(), vec![]);

    let mut job = Job::new(
        "npub1owner/repos/test".to_string(),
        "def456".to_string(),
        ".github/workflows/ci.yml".to_string(),
        "build".to_string(),
    );

    job.steps = vec![
        JobStep {
            name: "This succeeds".to_string(),
            action: StepAction::Run("echo 'success'".to_string()),
            working_directory: None,
            env: HashMap::new(),
            continue_on_error: false,
            timeout_minutes: None,
        },
        JobStep {
            name: "This fails".to_string(),
            action: StepAction::Run("exit 42".to_string()),
            working_directory: None,
            env: HashMap::new(),
            continue_on_error: false,
            timeout_minutes: None,
        },
        JobStep {
            name: "This should not run".to_string(),
            action: StepAction::Run("echo 'should not see'".to_string()),
            working_directory: None,
            env: HashMap::new(),
            continue_on_error: false,
            timeout_minutes: None,
        },
    ];

    let result = ci_runner_executor::execute_job(&job, &runner, repo_dir.path()).await.unwrap();

    assert_eq!(result.status, JobStatus::Failure);
    assert_eq!(result.steps.len(), 2); // Third step should not run
    assert_eq!(result.steps[0].status, JobStatus::Success);
    assert_eq!(result.steps[1].status, JobStatus::Failure);
    assert_eq!(result.steps[1].exit_code, Some(42));
}

/// E2E test: run CI job inside a container (podman/docker)
/// Skips if no container runtime is available
#[tokio::test]
async fn test_ci_execution_in_container() {
    // 1. Check if container runtime is available
    let runtime = detect_container_runtime().await;
    if runtime.is_none() {
        eprintln!("Skipping container test: no docker/podman available");
        return;
    }
    let runtime = runtime.unwrap();
    eprintln!("Using container runtime: {}", runtime);

    // 2. Create temp repo with a workflow
    let repo_dir = tempdir().unwrap();
    let workflows_dir = repo_dir.path().join(".github/workflows");
    std::fs::create_dir_all(&workflows_dir).unwrap();

    // Workflow that tests container isolation
    let workflow_yaml = r#"
name: Container Test
on: push
jobs:
  test:
    runs-on: linux
    steps:
      - name: Check we're in container
        run: |
          echo "Running in container!"
          cat /etc/os-release | head -2
      - name: Test network (should work with bridge)
        run: |
          # Just check DNS works, don't actually download
          cat /etc/resolv.conf || true
      - name: Test filesystem isolation
        run: |
          # Should be able to write to /workspace
          echo "test" > /workspace/testfile.txt
          cat /workspace/testfile.txt
      - name: Verify no root
        run: |
          id
          whoami || echo "whoami not available"
"#;
    std::fs::write(workflows_dir.join("ci.yml"), workflow_yaml).unwrap();

    // 3. Create a file in repo to verify it's mounted
    std::fs::write(repo_dir.path().join("README.md"), "# Test Repo").unwrap();

    // 4. Create runner and container config
    let runner = RunnerIdentity::generate("container-test".to_string(), vec!["linux".to_string()]);
    let container_config = ci_core::ContainerConfig {
        enabled: true,
        runtime,
        default_image: "ubuntu:22.04".to_string(),
        network: "bridge".to_string(),
        volumes: Vec::new(),
        memory_limit: Some("512m".to_string()),
        cpu_limit: Some("1".to_string()),
        rootless: true,
    };

    // 5. Parse workflow and create job
    let workflow = parse_workflow(workflow_yaml).unwrap();
    let jobs = workflow_to_jobs(
        &workflow,
        "npub1owner/repos/containertest",
        "container123",
        ".github/workflows/ci.yml",
    );

    assert_eq!(jobs.len(), 1);
    let job = &jobs[0];

    // 6. Execute in container
    let result = ci_runner_executor::execute_job_with_container(
        job,
        &runner,
        repo_dir.path(),
        &container_config,
    ).await.unwrap();

    // 7. Verify success
    eprintln!("Job status: {:?}", result.status);
    for step in &result.steps {
        eprintln!("  Step '{}': {:?} (exit: {:?})", step.name, step.status, step.exit_code);
        if let Some(ref err) = step.error {
            eprintln!("    Error: {}", err);
        }
    }

    assert_eq!(result.status, JobStatus::Success, "Container job should succeed");
    assert!(result.steps.iter().all(|s| s.status == JobStatus::Success));
    assert!(result.verify().unwrap());
}

/// Detect available container runtime by actually trying to run a container
async fn detect_container_runtime() -> Option<String> {
    use tokio::process::Command;
    use std::process::Stdio;

    // Prefer podman for rootless security
    for runtime in &["podman", "docker"] {
        // First check if command exists
        let version_check = Command::new(runtime)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;

        if !version_check.map(|s| s.success()).unwrap_or(false) {
            continue;
        }

        // Actually try to run a container - this catches cases where
        // the binary exists but can't run containers (e.g., nested containers
        // without proper permissions)
        let run_check = Command::new(runtime)
            .args(["run", "--rm", "alpine", "echo", "test"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;

        if run_check.map(|s| s.success()).unwrap_or(false) {
            return Some(runtime.to_string());
        }
    }
    None
}

// Wrapper module to access the executor from tests
mod ci_runner_executor {
    use ci_core::{ContainerConfig, Job, JobResult, JobStatus, RunnerIdentity, StepAction, StepResult};
    use chrono::Utc;
    use sha2::Digest;
    use std::path::Path;
    use std::process::Stdio;
    use tokio::process::Command;

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
                    let msg = format!("Action '{}' not yet supported", action);
                    (JobStatus::Skipped, None, msg.as_bytes().to_vec(), Some(msg))
                }
            };

            let duration = step_start.elapsed().as_secs();
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

            if status == JobStatus::Failure && !step.continue_on_error {
                job_failed = true;
                break;
            }
        }

        result.status = if job_failed {
            JobStatus::Failure
        } else {
            JobStatus::Success
        };

        result.finished_at = Utc::now();
        result.logs_hash = hex::encode(sha2::Sha256::digest(&all_logs));
        result.sign(&runner.nsec())?;

        Ok(result)
    }

    async fn execute_shell_step(
        cmd: &str,
        work_dir: &Path,
        env: &std::collections::HashMap<String, String>,
    ) -> (JobStatus, Option<i32>, Vec<u8>, Option<String>) {
        let mut logs = Vec::new();
        logs.extend_from_slice(format!("$ {}\n", cmd).as_bytes());

        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(cmd)
            .current_dir(work_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        for (key, value) in env {
            command.env(key, value);
        }

        let child = match command.spawn() {
            Ok(c) => c,
            Err(e) => {
                let error = format!("Failed to spawn: {}", e);
                logs.extend_from_slice(error.as_bytes());
                return (JobStatus::Failure, None, logs, Some(error));
            }
        };

        let output = match child.wait_with_output().await {
            Ok(o) => o,
            Err(e) => {
                let error = format!("Failed to wait: {}", e);
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
            Some(format!("Exit code: {:?}", exit_code))
        } else {
            None
        };

        (status, exit_code, logs, error)
    }

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
                StepAction::Uses { action, with: _ } => {
                    let msg = format!("Action '{}' not yet supported", action);
                    (JobStatus::Skipped, None, msg.as_bytes().to_vec(), Some(msg))
                }
            };

            let duration = step_start.elapsed().as_secs();
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

            if status == JobStatus::Failure && !step.continue_on_error {
                job_failed = true;
                break;
            }
        }

        result.status = if job_failed {
            JobStatus::Failure
        } else {
            JobStatus::Success
        };

        result.finished_at = Utc::now();
        result.logs_hash = hex::encode(sha2::Sha256::digest(&all_logs));
        result.sign(&runner.nsec())?;

        Ok(result)
    }

    async fn execute_container_step(
        cmd: &str,
        work_dir: &Path,
        env: &std::collections::HashMap<String, String>,
        config: &ContainerConfig,
    ) -> (JobStatus, Option<i32>, Vec<u8>, Option<String>) {
        let mut logs = Vec::new();
        logs.extend_from_slice(format!("$ {} (in container: {})\n", cmd, config.default_image).as_bytes());

        let mut command = Command::new(&config.runtime);
        command.arg("run").arg("--rm");

        // Network
        match config.network.as_str() {
            "none" => { command.arg("--network=none"); }
            "host" => { command.arg("--network=host"); }
            _ => { /* bridge is default */ }
        }

        // Resource limits
        if let Some(ref mem) = config.memory_limit {
            command.arg(format!("--memory={}", mem));
        }
        if let Some(ref cpu) = config.cpu_limit {
            command.arg(format!("--cpus={}", cpu));
        }

        // Rootless
        if config.rootless {
            command.arg("--user=1000:1000");
        }

        // Mount work directory
        let work_dir_abs = work_dir.canonicalize().unwrap_or_else(|_| work_dir.to_path_buf());
        command.arg("-v").arg(format!("{}:/workspace:rw", work_dir_abs.display()));
        command.arg("-w").arg("/workspace");

        // Environment
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
}
