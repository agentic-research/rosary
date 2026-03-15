# ADR-002: OTP Conductor — Elixir Supervision for Agent Lifecycle

## Status
Proposed

## Context

Rosary's `Reconciler` in `src/reconcile.rs` hand-rolls process supervision that OTP solves natively:

1. **`active: HashMap<String, AgentHandle>`** — manual process registry with no crash recovery
2. **`check_completed()` polling** — iterates every handle calling `try_wait()` every 30s
3. **`recover_stuck_beads()`** — on startup, resets `dispatched` beads because handles are lost on crash
4. **`wait_and_verify()` sub-loop** — second polling loop for `--once` mode with 30-min timeout
5. **Phase advancement via Vec** — collected during iteration, applied after, no transactional guarantee
6. **Timeout via elapsed check** — `handle.elapsed() > 10min` checked inside polling loop

All map to OTP primitives: `DynamicSupervisor`, `:DOWN` messages, `Process.send_after/3`, `GenServer` state machines.

**Immediate bug that prompted this**: `Stdio::piped()` + dropped `AgentHandle` = SIGPIPE kills dispatched agents. Fixed with `Stdio::null()`, but the root cause is Rust managing process lifecycles it shouldn't own.

**Reference**: OpenAI Symphony (github.com/openai/symphony-framework) — Elixir app that polls Linear, dispatches Codex agents via OTP supervision. Symphony reimplements everything; conductor is thin because rosary already has the domain logic.

## Decision

Create `conductor/` — a thin Elixir app that connects to `rsry serve --transport http` on `:8383/mcp`, gets dispatch decisions from rosary's triage, and supervises agent processes via OTP. Rosary keeps all domain logic.

## Architecture

### What stays in Rust (data plane)
- Triage scoring (`src/queue.rs`)
- Semantic dedup/clustering (`src/epic.rs`)
- Verification pipeline (`src/verify.rs`)
- Dolt persistence (`src/dolt.rs`)
- Linear sync (`src/linear.rs`, `src/linear_tracker.rs`)
- BDR decomposition (`crates/bdr/`)
- MCP tool implementations (`src/serve.rs`)
- Agent definitions + prompt building (`src/dispatch.rs`, `agents/`)
- Workspace isolation (`src/workspace.rs`)

### What moves to Elixir (control plane)
- Agent process supervision (replaces `active: HashMap`)
- Completion detection (`:DOWN` messages replace `check_completed()` polling)
- Timeout management (`Process.send_after` replaces elapsed checks)
- Phase advancement (synchronous GenServer state, no lost continuations)
- Crash recovery (supervision tree survives rsry restarts)
- Concurrency management (DynamicSupervisor max_children)

### Supervision Tree

```
Conductor.Application
  |
  +-- Conductor.RsryClient (GenServer)         # HTTP session to rsry /mcp
  |
  +-- Conductor.SessionRegistry (GenServer)     # ETS table of active agents
  |
  +-- Conductor.AgentSupervisor (DynamicSupervisor, strategy: :one_for_one)
  |     +-- Conductor.AgentWorker (bead-1)      # GenServer per bead
  |     +-- Conductor.AgentWorker (bead-2)
  |
  +-- Conductor.Orchestrator (GenServer)        # Periodic poll/triage/dispatch
```

### Transport: HTTP to rsry /mcp

Conductor connects to rsry's existing HTTP transport. No new Rust code needed.

- `POST /mcp` with JSON-RPC `tools/call` for each MCP tool
- `Mcp-Session-Id` header for session continuity
- HTTP gives crash independence — if rsry restarts, conductor reconnects

### AgentWorker: One GenServer Per Bead

```
State:
  bead_id, repo, issue_type
  pipeline: [agent_name, ...]     # from agent_pipeline()
  current_phase: integer
  port: Port                       # Erlang port wrapping agent process
  timeout_ref: reference           # Process.send_after timer
```

