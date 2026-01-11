//! hashtree-ci runner CLI

mod action;
mod executor;
mod watcher;

use ci_core::{ContainerConfig, RunnerConfig, RunnerIdentity, RunnerIdentityConfig, RunnerLimits, WatchedRepo};
use ci_store::{CiStore, HashtreeStore, NpubPathStore};
use ci_workflow::{parse_workflow, workflow_to_jobs};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use watcher::RepoWatcher;

#[derive(Parser)]
#[command(name = "htci")]
#[command(about = "Hashtree CI runner")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a new runner identity
    Init {
        /// Runner name
        #[arg(short, long)]
        name: String,

        /// Runner tags (comma-separated)
        #[arg(short, long, value_delimiter = ',')]
        tags: Vec<String>,

        /// Enable container isolation (docker/podman)
        #[arg(long)]
        container: bool,

        /// Container image (e.g., "ubuntu:22.04", "rust:1.75")
        #[arg(long, default_value = "ubuntu:22.04")]
        image: String,
    },

    /// Show runner identity (npub)
    Whoami,

    /// Run CI for a repository
    Run {
        /// Repository path
        #[arg(short, long, default_value = ".")]
        repo: String,

        /// Repository owner's npub (for result storage path)
        #[arg(long)]
        owner_npub: Option<String>,

        /// Repository path under owner (e.g., "repos/myproject")
        #[arg(long)]
        repo_path: Option<String>,

        /// Specific workflow to run
        #[arg(short, long)]
        workflow: Option<String>,

        /// Git commit SHA (defaults to HEAD)
        #[arg(short, long)]
        commit: Option<String>,
    },

    /// Start daemon mode (watch for jobs)
    Daemon {
        /// Bind address for status API
        #[arg(short, long, default_value = "127.0.0.1:8080")]
        bind: String,
    },

    /// Query CI results
    Results {
        /// Repository npub/path (e.g., "npub1.../repos/myproject")
        #[arg(short, long)]
        repo: Option<String>,

        /// Specific commit
        #[arg(short, long)]
        commit: Option<String>,

        /// Limit
        #[arg(short, long, default_value = "10")]
        limit: usize,
    },

    /// Add a repository to watch for updates
    Watch {
        /// Owner's npub (e.g., "npub1...")
        #[arg(short, long)]
        owner: String,

        /// Repository/tree path (e.g., "hashtree-ts")
        #[arg(short, long)]
        path: String,

        /// Local directory to clone/sync the repo
        #[arg(short, long)]
        local: Option<String>,
    },

    /// List watched repositories
    ListWatch,

    /// Remove a watched repository
    Unwatch {
        /// Owner's npub
        #[arg(short, long)]
        owner: String,

        /// Repository/tree path
        #[arg(short, long)]
        path: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Init { name, tags, container, image } => {
            // Detect container runtime if enabled
            let container_config = if container {
                let runtime = executor::detect_container_runtime().await
                    .ok_or_else(|| anyhow::anyhow!("No container runtime found. Install docker or podman."))?;
                println!("Detected container runtime: {}", runtime);
                ContainerConfig {
                    enabled: true,
                    runtime,
                    default_image: image,
                    network: "bridge".to_string(),  // Internet access for package downloads
                    volumes: Vec::new(),
                    memory_limit: Some("2g".to_string()),
                    cpu_limit: Some("2".to_string()),
                    rootless: true,
                }
            } else {
                ContainerConfig::default()
            };

            let identity = RunnerIdentity::generate(name.clone(), tags.clone());
            let config = RunnerConfig {
                runner: RunnerIdentityConfig {
                    name,
                    nsec: identity.nsec(),
                    tags,
                    limits: RunnerLimits::default(),
                    container: container_config.clone(),
                    allowed_repos: Vec::new(),
                    watched_repos: Vec::new(),
                },
            };

            let config_dir = dirs::config_dir()
                .ok_or_else(|| anyhow::anyhow!("Could not find config directory"))?;
            let ci_dir = config_dir.join("hashtree-ci");
            std::fs::create_dir_all(&ci_dir)?;

            let config_path = ci_dir.join("runner.toml");
            let toml_str = toml::to_string_pretty(&config)?;
            std::fs::write(&config_path, toml_str)?;

            println!("Runner initialized!");
            println!("  npub: {}", identity.npub());
            println!("  config: {}", config_path.display());
            if container_config.enabled {
                println!("  container: {} ({})", container_config.runtime, container_config.default_image);
            }
            println!("\nAdd this npub to your repo's .hashtree/ci.toml:");
            println!("[[ci.runners]]");
            println!("npub = \"{}\"", identity.npub());
            println!("name = \"{}\"", config.runner.name);
        }

        Commands::Whoami => {
            let config = RunnerConfig::load()?;
            let npub = get_npub(&config)?;
            println!("npub: {}", npub);
            println!("name: {}", config.runner.name);
            println!("tags: {:?}", config.runner.tags);
            if config.runner.container.enabled {
                println!("container: {} ({})", config.runner.container.runtime, config.runner.container.default_image);
                println!("  network: {}", config.runner.container.network);
                if let Some(ref mem) = config.runner.container.memory_limit {
                    println!("  memory: {}", mem);
                }
            } else {
                println!("container: disabled (WARNING: commands run directly on host)");
            }
            if !config.runner.allowed_repos.is_empty() {
                println!("allowed_repos:");
                for repo in &config.runner.allowed_repos {
                    println!("  - {}:{}", repo.owner_npub, repo.repo_pattern);
                }
            }
        }

        Commands::Run {
            repo,
            owner_npub,
            repo_path,
            workflow,
            commit,
        } => {
            run_ci(&repo, owner_npub, repo_path, workflow, commit).await?;
        }

        Commands::Daemon { bind } => {
            run_daemon(&bind).await?;
        }

        Commands::Results { repo, commit, limit } => {
            query_results(repo, commit, limit).await?;
        }

        Commands::Watch { owner, path, local } => {
            add_watched_repo(&owner, &path, local)?;
        }

        Commands::ListWatch => {
            list_watched_repos()?;
        }

        Commands::Unwatch { owner, path } => {
            remove_watched_repo(&owner, &path)?;
        }
    }

    Ok(())
}

