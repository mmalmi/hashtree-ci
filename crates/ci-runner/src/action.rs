//! Action resolver and executor.
//!
//! Actions in hashtree-ci are stored as hashtree repositories:
//! - Format: `owner/action-name@version`
//! - Example: `npub1.../checkout@v1` or `npub1.../setup-node@v3`
//!
//! Each action has an `action.yml` or `action.toml` that defines:
//! - name: Human-readable name
//! - description: What the action does
//! - inputs: Parameters the action accepts
//! - runs: The steps to execute (composite) or Docker image
//!
//! ## Built-in Actions
//!
//! Some actions are built-in and handled specially:
//! - `checkout` or `actions/checkout@*`: Check out the repository
//! - `cache` or `actions/cache@*`: Cache dependencies (TODO)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Parsed action reference
#[derive(Debug, Clone)]
pub struct ActionRef {
    /// Owner (npub or "actions" for built-ins)
    pub owner: String,
    /// Action name
    pub name: String,
    /// Version (tag, branch, or commit)
    pub version: String,
}

impl ActionRef {
    /// Parse an action reference string
    /// Formats: "owner/name@version", "name@version" (assumes built-in)
    pub fn parse(action_str: &str) -> Option<Self> {
        let (path, version) = if let Some(at_pos) = action_str.rfind('@') {
            (&action_str[..at_pos], &action_str[at_pos + 1..])
        } else {
            // No version specified, use "main"
            (action_str, "main")
        };

        let parts: Vec<&str> = path.split('/').collect();
        match parts.len() {
            1 => Some(ActionRef {
                owner: "actions".to_string(),
                name: parts[0].to_string(),
                version: version.to_string(),
            }),
            2 => Some(ActionRef {
                owner: parts[0].to_string(),
                name: parts[1].to_string(),
                version: version.to_string(),
            }),
            _ => None,
        }
    }

    /// Check if this is a built-in action
    pub fn is_builtin(&self) -> bool {
        self.owner == "actions"
    }
}

/// Action definition (from action.yml)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionDefinition {
    /// Action name
    pub name: String,

    /// Description
    #[serde(default)]
    pub description: String,

    /// Input definitions
    #[serde(default)]
    pub inputs: HashMap<String, ActionInput>,

    /// How to run the action
    pub runs: ActionRuns,
}

/// Input parameter definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionInput {
    /// Description of the input
    #[serde(default)]
    pub description: String,

    /// Whether this input is required
    #[serde(default)]
    pub required: bool,

    /// Default value if not provided
    #[serde(default)]
    pub default: Option<String>,
}

/// How the action runs
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "using")]
pub enum ActionRuns {
    /// Composite action with shell steps
    #[serde(rename = "composite")]
    Composite { steps: Vec<CompositeStep> },

    /// Node.js action (not yet supported)
    #[serde(rename = "node20")]
    Node20 { main: String },

    /// Docker action (not yet supported)
    #[serde(rename = "docker")]
    Docker { image: String },
}

/// A step in a composite action
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompositeStep {
    /// Step name
    #[serde(default)]
    pub name: String,

    /// Shell command
    pub run: String,

    /// Shell to use (bash, sh, pwsh, etc.)
    #[serde(default = "default_shell")]
    pub shell: String,

    /// Working directory
    #[serde(default)]
    pub working_directory: Option<String>,

    /// Environment variables
    #[serde(default)]
    pub env: HashMap<String, String>,
}

fn default_shell() -> String {
    "sh".to_string()
}

/// Execute a built-in action
pub async fn execute_builtin_action(
    action_ref: &ActionRef,
    inputs: &HashMap<String, String>,
    work_dir: &Path,
) -> Result<ActionResult, String> {
    match action_ref.name.as_str() {
        "checkout" => execute_checkout(inputs, work_dir).await,
        "cache" => {
            // Cache is a no-op for now (local execution)
            Ok(ActionResult {
                success: true,
                logs: "Cache action not yet implemented (skipped)".to_string(),
                outputs: HashMap::new(),
            })
        }
        "setup-node" | "setup-python" | "setup-go" | "setup-rust" => {
            // Setup actions are no-ops in local execution (tools should be pre-installed)
            Ok(ActionResult {
                success: true,
                logs: format!("Setup action {} skipped (use container with tools pre-installed)", action_ref.name),
                outputs: HashMap::new(),
            })
        }
        _ => Err(format!("Unknown built-in action: {}", action_ref.name)),
    }
}

