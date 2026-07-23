//! Episode orchestration: seed a workspace, invoke the adapter (killing it on
//! purpose when chaos is on), stage held-out verification, grade milestones,
//! and judge the journal with the trace oracles.
//!
//! The separation of concerns is deliberate: chaos only *produces* the
//! interesting journal — every durability judgement (resume honesty, effect
//! exactly-once, staleness) is made by the `contextgraph-trace` oracles
//! reading the recording afterwards. The runner never trusts the adapter's
//! self-report for anything it can observe or replay itself.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::adapter::AdapterSpec;
use crate::judge::judge_journal;
use crate::report::{ArmReport, EpisodeReport, MilestoneResult, Outcome, RunReport, TaskReport};
use crate::task::{ChaosSpec, Task, load_suite};
use crate::util::{SplitMix64, copy_dir, run_id};

/// The arm name whose persistent state is wiped before every episode —
/// the memory-ablation baseline. Any other arm keeps its state dir across
/// episodes, which is what lets a self-improving agent improve.
pub const AMNESIC_ARM: &str = "amnesic";

#[derive(Debug, Clone)]
pub struct RunConfig {
    pub suite: PathBuf,
    pub adapter_manifest: PathBuf,
    pub out_dir: PathBuf,
    /// Episodes per (task, arm) — repeated exposure for learning curves.
    pub episodes: u32,
    /// Arms to run. [`AMNESIC_ARM`] wipes state between episodes; every
    /// other name persists it.
    pub arms: Vec<String>,
    /// Master switch for the tasks' chaos specs.
    pub chaos: bool,
    pub seed: u64,
    /// Restrict to these task ids when non-empty.
    pub only_tasks: Vec<String>,
}

impl RunConfig {
    pub fn new(suite: PathBuf, adapter_manifest: PathBuf, out_dir: PathBuf) -> Self {
        Self {
            suite,
            adapter_manifest,
            out_dir,
            episodes: 1,
            arms: vec!["persistent".to_string()],
            chaos: false,
            seed: 0xA9E7A,
            only_tasks: Vec::new(),
        }
    }
}

