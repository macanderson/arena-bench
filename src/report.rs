//! Run reports. Three dimensions per episode, never blended into one number
//! (Arena's rule): **task** (outcome + milestone score), **loop** (the trace
//! oracles' verdict), and **cost** (wall clock, journal-declared tokens). A
//! run that solves every task while failing `effect-exactly-once` is exactly
//! the run this tool exists to expose, and a blended score would hide it.

use serde::{Deserialize, Serialize};

use crate::judge::JournalVerdict;

/// How an episode's adapter invocation chain ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    /// The adapter exited 0 — it believes the task is done.
    Completed,
    /// The wall-clock cap fired. Takes precedence over everything: a timeout
    /// never scores as a win regardless of workspace state.
    Timeout,
    /// The adapter could not be spawned or exited nonzero — a harness-side
    /// failure, reported separately, never counted as the agent "losing".
    AgentError {
        exit_code: i32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spawn_error: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MilestoneResult {
    pub name: String,
    pub weight: u32,
    pub reached: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EpisodeReport {
    pub episode: u32,
    pub outcome: Outcome,
    pub wall_secs: f64,
    /// Adapter invocations, including chaos re-invocations.
    pub invocations: u32,
    pub chaos_kills: u32,
    pub milestones: Vec<MilestoneResult>,
    /// Weighted fraction of milestone weight reached, in `[0, 1]`.
    pub milestone_score: f64,
    pub journal: JournalVerdict,
}

impl EpisodeReport {
    /// The task dimension: completed within budget with every milestone
    /// reached. Says nothing about the loop — that is `loop_passed`.
    pub fn task_passed(&self) -> bool {
        matches!(self.outcome, Outcome::Completed) && self.milestone_score >= 1.0
    }

    /// The loop dimension: the journal parsed and every oracle held.
    pub fn loop_passed(&self) -> bool {
        self.journal.loop_passed()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArmReport {
    pub arm: String,
    pub episodes: Vec<EpisodeReport>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskReport {
    pub task_id: String,
    pub arms: Vec<ArmReport>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunReport {
    pub run_id: String,
    pub seed: u64,
    pub suite: String,
    pub adapter: String,
    pub chaos: bool,
    pub episodes_per_arm: u32,
    pub tasks: Vec<TaskReport>,
}

impl RunReport {
    /// Whether every episode passed both dimensions — the CI verdict.
    pub fn all_green(&self) -> bool {
        self.episodes()
            .all(|episode| episode.task_passed() && episode.loop_passed())
    }

    pub fn episodes(&self) -> impl Iterator<Item = &EpisodeReport> {
        self.tasks
            .iter()
            .flat_map(|task| task.arms.iter())
            .flat_map(|arm| arm.episodes.iter())
    }

    /// Render the human report. One row per episode; the two verdict columns
    /// stay separate on purpose.
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "# arena-bench {}\n\nsuite `{}` · adapter `{}` · seed {} · chaos {}\n\n",
            self.run_id,
            self.suite,
            self.adapter,
            self.seed,
            if self.chaos { "on" } else { "off" }
        ));
        out.push_str(
            "| task | arm | ep | task verdict | milestones | loop verdict | turns | prompt tokens | kills | wall (s) |\n",
        );
        out.push_str("|---|---|---|---|---|---|---|---|---|---|\n");
        for task in &self.tasks {
            for arm in &task.arms {
                for episode in &arm.episodes {
                    let task_verdict = match &episode.outcome {
                        Outcome::Completed if episode.task_passed() => "pass".to_string(),
                        Outcome::Completed => "incomplete".to_string(),
                        Outcome::Timeout => "timeout".to_string(),
                        Outcome::AgentError { exit_code, .. } => {
                            format!("agent-error({exit_code})")
                        }
                    };
                    let (loop_verdict, turns, tokens) = match &episode.journal {
                        JournalVerdict::Judged {
                            passed,
                            report,
                            metrics,
                        } => {
                            let verdict = if *passed {
                                "pass".to_string()
                            } else {
                                let failed: Vec<&str> =
                                    report.failures().map(|check| check.name.as_str()).collect();
                                format!("FAIL: {}", failed.join(", "))
                            };
                            (
                                verdict,
                                metrics.turns.to_string(),
                                metrics.declared_prompt_tokens.to_string(),
                            )
                        }
                        JournalVerdict::Missing => {
                            ("FAIL: journal missing".to_string(), "-".into(), "-".into())
                        }
                        JournalVerdict::Invalid { .. } => {
                            ("FAIL: journal invalid".to_string(), "-".into(), "-".into())
                        }
                    };
                    let reached = episode
                        .milestones
                        .iter()
                        .filter(|milestone| milestone.reached)
                        .count();
                    out.push_str(&format!(
                        "| {} | {} | {} | {} | {}/{} ({:.0}%) | {} | {} | {} | {} | {:.1} |\n",
                        task.task_id,
                        arm.arm,
                        episode.episode,
                        task_verdict,
                        reached,
                        episode.milestones.len(),
                        episode.milestone_score * 100.0,
                        loop_verdict,
                        turns,
                        tokens,
                        episode.chaos_kills,
                        episode.wall_secs,
                    ));
                }
            }
        }
        out
    }
}
