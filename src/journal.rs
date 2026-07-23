//! A crash-safe writer for `contextgraph-trace` journals — the half of the
//! adapter contract a Rust agent should not have to re-derive.
//!
//! The writer appends one NDJSON line per event and flushes each line, so a
//! SIGKILL at any moment leaves a parseable prefix. On open it recovers the
//! prior recording (session id, last `seq`, highest turn, performed effect
//! ids), which is exactly the state an adapter needs to resume honestly:
//! continue the dense `seq`, emit `resume` with what it actually recovered,
//! and skip effects it can prove it already performed.

use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

use contextgraph_trace::{EventBody, TRACE_FORMAT, TraceEvent};

use crate::util::rfc3339_utc_now;

#[derive(Debug, thiserror::Error)]
pub enum JournalWriteError {
    #[error("journal {0}: {1}")]
    Io(String, std::io::Error),
    #[error("journal {path} line {line} is not a trace event: {reason}")]
    Corrupt {
        path: String,
        line: usize,
        reason: String,
    },
}

pub struct JournalWriter {
    file: File,
    path: String,
    session: String,
    next_seq: u64,
    open_turn: Option<u64>,
    highest_turn: u64,
    performed_effects: BTreeSet<String>,
    fresh: bool,
}

impl JournalWriter {
    /// Open (or create) a journal, recovering any prior recording. A partial
    /// last line — the signature of a kill mid-write — is tolerated and
    /// ignored; anything else unparseable is an error.
    pub fn open(path: &Path, session_if_fresh: &str) -> Result<Self, JournalWriteError> {
        let display = path.display().to_string();
        let existing = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(JournalWriteError::Io(display, error)),
        };

        let mut session = None;
        let mut next_seq = 1;
        let mut highest_turn = 0;
        let mut performed_effects = BTreeSet::new();
        let mut good_lines: Vec<&str> = Vec::new();
        let lines: Vec<&str> = existing.lines().filter(|l| !l.trim().is_empty()).collect();
        for (index, line) in lines.iter().enumerate() {
            match serde_json::from_str::<TraceEvent>(line) {
                Ok(event) => {
                    session.get_or_insert(event.session.clone());
                    next_seq = event.seq + 1;
                    if let Some(turn) = event.turn {
                        highest_turn = highest_turn.max(turn);
                    }
                    if let EventBody::SideEffect { effect_id, .. } = &event.body {
                        performed_effects.insert(effect_id.clone());
                    }
                    good_lines.push(line);
                }
                // A torn final line is what a kill mid-write looks like;
                // everything before it is the recording.
                Err(error) if index + 1 == lines.len() => {
                    let _ = error;
                    break;
                }
                Err(error) => {
                    return Err(JournalWriteError::Corrupt {
                        path: display,
                        line: index + 1,
                        reason: error.to_string(),
                    });
                }
            }
        }

        // Truncate a torn tail before appending: the oracle parser is
        // deliberately strict, so the recovered journal must contain whole
        // lines only — the torn line's events were never recovered, which the
        // `resume` event will declare honestly.
        if good_lines.len() != lines.len() || !(existing.is_empty() || existing.ends_with('\n')) {
            let mut clean = good_lines.join("\n");
            if !clean.is_empty() {
                clean.push('\n');
            }
            std::fs::write(path, clean)
                .map_err(|error| JournalWriteError::Io(display.clone(), error))?;
        }

