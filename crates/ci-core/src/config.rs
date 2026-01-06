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
