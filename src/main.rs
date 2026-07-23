use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

use arena_bench::{RunConfig, judge_journal, load_suite, run};

#[derive(Parser)]
#[command(
    name = "arena-bench",
    about = "Loop-integrity benchmark runner for coding agents",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a suite against an adapter and write a full report.
    Run {
        /// Suite directory (one subdirectory per task).
        #[arg(long)]
        suite: PathBuf,
        /// Adapter manifest (TOML).
        #[arg(long)]
        adapter: PathBuf,
        /// Output directory for run artifacts.
        #[arg(long, default_value = "results")]
        out: PathBuf,
        /// Episodes per (task, arm) — repeated exposure for learning curves.
        #[arg(long, default_value_t = 1)]
        episodes: u32,
        /// Comma-separated arms. `amnesic` wipes agent state between
        /// episodes; any other name persists it.
        #[arg(long, value_delimiter = ',', default_value = "persistent")]
        arms: Vec<String>,
        /// Enable the tasks' chaos specs (SIGKILL + resume).
        #[arg(long)]
        chaos: bool,
        /// Seed for the deterministic chaos schedule.
        #[arg(long, default_value_t = 0xA9E7A)]
        seed: u64,
        /// Restrict to these task ids.
        #[arg(long = "task")]
        tasks: Vec<String>,
        /// Exit nonzero unless every episode passes both the task and the
        /// loop dimension (CI mode).
        #[arg(long)]
        strict: bool,
    },
    /// Judge a standalone contextgraph-trace journal with the oracles.
    Judge { journal: PathBuf },
    /// List the tasks in a suite.
    List {
        #[arg(long)]
        suite: PathBuf,
    },
}

fn main() -> ExitCode {
    match main_inner() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("arena-bench: {error:#}");
            ExitCode::from(2)
        }
    }
}

fn main_inner() -> Result<ExitCode> {
    match Cli::parse().command {
        Commands::Run {
            suite,
            adapter,
            out,
            episodes,
            arms,
            chaos,
            seed,
            tasks,
            strict,
        } => {
            let mut config = RunConfig::new(suite, adapter, out.clone());
            config.episodes = episodes;
            config.arms = arms;
            config.chaos = chaos;
            config.seed = seed;
            config.only_tasks = tasks;
            let report = run(&config)?;
            println!("{}", report.to_markdown());
            println!("artifacts: {}", out.join(&report.run_id).display());
            if strict && !report.all_green() {
                return Ok(ExitCode::from(1));
            }
            Ok(ExitCode::SUCCESS)
        }
        Commands::Judge { journal } => {
            let verdict = judge_journal(&journal);
            println!("{}", serde_json::to_string_pretty(&verdict)?);
            Ok(if verdict.loop_passed() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        }
        Commands::List { suite } => {
            for task in load_suite(&suite, &[])? {
                println!(
                    "{}  ({} milestone(s), timeout {}s{})",
                    task.id,
                    task.milestones.len(),
                    task.timeout_secs,
                    if task.chaos.is_some() { ", chaos" } else { "" }
                );
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}
