//! The adapter contract — how the runner reaches any agent, and how a Rust
//! agent can implement the contract with zero ceremony.
//!
//! An adapter is an executable described by a small TOML manifest:
//!
//! ```toml
//! [adapter]
//! name = "stella"
//! command = ["stella", "arena"]        # argv array, never a shell string
//!
//! [adapter.env]                        # optional extra environment
//! STELLA_NONINTERACTIVE = "1"
//! ```
//!
//! Per invocation the runner appends these flags (and mirrors them into the
//! environment, so non-argv-friendly harnesses can read `ARENA_*` instead):
//!
//! ```text
//! --task-dir <dir>     ARENA_TASK_DIR    the episode workspace; TASK.md inside
//! --journal <file>     ARENA_JOURNAL     append contextgraph-trace NDJSON here
//! --state-dir <dir>    ARENA_STATE_DIR   persistent across episodes (memory)
//! --resume             ARENA_RESUME=1    present on re-invocation after a kill
//! ```
//!
//! The agent works on the task described by `TASK.md`, **appends** journal
//! events as it goes (the journal must survive a SIGKILL at any point — write
//! line-by-line, never buffer the run), and exits 0 when it believes the task
//! is done. On `--resume` it continues the same session: same session id,
//! `seq` continuing densely, a `resume` event first.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("adapter manifest {0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("adapter manifest {0} is invalid: {1}")]
    Manifest(PathBuf, toml::de::Error),
    #[error("adapter manifest {0}: command must be a non-empty argv array")]
    EmptyCommand(PathBuf),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AdapterManifest {
    adapter: AdapterSpec,
}

/// A parsed adapter manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterSpec {
    pub name: String,
    /// The base argv the runner spawns. Invocation flags are appended.
    pub command: Vec<String>,
    /// Extra environment applied to every invocation.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

impl AdapterSpec {
    pub fn load(path: &Path) -> Result<Self, AdapterError> {
        let raw = std::fs::read_to_string(path)
            .map_err(|error| AdapterError::Io(path.to_path_buf(), error))?;
        let mut manifest: AdapterManifest = toml::from_str(&raw)
            .map_err(|error| AdapterError::Manifest(path.to_path_buf(), error))?;
        if manifest.adapter.command.is_empty() {
            return Err(AdapterError::EmptyCommand(path.to_path_buf()));
        }
        // The adapter later runs with the episode workspace as its cwd, so a
        // relative program path in the manifest (`target/debug/…`) must be
        // anchored now, to the directory the runner was invoked from. A bare
        // name without a separator stays as-is for PATH lookup.
        let program = &mut manifest.adapter.command[0];
        if program.contains(std::path::MAIN_SEPARATOR)
            && Path::new(program).is_relative()
            && let Ok(cwd) = std::env::current_dir()
        {
            *program = cwd.join(&*program).display().to_string();
        }
        Ok(manifest.adapter)
    }
}

/// The per-invocation arguments of the adapter contract, from the agent's
/// side. A Rust adapter calls [`AdapterArgs::from_env_and_args`] in `main`
/// and is done parsing.
#[derive(Debug, Clone)]
pub struct AdapterArgs {
    pub task_dir: PathBuf,
    pub journal: PathBuf,
    pub state_dir: PathBuf,
    pub resume: bool,
}

impl AdapterArgs {
    /// Parse the contract from argv (`--task-dir`, `--journal`,
    /// `--state-dir`, `--resume`), falling back to the `ARENA_*` environment
    /// mirrors for anything argv did not provide.
    pub fn from_env_and_args(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut task_dir: Option<PathBuf> = None;
        let mut journal: Option<PathBuf> = None;
        let mut state_dir: Option<PathBuf> = None;
        let mut resume = false;

        let mut args = args.peekable();
        while let Some(arg) = args.next() {
            let take = |slot: &mut Option<PathBuf>,
                        flag: &str,
                        next: Option<String>|
             -> Result<(), String> {
                match next {
                    Some(value) => {
                        *slot = Some(PathBuf::from(value));
                        Ok(())
                    }
                    None => Err(format!("{flag} requires a value")),
                }
            };
            match arg.as_str() {
                "--task-dir" => take(&mut task_dir, "--task-dir", args.next())?,
                "--journal" => take(&mut journal, "--journal", args.next())?,
                "--state-dir" => take(&mut state_dir, "--state-dir", args.next())?,
                "--resume" => resume = true,
                _ => {} // an adapter may take flags of its own; ignore here
            }
        }

        let from_env = |name: &str| std::env::var(name).ok().map(PathBuf::from);
        let task_dir = task_dir
            .or_else(|| from_env("ARENA_TASK_DIR"))
            .ok_or("missing --task-dir (or ARENA_TASK_DIR)")?;
        let journal = journal
            .or_else(|| from_env("ARENA_JOURNAL"))
            .ok_or("missing --journal (or ARENA_JOURNAL)")?;
        let state_dir = state_dir
            .or_else(|| from_env("ARENA_STATE_DIR"))
            .ok_or("missing --state-dir (or ARENA_STATE_DIR)")?;
        let resume = resume || std::env::var("ARENA_RESUME").as_deref() == Ok("1");
        Ok(Self {
            task_dir,
            journal,
            state_dir,
            resume,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_contract_parses_from_argv() {
        let args = [
            "--task-dir",
            "/w",
            "--journal",
            "/j.ndjson",
            "--state-dir",
            "/s",
            "--resume",
        ]
        .into_iter()
        .map(String::from);
        let parsed = AdapterArgs::from_env_and_args(args).unwrap();
        assert_eq!(parsed.task_dir, PathBuf::from("/w"));
        assert_eq!(parsed.journal, PathBuf::from("/j.ndjson"));
        assert!(parsed.resume);
    }

    #[test]
    fn unknown_flags_belong_to_the_adapter_and_are_ignored() {
        let args = [
            "--verbose",
            "--task-dir",
            "/w",
            "--journal",
            "/j",
            "--state-dir",
            "/s",
        ]
        .into_iter()
        .map(String::from);
        assert!(AdapterArgs::from_env_and_args(args).is_ok());
    }

    #[test]
    fn a_manifest_with_an_empty_command_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "[adapter]\nname = \"x\"\ncommand = []\n").unwrap();
        assert!(matches!(
            AdapterSpec::load(&path),
            Err(AdapterError::EmptyCommand(_))
        ));
    }
}
