//! The scripted example agent — a deterministic adapter that completes the
//! demo `greet-cli` task while emitting an honest journal. It exists for the
//! same reason the Context Graph Protocol ships `contextgraph-example-docs`:
//! the end-to-end tests need a real adapter to run, kill, resume, and judge,
//! and adapter authors need a complete reference implementation of the
//! contract (including crash recovery, which is the part everyone gets
//! wrong).
//!
//! Misbehaviour modes (`ARENA_SCRIPTED_MISBEHAVE`) exist to prove the runner
//! *catches* a broken loop, in the `--misbehave` tradition:
//!
//! - `replay-effect` — on resume, re-performs a side effect it already
//!   performed (the double-`git push` bug); `effect-exactly-once` fails.
//! - `drop-citation` — renders the task frame without a citation label;
//!   `citation-at-use` fails.
//!
//! `ARENA_SCRIPTED_SLEEP_MS` inserts a pause between the side effect and its
//! tool result on the first invocation, giving a chaos kill a deterministic
//! window to land in.

use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use arena_bench::{AdapterArgs, JournalWriter};
use contextgraph_trace::{EventBody, RenderedFrame, SessionOutcome, ToolStatus};
use contextgraph_types::{FrameId, Representation};

const EFFECT_WRITE_GREETER: &str = "write:greet.sh#1";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("arena-scripted-agent: {error}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<(), String> {
    let args = AdapterArgs::from_env_and_args(std::env::args().skip(1))
        .map_err(|error| format!("adapter contract: {error}"))?;
    let misbehave = std::env::var("ARENA_SCRIPTED_MISBEHAVE").unwrap_or_default();
    let sleep_ms: u64 = std::env::var("ARENA_SCRIPTED_SLEEP_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);

    let session = format!("arena-scripted-{:x}", epoch_millis());
    let mut journal =
        JournalWriter::open(&args.journal, &session).map_err(|error| error.to_string())?;

    let fresh = journal.is_fresh();
    if fresh {
        journal
            .begin_session("scripted", "arena-scripted-agent/0.1", None)
            .map_err(|error| error.to_string())?;
    } else if args.resume {
        journal.resume().map_err(|error| error.to_string())?;
    } else {
        return Err("journal already has a recording but --resume was not given".into());
    }

    let turn = journal.start_turn().map_err(|error| error.to_string())?;
    let call_id = format!("call_{turn}");

    // The one frame this agent reasons over: TASK.md, digested so the
    // identity names its exact bytes. The digest scheme is opaque to the
    // trace vocabulary; fnv1a is honest about what it is.
    let task_md = std::fs::read_to_string(args.task_dir.join("TASK.md"))
        .map_err(|error| format!("reading TASK.md: {error}"))?;
    let token_cost = (task_md.len().div_ceil(4)) as u32;
    let citation_label = if misbehave == "drop-citation" {
        None
    } else {
        Some("TASK.md".to_string())
    };
    let frame = RenderedFrame {
        frame: FrameId::new(
            "workspace",
            "task-md",
            Some(format!("fnv1a:{:016x}", fnv1a(task_md.as_bytes()))),
        ),
        representation: Representation::Full,
        token_cost,
        citation_label,
    };
    journal
        .record(EventBody::PromptAssembled {
            budget_tokens: 4096,
            declared_total_tokens: u64::from(token_cost),
            composition_digest: Some(format!("fnv1a:{:016x}", fnv1a(task_md.as_bytes()))),
            frames: vec![frame],
        })
        .map_err(|error| error.to_string())?;
    journal
        .record(EventBody::ModelResponse {
            tool_calls: vec![call_id.clone()],
        })
        .map_err(|error| error.to_string())?;
    journal
        .record(EventBody::ToolCall {
            call_id: call_id.clone(),
            tool: "write_file".to_string(),
        })
        .map_err(|error| error.to_string())?;

    // The work. An honest agent checks its own recording before re-performing
    // an intended-once effect; the `replay-effect` mode is precisely that
    // check deleted.
    let already_performed = journal.effect_performed(EFFECT_WRITE_GREETER);
    if !already_performed || misbehave == "replay-effect" {
        write_greeter(&args)?;
        journal
            .record(EventBody::SideEffect {
                effect_id: EFFECT_WRITE_GREETER.to_string(),
                kind: "file_write".to_string(),
                call_id: Some(call_id.clone()),
            })
            .map_err(|error| error.to_string())?;
    }

    // The chaos window: on the first invocation, linger between performing
    // the effect and resolving the call — exactly where a crash hurts.
    if fresh && sleep_ms > 0 {
        std::thread::sleep(Duration::from_millis(sleep_ms));
    }

    journal
        .record(EventBody::ToolResult {
            call_id,
            status: ToolStatus::Ok,
        })
        .map_err(|error| error.to_string())?;
    journal.end_turn().map_err(|error| error.to_string())?;
    journal
        .end_session(SessionOutcome::Completed)
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn write_greeter(args: &AdapterArgs) -> Result<(), String> {
    let path = args.task_dir.join("greet.sh");
    std::fs::write(&path, "#!/bin/sh\necho \"hello, $1\"\n")
        .map_err(|error| format!("writing greet.sh: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .map_err(|error| format!("chmod greet.sh: {error}"))?;
    }
    Ok(())
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}

fn epoch_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}