/// Run a full suite. Every failure inside an episode becomes data in the
/// report; only harness-level problems (unreadable suite, unspawnable
/// output dir) are errors.
pub fn run(config: &RunConfig) -> Result<RunReport> {
    let adapter = AdapterSpec::load(&config.adapter_manifest)?;
    let tasks = load_suite(&config.suite, &config.only_tasks)?;
    anyhow::ensure!(
        !tasks.is_empty(),
        "suite {} contains no tasks{}",
        config.suite.display(),
        if config.only_tasks.is_empty() {
            String::new()
        } else {
            format!(" matching {:?}", config.only_tasks)
        }
    );

    let run_id = run_id();
    // Absolutize before any episode path derives from it: workspace, journal,
    // and state paths are handed to an adapter whose cwd is the workspace, so
    // a relative `--out` (the default, `results/`) would otherwise send the
    // agent's journal to a path that only resolves from the runner's cwd.
    let out_dir = std::path::absolute(&config.out_dir)
        .with_context(|| format!("resolving {}", config.out_dir.display()))?;
    let run_dir = out_dir.join(&run_id);
    std::fs::create_dir_all(&run_dir).with_context(|| format!("creating {}", run_dir.display()))?;

    let mut rng = SplitMix64::new(config.seed);
    let mut task_reports = Vec::new();
    for task in &tasks {
        let mut arm_reports = Vec::new();
        for arm in &config.arms {
            let arm_dir = run_dir.join(&task.id).join(arm);
            let state_dir = arm_dir.join("state");
            let mut episodes = Vec::new();
            for episode in 1..=config.episodes {
                if arm == AMNESIC_ARM && state_dir.exists() {
                    std::fs::remove_dir_all(&state_dir)?;
                }
                std::fs::create_dir_all(&state_dir)?;
                let episode_dir = arm_dir.join(format!("ep{episode}"));
                let report = run_episode(
                    task,
                    &adapter,
                    episode,
                    &episode_dir,
                    &state_dir,
                    config,
                    &mut rng,
                )?;
                std::fs::write(
                    episode_dir.join("episode.json"),
                    serde_json::to_string_pretty(&report)?,
                )?;
                episodes.push(report);
            }
            arm_reports.push(ArmReport {
                arm: arm.clone(),
                episodes,
            });
        }
        task_reports.push(TaskReport {
            task_id: task.id.clone(),
            arms: arm_reports,
        });
    }

    let report = RunReport {
        run_id,
        seed: config.seed,
        suite: config.suite.display().to_string(),
        adapter: adapter.name.clone(),
        chaos: config.chaos,
        episodes_per_arm: config.episodes,
        tasks: task_reports,
    };
    std::fs::write(
        run_dir.join("run.json"),
        serde_json::to_string_pretty(&report)?,
    )?;
    std::fs::write(run_dir.join("report.md"), report.to_markdown())?;
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
fn run_episode(
    task: &Task,
    adapter: &AdapterSpec,
    episode: u32,
    episode_dir: &Path,
    state_dir: &Path,
    config: &RunConfig,
    rng: &mut SplitMix64,
) -> Result<EpisodeReport> {
    let workspace = episode_dir.join("workspace");
    std::fs::create_dir_all(&workspace)?;
    if let Some(fixture) = &task.workspace_fixture {
        copy_dir(fixture, &workspace)?;
    }
    std::fs::write(workspace.join("TASK.md"), &task.prompt)?;
    let journal = episode_dir.join("journal.ndjson");
    let stdout_log = episode_dir.join("agent.stdout");
    let stderr_log = episode_dir.join("agent.stderr");

    let chaos = if config.chaos {
        task.chaos.as_ref()
    } else {
        None
    };
    let deadline = Instant::now() + Duration::from_secs(task.timeout_secs);
    let started = Instant::now();

    let mut invocations: u32 = 0;
    let mut chaos_kills: u32 = 0;
    let outcome = loop {
        let kill_after =
            chaos
                .filter(|spec| chaos_kills < spec.max_kills)
                .map(|spec: &ChaosSpec| {
                    Duration::from_secs_f64(
                        rng.sample_range(spec.kill_window_secs[0], spec.kill_window_secs[1]),
                    )
                });
        let resume = invocations > 0;
        let end = invoke_adapter(
            adapter,
            &workspace,
            &journal,
            state_dir,
            resume,
            &stdout_log,
            &stderr_log,
            kill_after,
            deadline,
        )?;
        invocations += 1;
        match end {
            InvocationEnd::Exited(0) => break Outcome::Completed,
            InvocationEnd::Exited(code) => {
                break Outcome::AgentError {
                    exit_code: code,
                    spawn_error: None,
                };
            }
            InvocationEnd::SpawnFailed(reason) => {
                break Outcome::AgentError {
                    exit_code: -1,
                    spawn_error: Some(reason),
                };
            }
            InvocationEnd::TimedOut => break Outcome::Timeout,
            InvocationEnd::ChaosKilled => {
                chaos_kills += 1;
                continue;
            }
        }
    };
    let wall_secs = started.elapsed().as_secs_f64();

    // Held-out verification, staged only now — anything the agent planted at
    // the verify path is deleted first (Arena's discipline).
    let staged_verify = workspace.join(".arena-verify");
    if staged_verify.exists() {
        std::fs::remove_dir_all(&staged_verify)?;
    }
    if let Some(verify_dir) = &task.verify_dir {
        copy_dir(verify_dir, &staged_verify)?;
    }

    let mut milestones = Vec::new();
    for milestone in &task.milestones {
        let reached = run_verify(&milestone.verify, &workspace);
        milestones.push(MilestoneResult {
            name: milestone.name.clone(),
            weight: milestone.weight,
            reached,
        });
    }
    let total_weight: u32 = task.total_weight();
    let reached_weight: u32 = milestones
        .iter()
        .filter(|milestone| milestone.reached)
        .map(|milestone| milestone.weight)
        .sum();
    let milestone_score = if total_weight == 0 {
        1.0
    } else {
        f64::from(reached_weight) / f64::from(total_weight)
    };

    let journal_verdict = judge_journal(&journal);

    Ok(EpisodeReport {
        episode,
        outcome,
        wall_secs,
        invocations,
        chaos_kills,
        milestones,
        milestone_score,
        journal: journal_verdict,
    })
}

/// Run one milestone verify command with the workspace as cwd. Exit 0 =
/// reached; a command that cannot even spawn counts as not reached.
fn run_verify(argv: &[String], workspace: &Path) -> bool {
    Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(workspace)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

enum InvocationEnd {
    Exited(i32),
    SpawnFailed(String),
    ChaosKilled,
    TimedOut,
}

#[allow(clippy::too_many_arguments)]
fn invoke_adapter(
    adapter: &AdapterSpec,
    workspace: &Path,
    journal: &Path,
    state_dir: &Path,
    resume: bool,
    stdout_log: &Path,
    stderr_log: &Path,
    kill_after: Option<Duration>,
    deadline: Instant,
) -> Result<InvocationEnd> {
    let stdout = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(stdout_log)?;
    let stderr = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(stderr_log)?;

    let mut command = Command::new(&adapter.command[0]);
    command
        .args(&adapter.command[1..])
        .arg("--task-dir")
        .arg(workspace)
        .arg("--journal")
        .arg(journal)
        .arg("--state-dir")
        .arg(state_dir)
        .envs(&adapter.env)
        .env("ARENA_TASK_DIR", workspace)
        .env("ARENA_JOURNAL", journal)
        .env("ARENA_STATE_DIR", state_dir)
        .env("ARENA_RESUME", if resume { "1" } else { "0" })
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr);
    if resume {
        command.arg("--resume");
    }
    // Own process group, so a chaos kill takes the adapter's children with it
    // — killing only the wrapper while the real agent lives on would make
    // "crash" a lie.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return Ok(InvocationEnd::SpawnFailed(error.to_string())),
    };
    let spawned = Instant::now();

    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(InvocationEnd::Exited(status.code().unwrap_or(-1)));
        }
        if let Some(kill_after) = kill_after
            && spawned.elapsed() >= kill_after
        {
            kill_group(&mut child);
            let _ = child.wait();
            return Ok(InvocationEnd::ChaosKilled);
        }
        if Instant::now() >= deadline {
            kill_group(&mut child);
            let _ = child.wait();
            return Ok(InvocationEnd::TimedOut);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// SIGKILL the adapter's whole process group — no warning, no grace, exactly
/// the crash the durability oracles exist to judge.
fn kill_group(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pgid = child.id() as libc::pid_t;
        // Safety: killpg with SIGKILL on the group we created via
        // process_group(0); failure falls through to the direct kill.
        if unsafe { libc::killpg(pgid, libc::SIGKILL) } == 0 {
            return;
        }
    }
    let _ = child.kill();
}
