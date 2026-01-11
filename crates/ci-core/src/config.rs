//! CI configuration schema for repos and runners.
//!
//! # Multi-Runner Design
//!
//! Each CI runner has its own Nostr keypair (npub/nsec). This enables:
//! - Independent runner identities
//! - Cryptographic proof of who ran a job
//! - Trust delegation via repo config
//!
//! ## Repo Config (`.hashtree/ci.toml`)
//!
//! Repos define which runners they trust:
//!
//! ```toml
//! [ci]
//! # Optional: organization npub that can authorize runners dynamically
//! org_npub = "npub1org..."
//!
//! # Trusted runners (jobs only accepted from these npubs)
//! [[ci.runners]]
//! npub = "npub1runner1..."
//! name = "linux-x64"
//! tags = ["linux", "x64", "docker"]
//!
//! [[ci.runners]]
//! npub = "npub1runner2..."
//! name = "macos-arm64"
//! tags = ["macos", "arm64"]
//! ```
//!
//! ## Runner Config (`~/.config/hashtree-ci/runner.toml`)
//!
//! Each runner has its own identity:
//!
//! ```toml
//! [runner]
//! name = "my-linux-runner"
//! nsec = "nsec1..."  # Private key - never share!
//! tags = ["linux", "x64", "docker"]
//!
//! [runner.limits]
//! max_concurrent_jobs = 4
//! job_timeout_secs = 3600
//! ```

use serde::{Deserialize, Serialize};

/// Repository CI configuration (`.hashtree/ci.toml`)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RepoConfig {
    pub ci: CiConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CiConfig {
    /// Optional organization npub that can dynamically authorize runners.
    /// If set, runners can be added via signed authorization messages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_npub: Option<String>,

    /// List of trusted runners.
    /// Jobs are only accepted if signed by one of these runner npubs.
    #[serde(default)]
    pub runners: Vec<TrustedRunner>,
}

/// A trusted CI runner definition in repo config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedRunner {
    /// Runner's Nostr public key (npub1...)
    pub npub: String,

    /// Human-readable name for this runner
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Tags for job routing (e.g., ["linux", "docker", "gpu"])
    /// Jobs with `runs-on` matching these tags will be routed to this runner.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Runner's own configuration (`~/.config/hashtree-ci/runner.toml`)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerConfig {
    pub runner: RunnerIdentityConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerIdentityConfig {
    /// Human-readable name
    pub name: String,

    /// Runner's Nostr private key (nsec1...)
    /// Used to sign job results.
    pub nsec: String,

    /// Tags describing this runner's capabilities
    #[serde(default)]
    pub tags: Vec<String>,

    /// Resource limits
    #[serde(default)]
    pub limits: RunnerLimits,

    /// Container configuration for secure execution
    #[serde(default)]
    pub container: ContainerConfig,

    /// Allowed repositories (if empty, only explicit CLI runs are allowed)
    #[serde(default)]
    pub allowed_repos: Vec<AllowedRepo>,

    /// Repositories to watch for updates (daemon mode)
    #[serde(default)]
    pub watched_repos: Vec<WatchedRepo>,
}

/// A repository to watch for updates in daemon mode
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchedRepo {
    /// Owner's Nostr public key (npub1...)
    pub owner_npub: String,

    /// Repository path (e.g., "hashtree-ts", "repos/myproject")
    pub path: String,

    /// Optional: local directory to clone/sync the repo
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_path: Option<String>,
}

/// Container runtime configuration for secure job execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerConfig {
    /// Enable container isolation
    #[serde(default)]
    pub enabled: bool,

    /// Container runtime: "docker" or "podman"
    #[serde(default = "default_runtime")]
    pub runtime: String,

    /// Default container image (e.g., "ubuntu:22.04", "rust:1.75")
    #[serde(default = "default_image")]
    pub default_image: String,

    /// Network mode: "none", "host", or "bridge"
    #[serde(default = "default_network")]
    pub network: String,

    /// Additional volume mounts (host:container format)
    #[serde(default)]
    pub volumes: Vec<String>,

    /// Memory limit (e.g., "2g", "512m")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_limit: Option<String>,

    /// CPU limit (e.g., "2" for 2 cores)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_limit: Option<String>,

    /// Run as non-root user inside container
    #[serde(default = "default_true")]
    pub rootless: bool,
}

impl Default for ContainerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            runtime: default_runtime(),
            default_image: default_image(),
            network: default_network(),
            volumes: Vec::new(),
            memory_limit: None,
            cpu_limit: None,
            rootless: true,
        }
    }
}

fn default_runtime() -> String {
    "docker".to_string()
}

fn default_image() -> String {
    "ubuntu:22.04".to_string()
}

fn default_network() -> String {
    "bridge".to_string()  // Isolated but has internet access for package downloads
}

fn default_true() -> bool {
    true
}

/// Allowed repository pattern for automatic job acceptance
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllowedRepo {
    /// Owner npub pattern (exact match or "*" for any)
    pub owner_npub: String,

    /// Repository path pattern (glob-like, e.g., "repos/*" or "repos/myproject")
    #[serde(default)]
    pub repo_pattern: String,
}

