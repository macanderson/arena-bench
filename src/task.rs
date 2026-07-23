//! Task packs — the on-disk format a suite is written in.
//!
//! A suite is a directory of task directories. Each task directory contains:
//!
//! ```text
//! <suite>/<task-id>/
//!   task.toml        # this file
//!   workspace/       # fixture copied to a fresh directory per episode
//!   verify/          # held-out verification, staged only AFTER the agent exits
//! ```
//!
//! The held-out discipline is inherited from Arena: `verify/` is never on disk
//! while the agent runs. Before milestones execute, anything the agent planted
//! at `.arena-verify/` inside the workspace is deleted and `verify/` is copied
//! there, so an agent can neither read nor author its own graders.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    #[error("suite {0} is not a readable directory")]
    SuiteUnreadable(PathBuf),
    #[error("task {0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("task {0}: task.toml is invalid: {1}")]
    Manifest(PathBuf, toml::de::Error),
    #[error("task {path}: {reason}")]
    Invalid { path: PathBuf, reason: String },
}

/// One milestone: a graded checkpoint, verified by running an argv command
/// with the episode workspace as its working directory (exit 0 = reached).
/// Argv arrays, never a shell string — the same injection-hostile convention
/// Arena uses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Milestone {
    pub name: String,
    /// Relative weight in the task's milestone score. Defaults to 1.
    #[serde(default = "default_weight")]
    pub weight: u32,
    /// The verification command, argv-style. Runs with cwd = workspace.
    pub verify: Vec<String>,
}

fn default_weight() -> u32 {
    1
}

/// Chaos configuration: whether (and when) the runner SIGKILLs the adapter
/// mid-episode and re-invokes it with `--resume`. Durability is judged from
/// the journal by the trace oracles; chaos is merely what makes a run
/// *produce* the interesting journal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChaosSpec {
    /// `[low, high)` seconds after adapter spawn at which the kill fires,
    /// sampled uniformly per invocation from the seeded run PRNG.
    pub kill_window_secs: [f64; 2],
    /// How many kills a single episode may suffer. After the budget is
    /// spent, the adapter runs undisturbed.
    #[serde(default = "default_max_kills")]
    pub max_kills: u32,
}

fn default_max_kills() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TaskManifest {
    task: TaskSection,
    #[serde(default)]
    milestones: Vec<Milestone>,
    #[serde(default)]
    chaos: Option<ChaosSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TaskSection {
    id: String,
    /// The prompt written to `TASK.md` in the seeded workspace — the identical
    /// text every adapter receives.
    prompt: String,
    #[serde(default = "default_timeout")]
    timeout_secs: u64,
}

fn default_timeout() -> u64 {
    600
}

/// A loaded task: manifest plus resolved fixture paths.
#[derive(Debug, Clone)]
pub struct Task {
    pub id: String,
    pub prompt: String,
    pub timeout_secs: u64,
    pub milestones: Vec<Milestone>,
    pub chaos: Option<ChaosSpec>,
    /// The fixture directory copied into each episode workspace. May be
    /// absent — a task can start from an empty workspace.
    pub workspace_fixture: Option<PathBuf>,
    /// The held-out verification directory. May be absent when every
    /// milestone is self-contained.
    pub verify_dir: Option<PathBuf>,
}

impl Task {
    /// Load one task directory.
    pub fn load(dir: &Path) -> Result<Self, TaskError> {
        let manifest_path = dir.join("task.toml");
        let raw = std::fs::read_to_string(&manifest_path)
            .map_err(|error| TaskError::Io(manifest_path.clone(), error))?;
        let manifest: TaskManifest =
            toml::from_str(&raw).map_err(|error| TaskError::Manifest(manifest_path, error))?;

        if manifest.task.id.trim().is_empty() {
            return Err(TaskError::Invalid {
                path: dir.to_path_buf(),
                reason: "task.id must be non-empty".into(),
            });
        }
        for milestone in &manifest.milestones {
            if milestone.verify.is_empty() {
                return Err(TaskError::Invalid {
                    path: dir.to_path_buf(),
                    reason: format!("milestone `{}` has an empty verify argv", milestone.name),
                });
            }
        }
        if let Some(chaos) = &manifest.chaos {
            let [low, high] = chaos.kill_window_secs;
            // NaN fails these comparisons too, so a NaN window is rejected.
            if low < 0.0 || high < low || low.is_nan() || high.is_nan() {
                return Err(TaskError::Invalid {
                    path: dir.to_path_buf(),
                    reason: format!("chaos.kill_window_secs [{low}, {high}] is not a valid window"),
                });
            }
        }

        let workspace_fixture = existing_dir(dir.join("workspace"));
        let verify_dir = existing_dir(dir.join("verify"));
        Ok(Self {
            id: manifest.task.id,
            prompt: manifest.task.prompt,
            timeout_secs: manifest.task.timeout_secs,
            milestones: manifest.milestones,
            chaos: manifest.chaos,
            workspace_fixture,
            verify_dir,
        })
    }

    /// The maximum achievable milestone weight. Zero when the task declares
    /// no milestones.
    pub fn total_weight(&self) -> u32 {
        self.milestones
            .iter()
            .map(|milestone| milestone.weight)
            .sum()
    }
}

fn existing_dir(path: PathBuf) -> Option<PathBuf> {
    path.is_dir().then_some(path)
}

/// Load every task in a suite directory, sorted by task id for stable run
/// order. `only` filters to the named task ids when non-empty.
pub fn load_suite(suite: &Path, only: &[String]) -> Result<Vec<Task>, TaskError> {
    let entries =
        std::fs::read_dir(suite).map_err(|_| TaskError::SuiteUnreadable(suite.to_path_buf()))?;
    let mut tasks = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| TaskError::Io(suite.to_path_buf(), error))?;
        let path = entry.path();
        if path.is_dir() && path.join("task.toml").is_file() {
            tasks.push(Task::load(&path)?);
        }
    }
    if !only.is_empty() {
        tasks.retain(|task| only.iter().any(|id| id == &task.id));
    }
    tasks.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(tasks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_manifest_parses_with_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("task.toml"),
            r#"
[task]
id = "demo"
prompt = "do the thing"

[[milestones]]
name = "exists"
verify = ["test", "-f", "out.txt"]
"#,
        )
        .unwrap();
        let task = Task::load(dir.path()).unwrap();
        assert_eq!(task.id, "demo");
        assert_eq!(task.timeout_secs, 600);
        assert_eq!(task.milestones[0].weight, 1);
        assert_eq!(task.total_weight(), 1);
        assert!(task.chaos.is_none());
        assert!(task.workspace_fixture.is_none());
    }

    #[test]
    fn an_empty_verify_argv_is_rejected_loudly() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("task.toml"),
            r#"
[task]
id = "demo"
prompt = "p"

[[milestones]]
name = "broken"
verify = []
"#,
        )
        .unwrap();
        assert!(matches!(
            Task::load(dir.path()),
            Err(TaskError::Invalid { .. })
        ));
    }
}