async fn run_ci(
    repo_dir: &str,
    owner_npub: Option<String>,
    repo_path: Option<String>,
    workflow_filter: Option<String>,
    commit: Option<String>,
) -> anyhow::Result<()> {
    // Load runner config
    let config = RunnerConfig::load()?;
    let runner = RunnerIdentity::from_nsec(
        &config.runner.nsec,
        config.runner.name.clone(),
        config.runner.tags.clone(),
    )?;

    let container_config = &config.runner.container;

    let repo_dir = PathBuf::from(repo_dir).canonicalize()?;
    println!("Running CI for: {}", repo_dir.display());
    if container_config.enabled {
        println!("  Container: {} ({})", container_config.runtime, container_config.default_image);
    } else {
        println!("  Container: disabled (running on host)");
    }

    // Determine repo identity
    let owner = owner_npub.unwrap_or_else(|| "local".to_string());
    let path = repo_path.unwrap_or_else(|| {
        repo_dir
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "repo".to_string())
    });

    // Get commit SHA
    let commit_sha = if let Some(c) = commit {
        c
    } else {
        get_git_head(&repo_dir)?
    };

    println!("  Owner: {}", owner);
    println!("  Path: {}", path);
    println!("  Commit: {}", commit_sha);

    // Find workflows
    let workflows_dir = repo_dir.join(".github/workflows");
    if !workflows_dir.exists() {
        anyhow::bail!("No .github/workflows directory found");
    }

    let mut workflow_files = Vec::new();
    for entry in std::fs::read_dir(&workflows_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().map(|e| e == "yml" || e == "yaml").unwrap_or(false) {
            if let Some(filter) = &workflow_filter {
                if !path.to_string_lossy().contains(filter) {
                    continue;
                }
            }
            workflow_files.push(path);
        }
    }

    if workflow_files.is_empty() {
        anyhow::bail!("No workflow files found");
    }

    // Setup store
    let data_dir = dirs::data_local_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not find data directory"))?
        .join("hashtree-ci");
    std::fs::create_dir_all(&data_dir)?;

    let store = HashtreeStore::new(data_dir, runner.npub());

    // Execute each workflow
    for workflow_file in workflow_files {
        println!("\nWorkflow: {}", workflow_file.display());

        let yaml = std::fs::read_to_string(&workflow_file)?;
        let workflow = parse_workflow(&yaml)?;

        let workflow_path = workflow_file
            .strip_prefix(&repo_dir)
            .unwrap_or(&workflow_file)
            .to_string_lossy()
            .to_string();

        let jobs = workflow_to_jobs(
            &workflow,
            &format!("{}/{}", owner, path),
            &commit_sha,
            &workflow_path,
        );

        for job in jobs {
            println!("  Job: {}", job.job_name);

            // Check if already run
            if store.has_ci_result(&owner, &path, &commit_sha).await? {
                println!("    Already run, skipping");
                continue;
            }

            // Execute with container isolation if configured
            let result = executor::execute_job_with_container(&job, &runner, &repo_dir, container_config).await?;

            println!("    Status: {:?}", result.status);
            for step in &result.steps {
                let icon = if step.status == ci_core::JobStatus::Success {
                    "✓"
                } else {
                    "✗"
                };
                println!("    {} {} ({}s)", icon, step.name, step.duration_secs);
            }

            // Store result
            store
                .store_ci_result(&owner, &path, &commit_sha, &result)
                .await?;

            // Store logs
            for step in &result.steps {
                // In a real impl, we'd store actual logs
                store
                    .store_step_logs(&owner, &path, &commit_sha, &step.name, b"[logs]")
                    .await?;
            }

            println!(
                "    Result stored at: {}/ci/{}/{}/{}",
                runner.npub(),
                owner,
                path,
                commit_sha
            );
        }
    }

    println!("\nDone!");
    Ok(())
}