Lifecycle:
1. `init/1` — calls `rsry_dispatch` via RsryClient, opens Port to monitor PID
2. `{port, {:exit_status, code}}` — instant completion notification (no polling)
3. `:timeout` — kills agent after time limit
4. Phase advancement — on pass, checks `next_agent()`. If next phase exists, updates owner + reopens bead, all within single synchronous GenServer handler (no lost continuations)

### Phase Advancement (the closure problem solved)

Current Rust approach: collect `phase_advances: Vec<(String, String, String)>` during iteration, apply after. If crash between collection and persistence, continuation lost.

Elixir approach: phase advancement is part of the `AgentWorker` message handler. When `{port, {:exit_status, 0}}` arrives, the GenServer synchronously: verifies → advances owner → reopens bead → dispatches next phase. No window for lost state.

## Directory Structure

```
conductor/
  mix.exs
  config/
    config.exs                   # rsry endpoint, timeouts
    runtime.exs                  # env var overrides
  lib/
    conductor.ex
    conductor/
      application.ex             # OTP Application
      rsry_client.ex             # JSON-RPC client to rsry /mcp
      orchestrator.ex            # Poll/triage/dispatch loop
      agent_supervisor.ex        # DynamicSupervisor
      agent_worker.ex            # GenServer per bead
      pipeline.ex                # Phase state machine (pure functions)
      session_registry.ex        # ETS-backed active agent registry
  test/
    ...
```

## New rsry MCP Tools Needed

| Tool | Purpose |
|------|---------|
| `rsry_triage` | Return scored open beads without dispatching |
| `rsry_verify` | Run verification tiers on a work_dir |
| `rsry_pipeline_update` | Persist pipeline state (phase, retries, highest_tier) to Dolt |
| `rsry_bead_set_owner` | Expose `set_assignee` as MCP tool |

## Migration Path

1. **Observer**: Conductor connects, calls `rsry_scan`/`rsry_status`, logs what it would dispatch. Rust reconciler runs normally.
2. **Conductor dispatches**: Conductor calls `rsry_dispatch`, OTP supervises agents. `rsry run` stopped, `rsry serve` continues.
3. **Delete Rust lifecycle**: Remove `active` HashMap, `check_completed`, `recover_stuck_beads`, `wait_and_verify`, phase advancement collection from `reconcile.rs`.
4. **Full separation**: Add `rsry_triage` + `rsry_verify` tools. Conductor makes all orchestration decisions via MCP.

## Pipeline Durability

Pipeline state persisted to Dolt (not in-memory):
- `pipeline_phase: u8` — current index in agent_pipeline
- `pipeline_retries: u8` — retry count for current phase
- `highest_verify_tier: u8` — for revert detection

On restart, conductor queries Dolt for `status = dispatched` beads and reconstructs state. No `recover_stuck_beads()` hack needed.

## What to Borrow from Symphony

- DynamicSupervisor for agent workers
- Session registry via ETS
- Periodic orchestration via GenServer + `Process.send_after`
- Workspace isolation as per-worker resource

## What to Skip from Symphony

- Linear polling (rosary has webhooks)
- Triage/scoring (keep in Rust `queue.rs`)
- Codex client (rosary has Claude/Gemini/ACP providers)
- Prompt building (keep in Rust `dispatch.rs`)

## Consequences

**Positive**:
- Agent crashes are structurally isolated (BEAM, not Result handling)
- Completion detection is instant (`:DOWN`, not polled)
- Phase advancement is atomic (GenServer handler, not Vec + separate persistence)
- Orchestrator can crash and restart without killing agents
- ~500 lines of buggy Rust lifecycle code deleted

**Negative**:
- Elixir/Erlang dependency on a Rust project
- Two build systems (Cargo + Mix) in one repo
- Learning curve for contributors unfamiliar with OTP
- Operational complexity during migration (two systems coexisting)

## Relation to BDR

BDR decomposes ADRs into beads (top-down). The conductor dispatches agents to work on those beads (execution). BDR's accrete module (`crates/bdr/src/accrete.rs`) tracks decade/thread completion via `CompletionEvent` — the conductor's phase advancement feeds into this. When all beads in a thread complete their pipelines, accrete transitions the thread/decade status.

The conductor is the execution engine for the lattice BDR creates.