/// Result of executing an action
#[derive(Debug, Clone)]
pub struct ActionResult {
    pub success: bool,
    pub logs: String,
    pub outputs: HashMap<String, String>,
}

/// Execute the checkout action
async fn execute_checkout(
    inputs: &HashMap<String, String>,
    work_dir: &Path,
) -> Result<ActionResult, String> {
    let mut logs = Vec::new();
    logs.push("Running checkout action...".to_string());

    // Get inputs
    let repo = inputs.get("repository").cloned();
    let ref_name = inputs.get("ref").cloned();
    let path = inputs.get("path").cloned();
    let fetch_depth = inputs
        .get("fetch-depth")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1);

    // For hashtree-ci, "checkout" means the repo is already available in work_dir
    // This action just logs confirmation and optionally switches refs

    if let Some(repo) = &repo {
        logs.push(format!("Repository: {}", repo));
    } else {
        logs.push("Using default repository (already checked out)".to_string());
    }

    if let Some(ref_name) = &ref_name {
        logs.push(format!("Ref: {}", ref_name));
        // In a full implementation, we would switch to this ref
    }

    if let Some(path) = &path {
        logs.push(format!("Path: {}", path));
        // In a full implementation, we would checkout to this subpath
    }

    logs.push(format!("Fetch depth: {}", fetch_depth));
    logs.push(format!("Work directory: {}", work_dir.display()));

    // Verify work directory exists
    if !work_dir.exists() {
        return Err(format!("Work directory does not exist: {}", work_dir.display()));
    }

    logs.push("Checkout complete (repository already available)".to_string());

    Ok(ActionResult {
        success: true,
        logs: logs.join("\n"),
        outputs: HashMap::new(),
    })
}

/// Substitute input variables in a command string
/// Replaces `${{ inputs.name }}` with the actual input value
pub fn substitute_inputs(cmd: &str, inputs: &HashMap<String, String>) -> String {
    let mut result = cmd.to_string();

    for (key, value) in inputs {
        let pattern = format!("${{{{ inputs.{} }}}}", key);
        result = result.replace(&pattern, value);

        // Also support without spaces
        let pattern_no_space = format!("${{{{inputs.{}}}}}", key);
        result = result.replace(&pattern_no_space, value);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_action_ref() {
        // Full format
        let action = ActionRef::parse("actions/checkout@v4").unwrap();
        assert_eq!(action.owner, "actions");
        assert_eq!(action.name, "checkout");
        assert_eq!(action.version, "v4");

        // Short format (built-in)
        let action = ActionRef::parse("checkout@v4").unwrap();
        assert_eq!(action.owner, "actions");
        assert_eq!(action.name, "checkout");
        assert_eq!(action.version, "v4");

        // No version
        let action = ActionRef::parse("owner/action").unwrap();
        assert_eq!(action.owner, "owner");
        assert_eq!(action.name, "action");
        assert_eq!(action.version, "main");

        // npub owner
        let action = ActionRef::parse("npub1abc123/my-action@v1.0").unwrap();
        assert_eq!(action.owner, "npub1abc123");
        assert_eq!(action.name, "my-action");
        assert_eq!(action.version, "v1.0");
    }

    #[test]
    fn test_substitute_inputs() {
        let mut inputs = HashMap::new();
        inputs.insert("name".to_string(), "world".to_string());
        inputs.insert("version".to_string(), "1.0".to_string());

        let cmd = "echo Hello ${{ inputs.name }} v${{ inputs.version }}";
        let result = substitute_inputs(cmd, &inputs);
        assert_eq!(result, "echo Hello world v1.0");
    }

    #[test]
    fn test_is_builtin() {
        let builtin = ActionRef::parse("actions/checkout@v4").unwrap();
        assert!(builtin.is_builtin());

        let custom = ActionRef::parse("npub1abc/my-action@v1").unwrap();
        assert!(!custom.is_builtin());
    }
}
