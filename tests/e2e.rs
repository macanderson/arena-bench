//! End-to-end proof over the real binary contract: the runner seeds the demo
//! task, invokes the scripted agent, kills it mid-episode when chaos is on,
//! stages held-out verification, and judges the journal with the trace
//! oracles. The misbehave cases prove the pipeline *catches* a broken loop —
//! a runner that can only bless a healthy agent proves nothing.

use std::path::{Path, PathBuf};

use arena_bench::{CheckLookup, JournalVerdict, Outcome, RunConfig, RunReport, run};

fn suite_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("suites/demo")
}

/// Write an adapter manifest pointing at the compiled scripted agent, with
/// extra env for the scenario under test.
fn manifest(dir: &Path, env: &[(&str, &str)]) -> PathBuf {
    let mut body = format!(
        "[adapter]\nname = \"scripted\"\ncommand = [{:?}]\n\n[adapter.env]\n",
        env!("CARGO_BIN_EXE_arena-scripted-agent")
    );
    for (key, value) in env {
        body.push_str(&format!("{key} = {value:?}\n"));
    }
    let path = dir.join("adapter.toml");
    std::fs::write(&path, body).unwrap();
    path
}

fn run_demo(env: &[(&str, &str)], chaos: bool) -> RunReport {
    let out = tempfile::tempdir().unwrap();
    let mut config = RunConfig::new(
        suite_dir(),
        manifest(out.path(), env),
        out.path().join("results"),
    );
    config.chaos = chaos;
    config.seed = 7;
    run(&config).unwrap()
}

fn only_episode(report: &RunReport) -> &arena_bench::EpisodeReport {
    &report.tasks[0].arms[0].episodes[0]
}

#[test]
fn a_clean_run_passes_both_dimensions() {
    let report = run_demo(&[], false);
    let episode = only_episode(&report);
    assert_eq!(episode.outcome, Outcome::Completed);
    assert_eq!(episode.milestone_score, 1.0, "{:?}", episode.milestones);
    assert!(episode.loop_passed(), "{:?}", episode.journal);
    assert!(report.all_green());
    assert_eq!(episode.chaos_kills, 0);
    assert_eq!(episode.invocations, 1);
}

#[test]
fn a_chaos_kill_mid_effect_is_survived_by_an_honest_resume() {
    // The agent lingers 6s between performing its side effect and resolving
    // the call; the kill window is 1.5–2.5s, so the SIGKILL lands mid-effect
    // even on a loaded machine. The resumed invocation recovers the journal,
    // declares exactly what it recovered, and skips the effect it can prove
    // it already performed.
    let report = run_demo(&[("ARENA_SCRIPTED_SLEEP_MS", "6000")], true);
    let episode = only_episode(&report);
    assert_eq!(episode.chaos_kills, 1);
    assert_eq!(episode.invocations, 2);
    assert_eq!(episode.outcome, Outcome::Completed);
    assert_eq!(episode.milestone_score, 1.0, "{:?}", episode.milestones);

    // Durability is *judged from the recording*, not assumed from survival:
    // resume-integrity was exercised and held, and the effect was performed
    // exactly once across the crash.
    let JournalVerdict::Judged {
        passed,
        report: oracles,
        metrics,
    } = &episode.journal
    else {
        panic!("journal must be judged: {:?}", episode.journal);
    };
    assert!(passed, "{oracles:?}");
    assert_eq!(oracles.check_status("resume-integrity"), Some("pass"));
    assert_eq!(metrics.resumes, 1);
    assert_eq!(metrics.side_effects, 1);
    assert_eq!(metrics.turns, 2);
}

#[test]
fn a_replayed_side_effect_after_resume_is_caught_not_blessed() {
    // Same crash, but the agent re-performs the effect it already performed
    // — the double-`git push` bug. The task still passes (the workspace
    // looks fine!); only the journal knows, and the runner reports the two
    // dimensions separately instead of blending the failure away.
    let report = run_demo(
        &[
            ("ARENA_SCRIPTED_SLEEP_MS", "6000"),
            ("ARENA_SCRIPTED_MISBEHAVE", "replay-effect"),
        ],
        true,
    );
    let episode = only_episode(&report);
    assert_eq!(episode.outcome, Outcome::Completed);
    assert_eq!(episode.milestone_score, 1.0);
    assert!(episode.task_passed());

    let JournalVerdict::Judged {
        passed,
        report: oracles,
        ..
    } = &episode.journal
    else {
        panic!("journal must be judged");
    };
    assert!(!passed);
    let failed: Vec<&str> = oracles
        .failures()
        .map(|check| check.name.as_str())
        .collect();
    assert_eq!(failed, vec!["effect-exactly-once"], "{oracles:?}");
    assert!(!report.all_green());
}

#[test]
fn a_frame_rendered_without_a_citation_is_caught() {
    let report = run_demo(&[("ARENA_SCRIPTED_MISBEHAVE", "drop-citation")], false);
    let episode = only_episode(&report);
    assert!(episode.task_passed());
    let JournalVerdict::Judged {
        report: oracles, ..
    } = &episode.journal
    else {
        panic!("journal must be judged");
    };
    let failed: Vec<&str> = oracles
        .failures()
        .map(|check| check.name.as_str())
        .collect();
    assert_eq!(failed, vec!["citation-at-use"]);
}

#[test]
fn amnesic_and_persistent_arms_run_independent_state() {
    let out = tempfile::tempdir().unwrap();
    let mut config = RunConfig::new(
        suite_dir(),
        manifest(out.path(), &[]),
        out.path().join("results"),
    );
    config.episodes = 2;
    config.arms = vec!["persistent".into(), "amnesic".into()];
    let report = run(&config).unwrap();
    assert_eq!(report.tasks[0].arms.len(), 2);
    for arm in &report.tasks[0].arms {
        assert_eq!(arm.episodes.len(), 2);
        for episode in &arm.episodes {
            assert!(episode.task_passed() && episode.loop_passed());
        }
    }
}
