//! arena-bench — a loop-integrity benchmark runner for coding agents.
//!
//! Outcome benchmarks (Harbor, [Arena](https://github.com/macanderson/arena))
//! grade *what an agent produced*. arena-bench grades *how the loop behaved
//! while producing it*: every episode records a
//! [`contextgraph-trace`](https://github.com/macanderson/context-graph-protocol)
//! journal, and the trace oracles judge the recording — context staleness at
//! use, budget honesty at assembly, tool-call pairing, deterministic
//! composition, side-effect exactly-once, resume integrity. Chaos mode
//! SIGKILLs the agent mid-episode and re-invokes it, so durability stops
//! being a claim and becomes a recording.
//!
//! The runner reaches any agent through a small executable contract
//! ([`adapter`]) in the tradition of Arena's and Harbor's adapters, and
//! never blends its dimensions: task success, loop integrity, and cost are
//! reported separately ([`report`]).

pub mod adapter;
pub mod journal;
pub mod judge;
pub mod report;
pub mod runner;
pub mod task;
pub mod util;

pub use adapter::{AdapterArgs, AdapterSpec};
pub use journal::JournalWriter;
pub use judge::{CheckLookup, JournalMetrics, JournalVerdict, judge_journal};
pub use report::{ArmReport, EpisodeReport, MilestoneResult, Outcome, RunReport, TaskReport};
pub use runner::{AMNESIC_ARM, RunConfig, run};
pub use task::{ChaosSpec, Milestone, Task, load_suite};