async fn query_results(
    repo: Option<String>,
    commit: Option<String>,
    limit: usize,
) -> anyhow::Result<()> {
    let config = RunnerConfig::load()?;
    let npub = get_npub(&config)?;

    let data_dir = dirs::data_local_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not find data directory"))?
        .join("hashtree-ci");

    let store = HashtreeStore::new(data_dir, npub);

    if let Some(repo_id) = repo {
        if let Some(commit) = commit {
            // Query specific commit
            let results = store.query_by_commit(&repo_id, &commit).await?;
            for r in results {
                println!(
                    "{} {} {} {:?}",
                    r.commit, r.job_name, r.finished_at, r.status
                );
            }
        } else {
            // Query all commits for repo
            let results = store.query_by_repo(&repo_id, limit).await?;
            for r in results {
                println!(
                    "{} {} {} {:?}",
                    r.commit, r.job_name, r.finished_at, r.status
                );
            }
        }
    } else {
        println!("Specify --repo to query results");
    }

    Ok(())
}

/// Run the CI daemon - watches repos via Nostr and executes CI on updates
async fn run_daemon(bind: &str) -> anyhow::Result<()> {
    // Load runner config
    let config = RunnerConfig::load()?;
    let runner = RunnerIdentity::from_nsec(
        &config.runner.nsec,
        config.runner.name.clone(),
        config.runner.tags.clone(),
    )?;
    let container_config = config.runner.container.clone();

    println!("=== hashtree-ci daemon ===");
    println!("Runner: {} ({})", config.runner.name, runner.npub());
    println!("Tags: {:?}", config.runner.tags);
    if container_config.enabled {
        println!("Container: {} ({})", container_config.runtime, container_config.default_image);
    } else {
        println!("Container: disabled (WARNING: running on host)");
    }
    println!("Status API: http://{}", bind);
    println!();

    // Check if we have watched repos configured
    if config.runner.watched_repos.is_empty() {
        println!("No repos configured to watch.");
        println!();
        println!("Add repos with: htci watch -o <npub> -p <path> [-l <local_path>]");
        println!();
        println!("Example:");
        println!("  htci watch -o npub1abc... -p hashtree-ts -l /path/to/local/clone");
        return Ok(());
    }

    println!("Watching {} repos via Nostr:", config.runner.watched_repos.len());
    for repo in &config.runner.watched_repos {
        print!("  - {}/{}", repo.owner_npub, repo.path);
        if let Some(ref local) = repo.local_path {
            print!(" -> {}", local);
        }
        println!();
    }
    println!();

    // Setup store
    let data_dir = dirs::data_local_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not find data directory"))?
        .join("hashtree-ci");
    std::fs::create_dir_all(&data_dir)?;
    let store = HashtreeStore::new(data_dir, runner.npub());

    // Create channel for updates
    let (tx, mut rx) = tokio::sync::mpsc::channel::<watcher::RepoUpdate>(100);

    // Start watcher
    println!("Connecting to Nostr relays...");
    let watcher = RepoWatcher::new(&config, tx).await?;
    println!("Connected! Listening for tree root updates...");
    println!();
    println!("Press Ctrl+C to stop");
    println!();

    // Spawn watcher task
    let watch_handle = tokio::spawn(async move {
        if let Err(e) = watcher.watch().await {
            tracing::error!("Watcher error: {}", e);
        }
    });

    // Handle updates
    while let Some(update) = rx.recv().await {
        let short_hash = if update.merkle_root.len() >= 16 {
            &update.merkle_root[..16]
        } else {
            &update.merkle_root
        };
        println!(
            "[UPDATE] {}/{} -> {}...",
            update.owner_npub,
            update.path,
            short_hash
        );

        // Find local path for this repo
        let watched = config
            .runner
            .watched_repos
            .iter()
            .find(|r| r.owner_npub == update.owner_npub && r.path == update.path);

        let local_path = watched.and_then(|r| r.local_path.clone());

        if let Some(ref local) = local_path {
            let repo_dir = PathBuf::from(local);
            if !repo_dir.exists() {
                println!("  Local path does not exist: {}", local);
                continue;
            }

            // TODO: Fetch/sync the tree using merkle_root
            // For now, use the local git HEAD as the commit reference
            let commit_sha = match get_git_head(&repo_dir) {
                Ok(h) => h,
                Err(e) => {
                    println!("  Error getting git HEAD: {}", e);
                    continue;
                }
            };

            println!("  Local commit: {}", &commit_sha[..8.min(commit_sha.len())]);

            // Find and run workflows
            let workflows_dir = repo_dir.join(".github/workflows");
            if !workflows_dir.exists() {
                println!("  No .github/workflows directory");
                continue;
            }

            let entries = match std::fs::read_dir(&workflows_dir) {
                Ok(e) => e,
                Err(e) => {
                    println!("  Error reading workflows: {}", e);
                    continue;
                }
            };

            for entry in entries.flatten() {
                let path = entry.path();
                if !path.extension().map(|e| e == "yml" || e == "yaml").unwrap_or(false) {
                    continue;
                }

                println!("  Workflow: {}", path.file_name().unwrap_or_default().to_string_lossy());
                let yaml = match std::fs::read_to_string(&path) {
                    Ok(y) => y,
                    Err(e) => {
                        println!("    Error reading: {}", e);
                        continue;
                    }
                };

                let workflow = match ci_workflow::parse_workflow(&yaml) {
                    Ok(w) => w,
                    Err(e) => {
                        println!("    Error parsing: {}", e);
                        continue;
                    }
                };

                let workflow_path = path
                    .strip_prefix(&repo_dir)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string();

                let jobs = ci_workflow::workflow_to_jobs(
                    &workflow,
                    &format!("{}/{}", update.owner_npub, update.path),
                    &commit_sha,
                    &workflow_path,
                );

                for job in jobs {
                    println!("    Job: {}", job.job_name);

                    // Check if already run
                    if store.has_ci_result(&update.owner_npub, &update.path, &commit_sha).await.unwrap_or(false) {
                        println!("      Skipped (already run)");
                        continue;
                    }

                    // Execute
                    let result = match executor::execute_job_with_container(
                        &job,
                        &runner,
                        &repo_dir,
                        &container_config,
                    )
                    .await
                    {
                        Ok(r) => r,
                        Err(e) => {
                            println!("      Error: {}", e);
                            continue;
                        }
                    };

                    let status_icon = if result.status == ci_core::JobStatus::Success {
                        "✓"
                    } else {
                        "✗"
                    };
                    println!("      {} {:?}", status_icon, result.status);

                    // Store result
                    if let Err(e) = store
                        .store_ci_result(&update.owner_npub, &update.path, &commit_sha, &result)
                        .await
                    {
                        println!("      Error storing result: {}", e);
                    }
                }
            }
        } else {
            println!("  No local path configured - run:");
            println!("    htci watch -o {} -p {} -l /path/to/clone", update.owner_npub, update.path);
        }
    }

    watch_handle.await?;
    Ok(())
}

