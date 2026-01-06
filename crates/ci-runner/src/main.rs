//! hashtree-ci runner CLI

mod executor;

use ci_core::{RunnerConfig, RunnerIdentity, RunnerIdentityConfig, RunnerLimits};
use ci_store::{CiStore, HashtreeStore, NpubPathStore};
use ci_workflow::{parse_workflow, workflow_to_jobs};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Init { name, tags } => {
            let identity = RunnerIdentity::generate(name.clone(), tags.clone());
            let config = RunnerConfig {
                runner: RunnerIdentityConfig {
                    name,
                    nsec: identity.nsec(),
                    tags,
                    limits: RunnerLimits::default(),
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
            println!("Starting daemon on {}", bind);
            println!("TODO: Implement daemon mode");
        }

        Commands::Results { repo, commit, limit } => {
            query_results(repo, commit, limit).await?;
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

    let repo_dir = PathBuf::from(repo_dir).canonicalize()?;
    println!("Running CI for: {}", repo_dir.display());

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

            // Execute
            let result = executor::execute_job(&job, &runner, &repo_dir).await?;

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
