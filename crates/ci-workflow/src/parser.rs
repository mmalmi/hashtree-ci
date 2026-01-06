//! YAML workflow parser for GitHub Actions format.

use ci_core::{Job, JobStep, StepAction};
use serde::Deserialize;
use std::collections::HashMap;

/// Raw workflow file structure
#[derive(Debug, Deserialize)]
pub struct Workflow {
    pub name: Option<String>,
    pub on: WorkflowTrigger,
    #[serde(default)]
    pub env: HashMap<String, String>,
    pub jobs: HashMap<String, WorkflowJob>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum WorkflowTrigger {
    Single(String),
    List(Vec<String>),
    Detailed(HashMap<String, serde_yaml::Value>),
}

#[derive(Debug, Deserialize)]
pub struct WorkflowJob {
    #[serde(rename = "runs-on")]
    pub runs_on: RunsOn,
    #[serde(default)]
    pub env: HashMap<String, String>,
    pub steps: Vec<WorkflowStep>,
    #[serde(rename = "if")]
    pub condition: Option<String>,
    pub needs: Option<JobNeeds>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum RunsOn {
    Single(String),
    List(Vec<String>),
}

impl RunsOn {
    pub fn to_tags(&self) -> Vec<String> {
        match self {
            RunsOn::Single(s) => vec![s.clone()],
            RunsOn::List(v) => v.clone(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum JobNeeds {
    Single(String),
    List(Vec<String>),
}

#[derive(Debug, Deserialize)]
pub struct WorkflowStep {
    pub name: Option<String>,
    pub run: Option<String>,
    pub uses: Option<String>,
    #[serde(default)]
    pub with: HashMap<String, String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(rename = "working-directory")]
    pub working_directory: Option<String>,
    #[serde(rename = "continue-on-error", default)]
    pub continue_on_error: bool,
    #[serde(rename = "timeout-minutes")]
    pub timeout_minutes: Option<u32>,
}

/// Parse a workflow YAML file
pub fn parse_workflow(yaml: &str) -> anyhow::Result<Workflow> {
    Ok(serde_yaml::from_str(yaml)?)
}

/// Convert parsed workflow into executable jobs
pub fn workflow_to_jobs(
    workflow: &Workflow,
    repo_hash: &str,
    commit: &str,
    workflow_path: &str,
) -> Vec<Job> {
    workflow
        .jobs
        .iter()
        .map(|(job_name, wf_job)| {
            let mut job = Job::new(
                repo_hash.to_string(),
                commit.to_string(),
                workflow_path.to_string(),
                job_name.clone(),
            );

            job.runs_on = wf_job.runs_on.to_tags();
            job.env = workflow.env.clone();
            job.env.extend(wf_job.env.clone());

            job.steps = wf_job
                .steps
                .iter()
                .enumerate()
                .map(|(i, step)| {
                    let name = step
                        .name
                        .clone()
                        .unwrap_or_else(|| format!("Step {}", i + 1));

                    let action = if let Some(run) = &step.run {
                        StepAction::Run(run.clone())
                    } else if let Some(uses) = &step.uses {
                        StepAction::Uses {
                            action: uses.clone(),
                            with: step.with.clone(),
                        }
                    } else {
                        StepAction::Run(String::new())
                    };

                    JobStep {
                        name,
                        action,
                        working_directory: step.working_directory.clone(),
                        env: step.env.clone(),
                        continue_on_error: step.continue_on_error,
                        timeout_minutes: step.timeout_minutes,
                    }
                })
                .collect();

            job
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_workflow() {
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
"#;
        let workflow = parse_workflow(yaml).unwrap();
        assert_eq!(workflow.name, Some("CI".to_string()));
        assert_eq!(workflow.jobs.len(), 1);

        let jobs = workflow_to_jobs(&workflow, "htree://abc", "abc123", ".github/workflows/ci.yml");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].job_name, "build");
        assert_eq!(jobs[0].steps.len(), 2);
    }
}