        let fresh = session.is_none();
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|error| JournalWriteError::Io(display.clone(), error))?;
        Ok(Self {
            file,
            path: display,
            session: session.unwrap_or_else(|| session_if_fresh.to_string()),
            next_seq,
            open_turn: None,
            highest_turn,
            performed_effects,
            fresh,
        })
    }

    /// Whether this journal had no prior recording.
    pub fn is_fresh(&self) -> bool {
        self.fresh
    }

    /// The highest `seq` recovered from the prior recording (0 when fresh) —
    /// what an honest `resume` declares as `last_seq_seen`.
    pub fn last_recovered_seq(&self) -> u64 {
        self.next_seq - 1
    }

    /// Whether the recording proves this effect was already performed.
    pub fn effect_performed(&self, effect_id: &str) -> bool {
        self.performed_effects.contains(effect_id)
    }

    /// Append `session_start`. Call once, on a fresh journal.
    pub fn begin_session(
        &mut self,
        agent: &str,
        harness: &str,
        model: Option<String>,
    ) -> Result<(), JournalWriteError> {
        self.emit(
            None,
            EventBody::SessionStart {
                agent: agent.to_string(),
                harness: harness.to_string(),
                model,
                trace_format: Some(TRACE_FORMAT.to_string()),
            },
        )
    }

    /// Append `resume` declaring exactly what was recovered. A resume closes
    /// any turn the crash left open; resumed work starts a new turn.
    pub fn resume(&mut self) -> Result<(), JournalWriteError> {
        let last_seq_seen = self.last_recovered_seq();
        self.open_turn = None;
        self.emit(None, EventBody::Resume { last_seq_seen })
    }

    /// Open the next turn (numbers strictly increase across resumes).
    pub fn start_turn(&mut self) -> Result<u64, JournalWriteError> {
        let turn = self.highest_turn + 1;
        self.highest_turn = turn;
        self.emit(Some(turn), EventBody::TurnStart)?;
        self.open_turn = Some(turn);
        Ok(turn)
    }

    pub fn end_turn(&mut self) -> Result<(), JournalWriteError> {
        let turn = self.open_turn.take();
        self.emit(turn, EventBody::TurnEnd)
    }

    /// Append an in-turn event (prompt, model response, tool call/result,
    /// side effect, verify observation) stamped with the open turn.
    pub fn record(&mut self, body: EventBody) -> Result<(), JournalWriteError> {
        if let EventBody::SideEffect { effect_id, .. } = &body {
            self.performed_effects.insert(effect_id.clone());
        }
        self.emit(self.open_turn, body)
    }

    /// Append `session_end` and close out.
    pub fn end_session(
        &mut self,
        outcome: contextgraph_trace::SessionOutcome,
    ) -> Result<(), JournalWriteError> {
        self.emit(None, EventBody::SessionEnd { outcome })
    }

    fn emit(&mut self, turn: Option<u64>, body: EventBody) -> Result<(), JournalWriteError> {
        let event = TraceEvent {
            seq: self.next_seq,
            at: rfc3339_utc_now(),
            session: self.session.clone(),
            turn,
            body,
        };
        let line = serde_json::to_string(&event).expect("trace events serialize");
        writeln!(self.file, "{line}")
            .and_then(|()| self.file.flush())
            .map_err(|error| JournalWriteError::Io(self.path.clone(), error))?;
        self.next_seq += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use contextgraph_trace::{Journal, SessionOutcome, run_oracles};

    #[test]
    fn a_fresh_recording_judged_by_the_oracles_passes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.ndjson");
        let mut writer = JournalWriter::open(&path, "sess_test").unwrap();
        assert!(writer.is_fresh());
        writer.begin_session("t", "test/0", None).unwrap();
        writer.start_turn().unwrap();
        writer
            .record(EventBody::ModelResponse {
                tool_calls: vec!["call_1".into()],
            })
            .unwrap();
        writer
            .record(EventBody::ToolCall {
                call_id: "call_1".into(),
                tool: "write_file".into(),
            })
            .unwrap();
        writer
            .record(EventBody::SideEffect {
                effect_id: "write:x#1".into(),
                kind: "file_write".into(),
                call_id: Some("call_1".into()),
            })
            .unwrap();
        writer
            .record(EventBody::ToolResult {
                call_id: "call_1".into(),
                status: contextgraph_trace::ToolStatus::Ok,
            })
            .unwrap();
        writer.end_turn().unwrap();
        writer.end_session(SessionOutcome::Completed).unwrap();

        let journal = Journal::from_ndjson(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let report = run_oracles(&journal);
        assert!(report.passed(), "{:?}", report);
    }

    #[test]
    fn reopening_recovers_seq_session_turns_and_effects_for_an_honest_resume() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.ndjson");
        {
            let mut writer = JournalWriter::open(&path, "sess_test").unwrap();
            writer.begin_session("t", "test/0", None).unwrap();
            writer.start_turn().unwrap();
            writer
                .record(EventBody::SideEffect {
                    effect_id: "write:x#1".into(),
                    kind: "file_write".into(),
                    call_id: None,
                })
                .unwrap();
            // Killed here: no turn_end, no session_end.
        }
        let mut writer = JournalWriter::open(&path, "ignored").unwrap();
        assert!(!writer.is_fresh());
        assert_eq!(writer.last_recovered_seq(), 3);
        assert!(writer.effect_performed("write:x#1"));
        writer.resume().unwrap();
        let turn = writer.start_turn().unwrap();
        assert_eq!(turn, 2, "turn numbers continue across a resume");
        writer.end_turn().unwrap();
        writer.end_session(SessionOutcome::Completed).unwrap();

        let journal = Journal::from_ndjson(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let report = run_oracles(&journal);
        assert!(report.passed(), "{:?}", report);
    }

    #[test]
    fn a_torn_final_line_is_tolerated_as_the_signature_of_a_kill() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.ndjson");
        {
            let mut writer = JournalWriter::open(&path, "sess_test").unwrap();
            writer.begin_session("t", "test/0", None).unwrap();
        }
        // Simulate a kill mid-write.
        use std::io::Write as _;
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        write!(file, "{{\"seq\":2,\"at\":\"2026-").unwrap();
        drop(file);

        let mut writer = JournalWriter::open(&path, "ignored").unwrap();
        assert_eq!(writer.last_recovered_seq(), 1);
        // The torn tail was truncated, so finishing the recording yields a
        // journal the strict oracle parser accepts.
        writer.resume().unwrap();
        writer.end_session(SessionOutcome::Completed).unwrap();
        let journal = Journal::from_ndjson(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(run_oracles(&journal).passed());
    }
}
