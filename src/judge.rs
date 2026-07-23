//! Journal judgement: parse the episode's recording, run the
//! `contextgraph-trace` oracles over it, and extract loop metrics.
//!
//! A missing or unparseable journal is a verdict of its own, never a silent
//! pass — an adapter that "forgot" to record is indistinguishable from one
//! hiding a broken loop, and gets treated accordingly.

use std::path::Path;

use contextgraph_trace::{EventBody, Journal, TraceReport, run_oracles};
use serde::{Deserialize, Serialize};

/// Loop metrics extracted from the journal itself — turns, prompts, declared
/// context tokens. Deliberately sourced from the recording rather than any
/// adapter self-report.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct JournalMetrics {
    pub events: usize,
    pub turns: usize,
    pub prompts: usize,
    pub resumes: usize,
    pub side_effects: usize,
    /// Sum of `declared_total_tokens` across every prompt assembly — the
    /// episode's declared context spend.
    pub declared_prompt_tokens: u64,
}

/// The journal's verdict for one episode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum JournalVerdict {
    /// The journal parsed; the oracle report and metrics are attached.
    /// `passed` mirrors `report.passed()` for one-glance reading.
    Judged {
        passed: bool,
        report: TraceReport,
        metrics: JournalMetrics,
    },
    /// The adapter never wrote a journal. Loop integrity is unverifiable,
    /// which is a failure of the contract, not a skip.
    Missing,
    /// The journal exists but does not parse — a broken recorder.
    Invalid { reason: String },
}

impl JournalVerdict {
    /// Whether loop integrity was demonstrated. Only a parsed journal with a
    /// green oracle report qualifies.
    pub fn loop_passed(&self) -> bool {
        matches!(self, JournalVerdict::Judged { passed: true, .. })
    }
}

/// Name-based lookup over a trace oracle report, for assertions and
/// reporting ("did `resume-integrity` actually run?").
pub trait CheckLookup {
    /// The wire status of the named check: `"pass"`, `"fail"`, `"skipped"`,
    /// or `None` when the report does not contain it.
    fn check_status(&self, name: &str) -> Option<&'static str>;
}

impl CheckLookup for TraceReport {
    fn check_status(&self, name: &str) -> Option<&'static str> {
        use contextgraph_trace::CheckStatus;
        self.checks
            .iter()
            .find(|check| check.name == name)
            .map(|check| match check.status {
                CheckStatus::Pass => "pass",
                CheckStatus::Fail => "fail",
                CheckStatus::Skipped => "skipped",
            })
    }
}

/// Judge one journal file.
pub fn judge_journal(path: &Path) -> JournalVerdict {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return JournalVerdict::Missing;
        }
        Err(error) => {
            return JournalVerdict::Invalid {
                reason: error.to_string(),
            };
        }
    };
    let journal = match Journal::from_ndjson(&raw) {
        Ok(journal) => journal,
        Err(error) => {
            return JournalVerdict::Invalid {
                reason: error.to_string(),
            };
        }
    };
    let report = run_oracles(&journal);
    let metrics = extract_metrics(&journal);
    JournalVerdict::Judged {
        passed: report.passed(),
        report,
        metrics,
    }
}

fn extract_metrics(journal: &Journal) -> JournalMetrics {
    let mut metrics = JournalMetrics {
        events: journal.events.len(),
        ..JournalMetrics::default()
    };
    for event in &journal.events {
        match &event.body {
            EventBody::TurnStart => metrics.turns += 1,
            EventBody::Resume { .. } => metrics.resumes += 1,
            EventBody::SideEffect { .. } => metrics.side_effects += 1,
            EventBody::PromptAssembled {
                declared_total_tokens,
                ..
            } => {
                metrics.prompts += 1;
                metrics.declared_prompt_tokens += declared_total_tokens;
            }
            _ => {}
        }
    }
    metrics
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_journal_is_a_verdict_not_a_skip() {
        let dir = tempfile::tempdir().unwrap();
        let verdict = judge_journal(&dir.path().join("nope.ndjson"));
        assert_eq!(verdict, JournalVerdict::Missing);
        assert!(!verdict.loop_passed());
    }

    #[test]
    fn garbage_is_an_invalid_verdict_naming_the_reason() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.ndjson");
        std::fs::write(&path, "not json\n").unwrap();
        match judge_journal(&path) {
            JournalVerdict::Invalid { reason } => assert!(reason.contains("line 1")),
            other => panic!("expected invalid, got {other:?}"),
        }
    }
}
