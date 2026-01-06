//! hashtree-ci runner CLI

use clap::{Parser, Subcommand};
use ci_core::{RunnerConfig, RunnerIdentity, RunnerIdentityConfig, RunnerLimits};

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

        /// Specific workflow to run
        #[arg(short, long)]
        workflow: Option<String>,
    },

    /// Start daemon mode (watch for jobs)
    Daemon {
        /// Bind address for status API
        #[arg(short, long, default_value = "127.0.0.1:8080")]
        bind: String,
    },

    /// Query CI results
    Results {
        /// Repository hash
        #[arg(short, long)]
        repo: Option<String>,

        /// Runner npub
        #[arg(short = 'n', long)]
        runner: Option<String>,

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

        Commands::Run { repo, workflow } => {
            println!("Running CI for {} (workflow: {:?})", repo, workflow);
            println!("TODO: Implement job execution");
        }

        Commands::Daemon { bind } => {
            println!("Starting daemon on {}", bind);
            println!("TODO: Implement daemon mode");
        }

        Commands::Results { repo, runner, limit } => {
            println!("Querying results (repo: {:?}, runner: {:?}, limit: {})", repo, runner, limit);
            println!("TODO: Implement result queries");
        }
    }

    Ok(())
}

fn get_npub(config: &RunnerConfig) -> anyhow::Result<String> {
    use nostr::prelude::*;
    let keys = Keys::parse(&config.runner.nsec)?;
    Ok(keys.public_key().to_bech32()?)
}