fn get_npub(config: &RunnerConfig) -> anyhow::Result<String> {
    use nostr::prelude::*;
    let keys = Keys::parse(&config.runner.nsec)?;
    Ok(keys.public_key().to_bech32()?)
}

fn get_git_head(repo_dir: &PathBuf) -> anyhow::Result<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_dir)
        .output()?;

    if !output.status.success() {
        anyhow::bail!("Failed to get git HEAD");
    }

    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

/// Add a repository to watch for updates
fn add_watched_repo(owner: &str, path: &str, local: Option<String>) -> anyhow::Result<()> {
    // Validate npub format
    use nostr::prelude::*;
    let _ = PublicKey::from_bech32(owner)
        .map_err(|_| anyhow::anyhow!("Invalid npub format: {}", owner))?;

    let mut config = RunnerConfig::load()?;

    // Check if already watching
    if config.runner.watched_repos.iter().any(|r| r.owner_npub == owner && r.path == path) {
        println!("Already watching {}/{}", owner, path);
        return Ok(());
    }

    config.runner.watched_repos.push(WatchedRepo {
        owner_npub: owner.to_string(),
        path: path.to_string(),
        local_path: local,
    });

    save_config(&config)?;

    println!("Now watching: {}/{}", owner, path);
    println!("Run 'htci daemon' to start watching for updates");
    Ok(())
}

