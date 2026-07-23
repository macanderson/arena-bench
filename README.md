# arena-bench

[![CI](https://github.com/macanderson/arena-bench/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/macanderson/arena-bench/actions/workflows/ci.yml)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

**The benchmark for the part of an agent no outcome test can see: the loop.**
Sibling to [Arena](https://github.com/macanderson/arena) (head-to-head outcome
benchmarks) — arena-bench runs one agent on seeded tasks, **SIGKILLs it
mid-episode on purpose**, re-invokes it, and judges the *recording* of its
execution with the
[Context Graph Protocol](https://github.com/macanderson/context-graph-protocol)'s
trace oracles.

An agent can produce a perfect diff while having:

- cited context it was told was stale three turns earlier,
- blown the token budget it announced for its own prompt,
- executed a tool call its model never requested,
- **replayed a side effect after a crash-resume** (the double-`git push` bug),
- resumed blind to work its own durable record proves it already did.

Outcome benchmarks bless all five. arena-bench exists to catch them.

## How it works

Every episode, the adapter appends an NDJSON **journal** of
[`contextgraph-trace`](https://github.com/macanderson/context-graph-protocol/blob/main/docs/sketches/host-trace.md)
events as it works: turns, prompt assemblies (context-frame identities +
declared token costs — never frame bodies), model-requested tool-call ids,
tool results, side effects with intended-once ids, verify observations,
crashes and resumes. After the episode, eight **replay oracles** hold the
recording to the loop invariants (`sequence-integrity`, `turn-loop-pairing`,
`assembly-budget-honesty`, `staleness-at-use`, `citation-at-use`,
`deterministic-composition`, `effect-exactly-once`, `resume-integrity`), and
held-out milestone verifiers grade the task.

Three dimensions, **never blended** into one score (a run that solves the task
while failing `effect-exactly-once` is exactly the run this tool exists to
expose):

| Dimension | Judged by |
|---|---|
| **task** | outcome (timeout takes precedence) + weighted, held-out milestones |
| **loop** | the trace oracles, replayed over the journal |
| **cost** | wall clock + journal-declared prompt tokens |

Fairness rules inherited from Arena: argv arrays (no shell), `verify/` is
never on disk while the agent runs and anything planted at its staging path is
deleted first, `agent-error` is reported separately and never counted as the
agent losing, and the chaos schedule is deterministic per seed.

## Quick start

```bash
git clone https://github.com/macanderson/arena-bench
cd arena-bench && cargo build --bins

# The bundled scripted agent, clean:
cargo run -- run --suite suites/demo --adapter adapters/scripted.toml --strict

# Kill it mid-side-effect, watch it resume honestly:
ARENA_SCRIPTED_SLEEP_MS=6000 \
cargo run -- run --suite suites/demo --adapter adapters/scripted.toml --chaos --strict

# Watch the runner CATCH a broken loop (task still passes; the journal convicts):
ARENA_SCRIPTED_MISBEHAVE=replay-effect ARENA_SCRIPTED_SLEEP_MS=6000 \
cargo run -- run --suite suites/demo --adapter adapters/scripted.toml --chaos; echo "exit $?"

# Judge any journal standalone:
cargo run -- judge path/to/journal.ndjson
```

Every run writes `results/<run-id>/` with `run.json`, `report.md`, and per
episode: the journal, `episode.json`, the workspace, and full agent
stdout/stderr.

## The adapter contract

Any agent joins by being an executable (described by a TOML manifest) that
honors four flags — deliberately in the spirit of Harbor's and Arena's
adapters:

```toml
[adapter]
name = "my-agent"
command = ["my-agent", "arena"]   # argv array, never a shell string

[adapter.env]                     # optional
MY_AGENT_NONINTERACTIVE = "1"
```

Per invocation the runner appends (and mirrors into `ARENA_*` env vars):

```text
--task-dir <dir>    the episode workspace; the prompt is in TASK.md
--journal <file>    append contextgraph-trace NDJSON events here
--state-dir <dir>   persists across episodes — the agent's memory
--resume            present when re-invoked after a kill
```

Contract in one paragraph: work on `TASK.md`, append journal events **as you
go** (the journal must survive a SIGKILL at any byte — the bundled
`JournalWriter` flushes per line and truncates torn tails on recovery), exit 0
when done. On `--resume`, continue the same session: recover the journal, emit
a `resume` event declaring exactly what you recovered, keep `seq` dense, and
don't re-perform effects you can prove you already performed — the
`effect-exactly-once` oracle is watching.

`src/bin/arena-scripted-agent.rs` is the complete reference implementation,
including crash recovery and the misbehave modes CI uses to prove the runner
catches a broken loop.

## Task packs

```text
suites/<name>/<task-id>/
  task.toml       # prompt, timeout, milestones, chaos window
  workspace/      # fixture seeded into a fresh dir per episode
  verify/         # held-out; staged to .arena-verify/ only after the agent exits
```

```toml
[task]
id = "greet-cli"
prompt = "Create an executable greet.sh …"
timeout_secs = 120

[chaos]                      # only under --chaos
kill_window_secs = [1.5, 2.5]
max_kills = 1

[[milestones]]
name = "held-out-tests"
weight = 3
verify = ["sh", ".arena-verify/run.sh"]
```

## Long horizons and self-improvement

- `--episodes N` runs repeated exposures of each task; `--state-dir` persists
  between them, so an agent that learns gets to show it.
- `--arms persistent,amnesic` runs a memory-ablation baseline: the `amnesic`
  arm wipes state before every episode. The difference between the arms'
  learning curves — episode-over-episode success, wall clock, journal-declared
  tokens — is the self-improvement signal, reported per episode and never
  averaged away.

## Relationship to the family

- **[context-graph-protocol](https://github.com/macanderson/context-graph-protocol)**
  owns the measurable surface: the journal vocabulary and the oracles
  (`contextgraph-trace`). arena-bench is a consumer; any other runner can hold
  agents to the same invariants.
- **[Arena](https://github.com/macanderson/arena)** answers "which agent wins
  on outcomes, with real statistics." arena-bench answers "is this agent's
  loop telling the truth, and does it survive being killed." Run both.
- **[Stella](https://github.com/macanderson/stella)** ships the first
  first-class port (`stella arena`, `adapters/stella.toml`).

## License

Dual-licensed under [MIT](./LICENSE-MIT) or
[Apache-2.0](./LICENSE-APACHE), at your option.