impl AllowedRepo {
    /// Check if this pattern matches a given repo
    pub fn matches(&self, owner_npub: &str, repo_path: &str) -> bool {
        // Check owner match
        let owner_matches = self.owner_npub == "*" || self.owner_npub == owner_npub;
        if !owner_matches {
            return false;
        }

        // Check repo pattern match
        if self.repo_pattern.is_empty() || self.repo_pattern == "*" {
            return true;
        }

        // Simple glob matching: "repos/*" matches "repos/anything"
        if self.repo_pattern.ends_with("/*") {
            let prefix = &self.repo_pattern[..self.repo_pattern.len() - 2];
            repo_path.starts_with(prefix)
        } else {
            self.repo_pattern == repo_path
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerLimits {
    /// Maximum concurrent jobs
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent_jobs: u32,

    /// Job timeout in seconds
    #[serde(default = "default_timeout")]
    pub job_timeout_secs: u64,

    /// Maximum artifact size in bytes
    #[serde(default = "default_max_artifact_size")]
    pub max_artifact_size: u64,
}

/// Shared hashtree network config (`~/.hashtree/config.toml`)
/// Used for Nostr relays and Blossom servers - shared with hashtree-ts
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HashtreeConfig {
    #[serde(default)]
    pub network: NetworkConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NetworkConfig {
    /// Nostr relay URLs for publishing/subscribing
    #[serde(default = "default_relays")]
    pub relays: Vec<String>,

    /// Blossom server URLs for blob storage
    #[serde(default = "default_blossom_servers")]
    pub blossom_servers: Vec<String>,
}

fn default_relays() -> Vec<String> {
    vec![
        "wss://relay.damus.io".to_string(),
        "wss://nos.lol".to_string(),
        "wss://relay.nostr.band".to_string(),
    ]
}

fn default_blossom_servers() -> Vec<String> {
    vec![
        "https://blossom.primal.net".to_string(),
        "https://cdn.satellite.earth".to_string(),
    ]
}

impl Default for RunnerLimits {
    fn default() -> Self {
        Self {
            max_concurrent_jobs: default_max_concurrent(),
            job_timeout_secs: default_timeout(),
            max_artifact_size: default_max_artifact_size(),
        }
    }
}

fn default_max_concurrent() -> u32 {
    4
}

fn default_timeout() -> u64 {
    3600 // 1 hour
}

fn default_max_artifact_size() -> u64 {
    1024 * 1024 * 1024 // 1 GB
}

impl RepoConfig {
    /// Load repo config from `.hashtree/ci.toml`
    pub fn load_from_repo(repo_path: &std::path::Path) -> anyhow::Result<Self> {
        let config_path = repo_path.join(".hashtree/ci.toml");
        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)?;
            Ok(toml::from_str(&content)?)
        } else {
            Ok(Self::default())
        }
    }

    /// Check if a runner npub is trusted
    pub fn is_runner_trusted(&self, npub: &str) -> bool {
        self.ci.runners.iter().any(|r| r.npub == npub)
    }

    /// Get runner by npub
    pub fn get_runner(&self, npub: &str) -> Option<&TrustedRunner> {
        self.ci.runners.iter().find(|r| r.npub == npub)
    }

    /// Find runners matching tags (for job routing)
    pub fn find_runners_by_tags(&self, required_tags: &[String]) -> Vec<&TrustedRunner> {
        self.ci
            .runners
            .iter()
            .filter(|r| required_tags.iter().all(|tag| r.tags.contains(tag)))
            .collect()
    }
}

impl HashtreeConfig {
    /// Load shared hashtree config from `~/.hashtree/config.toml`
    pub fn load() -> anyhow::Result<Self> {
        let home = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;
        let config_path = home.join(".hashtree/config.toml");

        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)?;
            Ok(toml::from_str(&content)?)
        } else {
            // Return defaults if no config file exists
            Ok(Self::default())
        }
    }

    /// Load from specific path
    pub fn load_from(path: &std::path::Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&content)?)
    }
}

impl RunnerConfig {
    /// Load runner config from default location
    pub fn load() -> anyhow::Result<Self> {
        let config_dir = dirs::config_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not find config directory"))?;
        let config_path = config_dir.join("hashtree-ci/runner.toml");
        let content = std::fs::read_to_string(&config_path)?;
        Ok(toml::from_str(&content)?)
    }

    /// Load from specific path
    pub fn load_from(path: &std::path::Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&content)?)
    }

    /// Get the runner's npub (derived from nsec)
    pub fn npub(&self) -> anyhow::Result<String> {
        use nostr::prelude::*;
        let keys = Keys::parse(&self.runner.nsec)?;
        Ok(keys.public_key().to_bech32()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_repo_config_parse() {
        let toml = r#"
[ci]
org_npub = "npub1orgtest"

[[ci.runners]]
npub = "npub1runner1"
name = "linux-x64"
tags = ["linux", "x64", "docker"]

[[ci.runners]]
npub = "npub1runner2"
name = "macos-arm64"
tags = ["macos", "arm64"]
"#;
        let config: RepoConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.ci.org_npub, Some("npub1orgtest".to_string()));
        assert_eq!(config.ci.runners.len(), 2);
        assert!(config.is_runner_trusted("npub1runner1"));
        assert!(!config.is_runner_trusted("npub1unknown"));
    }

    #[test]
    fn test_find_runners_by_tags() {
        let toml = r#"
[ci]
[[ci.runners]]
npub = "npub1runner1"
tags = ["linux", "docker"]

[[ci.runners]]
npub = "npub1runner2"
tags = ["linux", "gpu"]
"#;
        let config: RepoConfig = toml::from_str(toml).unwrap();
        let linux_runners = config.find_runners_by_tags(&["linux".to_string()]);
        assert_eq!(linux_runners.len(), 2);

        let docker_runners = config.find_runners_by_tags(&["linux".to_string(), "docker".to_string()]);
        assert_eq!(docker_runners.len(), 1);
        assert_eq!(docker_runners[0].npub, "npub1runner1");
    }
}