/// List all watched repositories
fn list_watched_repos() -> anyhow::Result<()> {
    let config = RunnerConfig::load()?;

    if config.runner.watched_repos.is_empty() {
        println!("No repos being watched");
        println!("Use 'htci watch -o <npub> -p <path>' to add one");
        return Ok(());
    }

    println!("Watched repositories:");
    for repo in &config.runner.watched_repos {
        print!("  {}/{}", repo.owner_npub, repo.path);
        if let Some(ref local) = repo.local_path {
            print!(" -> {}", local);
        }
        println!();
    }
    Ok(())
}

/// Remove a watched repository
fn remove_watched_repo(owner: &str, path: &str) -> anyhow::Result<()> {
    let mut config = RunnerConfig::load()?;

    let initial_len = config.runner.watched_repos.len();
    config.runner.watched_repos.retain(|r| !(r.owner_npub == owner && r.path == path));

    if config.runner.watched_repos.len() == initial_len {
        println!("Not watching {}/{}", owner, path);
        return Ok(());
    }

    save_config(&config)?;
    println!("Stopped watching: {}/{}", owner, path);
    Ok(())
}

/// Save config to disk
fn save_config(config: &RunnerConfig) -> anyhow::Result<()> {
    let config_dir = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not find config directory"))?;
    let config_path = config_dir.join("hashtree-ci/runner.toml");
    let toml_str = toml::to_string_pretty(config)?;
    std::fs::write(&config_path, toml_str)?;
    Ok(())
}
