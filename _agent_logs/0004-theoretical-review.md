# Theoretical Review: ADR-004 Dual State Machine Architecture

**Analyst**: Theoretical Foundations Analyst
**Date**: 2026-03-15
**Subject**: ADR-004 Dual State Machine -- Bead Flow vs Agent Flow
**Scope**: Formal state machine analysis, consistency guarantees, failure mode enumeration, recovery semantics

---

## Executive Summary

ADR-004 proposes decomposing rosary's orchestration into two state machines: a user-facing bead flow and an infrastructure agent flow. The decomposition is **architecturally sound and pragmatically well-motivated**, but it is not formally "two separate state machines" -- it is a product state machine with a projection. This distinction matters for consistency reasoning. The ADR's touch-point model is a correct engineering approximation of what is formally a fiber bundle structure, but the approximation has specific failure modes at the touch points that need explicit mitigation. The "backend wins" conflict resolution is necessary but not sufficient. The recovery semantics have a gap around partially-completed pipeline phases.

**Overall assessment**: Strong architecture. The core insight (users see beads, not agents) is correct and well-applied. The implementation plan (rsry as shared persistence, conductor as execution owner) follows established patterns from distributed systems (CQRS, event sourcing lite). Five specific issues need resolution, ranked by severity below.

---

## 1. State Machine Composition: Product Space, Not Disjoint Union

### The Claim

ADR-004 states: "Bead flow and agent flow are separate state machines."

### The Formal Analysis

Let B be the bead state machine with states S_B = {backlog, open, queued, dispatched, verifying, done, rejected, blocked, stale} and transition relation T_B (as defined in `bead.rs:valid_transitions()`).

Let A be the agent flow state machine with states S_A = {idle, spawn, running, exited(success|failure|timeout), verify, advance, retry, deadletter} (reconstructed from the ADR and `reconcile.rs`).

If these were truly **separate** (in the formal sense of a disjoint union or coproduct of automata), they would evolve independently. But the ADR explicitly defines four coupling points:

1. dispatched (B) <=> spawn (A)
2. done (B) <=> advance-terminal (A)
3. blocked (B) <=> deadletter (A)
4. rejected (B) <=> verify-fail (A)

These are not mere "touch points" -- they are **synchronization constraints** that create a joint state space. The actual system lives in a subset of S_B x S_A, with invariants like:

- If A = running, then B must be in {dispatched, verifying}
- If A = idle, then B must be in {backlog, open, queued, done, rejected, blocked, stale}
- If B = done, then A must be in {idle} (pipeline cleared)

This is a **product state machine with invariant constraints** -- formally, a subautomaton of B x A defined by the synchronization predicate.

### Why This Matters

The distinction is not academic pedantry. When you model two systems as "separate with touch points," you implicitly assume that violations at the touch points are the only consistency risks. But in a product state machine, the invariant constraints create additional failure modes:

**Invariant violation example**: Agent crashes (A transitions to exited) but the MCP call to transition B from dispatched to open fails. Now B = dispatched, A = exited(failure). No live agent exists, but the bead appears active. The system is in an unreachable state of the intended product automaton.

The ADR's `recover_stuck_beads()` in `reconcile.rs` handles this exact case for the Rust reconciler: on startup, any bead at `dispatched` with no live agent gets reset to `open`. This is correct. The question is whether the conductor implements equivalent invariant recovery.

### Recommendation

The ADR should acknowledge the product structure explicitly. Replace "separate state machines connected at touch points" with "two state machines forming a constrained product, where the constraint invariants are enforced by the persistence layer." This change in framing clarifies what the system must guarantee: not just correct transitions at touch points, but maintenance of the cross-machine invariants at all times.

**Strength to preserve**: The *projection* model is exactly right for the user-facing API. Users see the B-projection of the product state. This is a clean design -- analogous to how a database view hides join complexity. The ADR's instinct here is sound.

---

## 2. The User/Bead Boundary: Clean Decomposition with One Leak

### The Claim

"Users touch beads, not agents. User <-> backend <-> beads <-> orchestrator."

### Analysis

This is an excellent decomposition and one of the strongest aspects of the ADR. Let me trace the information flow:

```
User creates bead (Linear/MCP/CLI)
  -> per-repo Dolt (.beads/)
  -> rsry scan discovers it
  -> rsry triage scores it
  -> conductor receives dispatchable list
  -> conductor spawns agent (A = running)
  -> conductor writes PipelineState to backend Dolt
  -> bead status -> dispatched (B transition)
  -> agent works...
  -> agent exits
  -> verification runs
  -> bead status -> done/rejected/blocked (B transition)
  -> Linear sync updates visible status
  -> user sees result
```

The agent flow (pipeline phase, retry count, backoff timer) is correctly invisible at the bead level. The user never needs to know that their bead went through dev-agent phase 0, then staging-agent phase 1.

### The Leak: "Why Is My Bead Stuck?"

There is exactly one point where agent flow state becomes user-visible, and the ADR implicitly acknowledges it but does not resolve it: **debugging stuck beads**.

Consider: a bead has been `dispatched` for 45 minutes. The user asks "why is my bead stuck?" To answer this, someone needs to inspect agent flow state:

- What pipeline phase is it in?
- Has the agent crashed? (dispatch record with no completion)
- Is it in backoff? (backoff_until in the future)
- How many retries have been attempted?

The ADR proposes `rsry_pipeline_query` and `rsry_dispatch_history` as MCP tools, labeling them "debugging, analytics." This is the right call -- they are escape hatches for operators, not primary user interfaces.

**But the ADR does not specify how this debugging information surfaces to users.** A Linear comment? A bead event? A status field?

### Recommendation

Define a **diagnostic comment** mechanism: when a bead has been `dispatched` for longer than a configurable threshold (e.g., 2x the median dispatch duration), the conductor should automatically append a bead comment with a human-readable summary of agent flow state:

```
[conductor] Pipeline status for rsry-001:
  Phase: 1/4 (staging-agent)
  Attempt: 3 of 5
  Status: backoff until 14:30 UTC (last failure: test tier)
```

This keeps the bead-as-interface model intact while providing the escape hatch for the one scenario where users need to peek behind the curtain. It respects the projection boundary -- the user sees a comment on their bead, not raw pipeline state.

**Strength to preserve**: The core insight that bead state is the user-facing contract is exactly correct. Do not weaken this by exposing pipeline_phase as a bead field or similar. The comment mechanism maintains the separation.

---

## 3. Consistency at Touch Points: Failure Mode Enumeration

### The Architecture

Two databases, no distributed transaction:
- Bead state: per-repo Dolt (`.beads/`)
- Agent flow state: backend Dolt (`~/.rsry/dolt/rosary/`)

The conductor writes to both via MCP (HTTP JSON-RPC to rsry). rsry translates to SQL against the appropriate Dolt instance.

### Failure Modes

I identify **six** distinct failure modes at the touch points, ordered by severity:

#### F1: Split-brain on dispatch (CRITICAL)

**Scenario**: Conductor spawns agent, writes PipelineState to backend (success), then calls rsry_bead_update to set status=dispatched (fails -- rsry restart, network blip, Dolt timeout).

**Result**: Backend says "pipeline active for rsry-001." Per-repo Dolt says "rsry-001 is open." The conductor thinks it is managing an active dispatch. The Rust reconciler (or another conductor instance) sees an open bead and may dispatch a second agent.

**Current mitigation**: None explicit in the ADR. The "backend wins" rule would say the pipeline is active, but the per-repo Dolt is a separate authority that the scan phase reads.

**Fix**: The conductor must treat the bead status update as the **commit point** of a dispatch. The sequence must be: (1) set bead status to dispatched, (2) only then write PipelineState and spawn agent. If step 1 fails, abort. If step 2 fails after step 1, the bead is dispatched with no pipeline -- the recovery path handles this (see F4).

More precisely, the bead state transition to `dispatched` is the linearization point. Agent flow state is derived. This inverts the ADR's current implied ordering (spawn first, record after).

#### F2: Phantom pipeline on completion (HIGH)

**Scenario**: Agent completes successfully. Conductor writes bead status=done (success). Conductor calls clear_pipeline on backend (fails).

**Result**: Stale PipelineState row persists. On restart, conductor reads it, finds no live agent, attempts to "resume" a completed bead.

**Current mitigation**: Step 2-4 of the restart recovery (check if alive, decide resume vs retry). But the bead is already `done` -- resuming makes no sense.

**Fix**: Recovery must cross-reference pipeline state against bead state. If bead is `done` and pipeline exists, clear the pipeline unconditionally. Add an invariant check: `if bead.state in {done, blocked, rejected} then pipeline must be empty`.

#### F3: Lost completion event (HIGH)

**Scenario**: Agent exits. Conductor detects exit. Conductor calls rsry_bead_update to set status but rsry is down.

**Result**: Bead stays `dispatched` indefinitely. Agent flow shows `exited(success)` but bead never advances.

**Current mitigation**: `recover_stuck_beads()` in `reconcile.rs` resets dispatched beads on startup. But if the conductor is the execution owner (not the Rust reconciler), who runs this recovery?

**Fix**: The conductor needs its own `recover_stuck_beads` equivalent that runs periodically (not just on startup). A periodic reconciliation sweep: for every pipeline in `exited` state where the bead is still `dispatched`, retry the bead state transition. This is the conductor's version of the k8s controller pattern's "eventual consistency through re-reconciliation."

#### F4: Orphaned bead at dispatched (MEDIUM)

**Scenario**: Bead transitions to `dispatched` but conductor crashes before spawning agent or writing pipeline state.

**Result**: Bead is `dispatched` with no pipeline record and no agent process.

**Current mitigation**: If the Rust reconciler runs `recover_stuck_beads`, it resets to open. But in the conductor-primary model, the Rust reconciler may not be running this logic.

**Fix**: The conductor's startup recovery should also scan for beads at `dispatched` that have no corresponding pipeline record. These are F1/F4 orphans and should be reset to `open`.

#### F5: Pipeline phase drift (MEDIUM)

**Scenario**: Conductor advances pipeline to phase 2 (writes to backend), but the bead owner update (set_assignee to staging-agent) and status reset (open) fail.

**Result**: Backend says phase 2, bead still shows previous agent. Next dispatch may use wrong agent definition.

**Fix**: The pipeline phase and bead owner must advance atomically from the user's perspective. Since they are in different databases, use the bead update as the commit point: only advance pipeline_phase in backend after bead owner is confirmed updated. On failure, retry the bead update.

#### F6: Stale generation hash (LOW)

**Scenario**: Bead content changes in Dolt while an agent is running. Generation hash in PipelineState no longer matches.

**Result**: When the agent completes, the pipeline's `last_generation` is stale. This is actually handled correctly in the current code -- generation is checked at triage time, not completion time. But with the conductor holding pipeline state, a stale generation could cause the conductor to re-dispatch unnecessarily.

**Fix**: Document that generation is a triage-time optimization, not a completion-time invariant. The conductor should re-read generation on completion, not rely on its cached value.

### Summary Table

| ID | Severity | Touch Point | Root Cause | Fix |
|----|----------|-------------|------------|-----|
| F1 | Critical | dispatch | Write ordering | Bead update first, pipeline after |
| F2 | High | completion | Incomplete cleanup | Cross-ref pipeline vs bead on recovery |
| F3 | High | completion | MCP failure | Periodic reconciliation sweep |
| F4 | Medium | dispatch | Crash before spawn | Startup scan for orphaned dispatched beads |
| F5 | Medium | phase advance | Partial multi-db update | Bead update as commit point |
| F6 | Low | generation | Stale cache | Document as triage-time only |

---

## 4. "Backend Wins" Conflict Resolution: Necessary But Not Sufficient

### The Claim

"Conductor treats rsry as authoritative; on conflict, re-read from rsry. The conductor's in-memory state is the 'hot' copy; backend is the 'durable' copy. If they diverge, backend wins."

### Analysis

This rule is sound for one direction of divergence: when the conductor has stale in-memory state and the backend has been updated (e.g., by a Linear webhook, a human MCP call, or another conductor instance). Re-reading from rsry is correct.

But "backend wins" is **undefined for the other direction**: when the conductor has advanced beyond what the backend records. This is the scenario in your question: conductor has advanced to pipeline phase 3, but the backend still shows phase 1 (MCP writes for phase 2 and 3 failed).

Under "backend wins," the conductor would discard its phase 3 knowledge and reset to phase 1. This means:

1. The work done in phases 2 and 3 is **orphaned**. The agent processes that ran those phases may have committed code, opened PRs, etc.
2. The pipeline would **re-execute phases 2 and 3**, potentially on work already completed. This is wasteful but not catastrophic if agents are idempotent (the ADR's "one bead, one LLM call" model helps here).
3. If agents are NOT idempotent (e.g., they create PRs), re-execution creates duplicates.

### The Deeper Issue

"Backend wins" is a **last-writer-wins** policy with the backend as the designated writer. But the conductor is also a writer (via MCP). The conflict resolution should be:

**For bead state**: Backend wins. This is correct because bead state can be modified by external actors (users, Linear webhooks, other MCP consumers). The conductor is one of many writers.

**For agent flow state**: Conductor wins, with backend as durable log. This is correct because the conductor is the **sole writer** of pipeline state. If the conductor says phase 3 and the backend says phase 1, the conductor is ahead (writes were lost), not behind. The conductor should **re-apply** its state to the backend, not revert.

### Recommendation

Split the conflict resolution rule:

1. **Bead flow conflicts**: Backend (per-repo Dolt) is authoritative. Conductor re-reads on conflict.
2. **Agent flow conflicts**: Conductor is authoritative for in-memory state. On divergence, conductor **overwrites** backend. On restart (no in-memory state), backend is authoritative by default (it is the best available record).

This creates a clear ownership model: bead state has multiple writers (backend is arbiter), agent flow state has one writer (conductor, with backend as durable store).

**Strength to preserve**: The instinct to have a single authority is correct. The refinement is to recognize that different state has different ownership. The ADR already implies this ("rsry owns data, conductor owns execution") -- the conflict resolution rule should match.

---

## 5. Recovery Semantics: The Partial-Completion Gap

### The Claim

On conductor restart:
1. Read `list_active_pipelines()` from backend
2. For each, check if the agent process is still alive (PID check)
3. If alive, re-attach supervision
4. If dead, decide: resume (next phase) or retry (same phase)

### Analysis of Each Step

**Step 1**: Sound, assuming the backend is durable and the schema is correct. The `pipeline_state` table has the right fields. One issue: `list_active_pipelines()` returns ALL rows in the table (see `store_dolt.rs:432-443`). There is no `active` flag or `completed_at` timestamp. Cleared pipelines are DELETEd. This means any pipeline not explicitly cleared is "active." After a crash where clear_pipeline was not called, these will all appear active. This is correct behavior for recovery purposes.

**Step 2**: PID check. The ADR says "check if the agent process is still alive." But agent processes are spawned by the conductor as child processes. After conductor restart, the conductor's child process table is empty. The previous conductor's children have been orphaned (re-parented to PID 1 on Unix, or terminated by SIGPIPE -- the commit `fb04136` specifically addresses SIGPIPE).

PID checking after restart is **unreliable**:
- The PID may have been recycled by the OS.
- The process at that PID may not be the original agent.
- On macOS (this system is Darwin), PID recycling is relatively fast.
- The `DispatchRecord` stores `work_dir` but not PID. The `PipelineState` does not store PID at all.

**Step 3**: Re-attach supervision. Even if you could identify the process, OTP supervision requires the process to be an Erlang/Elixir process within the BEAM VM, or an external port/command managed by a Port. You cannot "re-attach" a previously-orphaned Unix process to an OTP supervisor. The supervisor-child relationship is established at spawn time.

**Step 4**: Resume vs retry. This is the key decision, and it depends on what "partially completed" means:

- **Agent was running (exited uncleanly)**: The work directory may contain partial changes. The git worktree may have uncommitted edits. Retrying from the same phase is correct -- the agent starts fresh in a new worktree (the workspace provisioning creates a new one).

- **Agent completed but verification failed**: Pipeline state shows the last successful verification tier. Retrying at the same phase with incremented retry count is correct.

- **Agent completed, verification passed, phase advance was in progress**: This is the gap. The pipeline shows phase N, but the agent for phase N actually completed. The conductor was about to advance to phase N+1 when it crashed. On recovery, it sees phase N and retries it -- re-executing completed work.

### The Fundamental Problem

The recovery model treats pipeline_phase as "the phase currently being executed." But after an agent completes a phase, there is a window between "phase N complete" and "phase advanced to N+1" where the pipeline_phase is ambiguous. Is it "executing N" or "completed N, about to start N+1"?

### Recommendation

Add a **phase_status** field to PipelineState:

```rust
pub struct PipelineState {
    // ... existing fields ...
    /// Status within the current phase: pending, executing, completed, failed
    pub phase_status: String,
}
```

The lifecycle becomes:
1. `phase_status = pending` -- phase selected, not yet dispatched
2. `phase_status = executing` -- agent spawned
3. `phase_status = completed` -- agent exited, verification passed
4. `phase_status = failed` -- agent exited, verification failed

On recovery:
- `executing` + dead process = retry same phase (agent crashed)
- `completed` + no next phase = bead is done, clear pipeline
- `completed` + next phase exists = advance to next phase
- `failed` = check retry count, retry or deadletter
- `pending` = dispatch the phase

This eliminates the ambiguity window entirely. The phase_status field is the **sub-state** within the agent flow state machine that resolves partial completion.

**Strength to preserve**: The overall recovery strategy (read from durable store, check liveness, decide action) is the right approach. It follows the established pattern from Kubernetes pod recovery. The refinement is adding enough state to make the decision deterministic.

---

## 6. Cross-Cutting Concerns

### 6.1 The Verifying State Ownership Question (Open Question 4)

The ADR asks whether verification should move to the conductor, stay in rsry, or be an MCP tool. Based on the dual state machine analysis:

Verification is the **bridge** between agent flow and bead flow. It consumes agent flow output (work directory) and produces bead flow transitions (done/rejected/blocked). It should be owned by the execution layer (conductor) because:

1. The conductor has the agent's exit status and work directory.
2. Verification may need to re-dispatch (staging-agent reviewing dev-agent's work) -- this is execution.
3. The Rust verifier in `verify.rs` runs shell commands (cargo check, go test) -- these are workload execution, not data operations.

However, the verification **result** should be persisted via rsry (bead comments, status transitions). This keeps rsry as the data layer.

### 6.2 Write-Through vs Batching (Open Question 1)

Write-through on every phase transition. The latency cost is one HTTP round-trip (~1-5ms local loopback) per transition. Phases last minutes (agent execution time). The overhead is negligible relative to phase duration. Batching risks losing exactly the state needed for recovery -- it is a false optimization.

### 6.3 The Stale State as a Hidden Third State Machine

There is a subtlety the ADR does not address: `BeadState::Stale` in `bead.rs`. Stale beads have `valid_transitions = [Open]` -- they can only go to Open. But what triggers Stale? Looking at the code, `Stale` appears to be set externally (build hash detection per commit `4c648e3`).

Stale detection is a **third concern** -- neither bead flow (user-facing lifecycle) nor agent flow (execution lifecycle), but **infrastructure health monitoring**. The ADR should note this as out of scope or explicitly assign it to one of the two state machines. Given that Stale is a bead-level status visible to users, it belongs to the bead flow, triggered by infrastructure signals.

### 6.4 The `rsry run` Fallback Path (Open Question 2)

Keep it. The Rust reconciler (`reconcile.rs`) is a working, tested, single-process execution path. The conductor adds OTP supervision but also adds a network hop, a second runtime, and distributed state complexity. For local single-repo development, `rsry run` is simpler and sufficient. The two paths share the same bead state (per-repo Dolt) and the same verification pipeline -- they differ only in execution supervision.

The risk is **state machine divergence**: if `rsry run` and the conductor implement slightly different transition logic, they could drive beads into inconsistent states. Mitigation: the bead flow transitions should be validated by rsry (the data layer), not by the execution layer. `persist_status()` should enforce `valid_transitions()` regardless of caller.

---

## 7. Summary of Findings

### What the ADR Gets Right

1. **The core decomposition** -- users see beads, infrastructure sees agents -- is the correct abstraction boundary. This is well-motivated and consistently applied.

2. **rsry as shared persistence** eliminates a class of state synchronization problems. Having one data layer with two consumers (conductor and user-facing tools) is cleaner than two data layers.

3. **The factory pattern** (one bead, one agent, isolated worktrees) makes agent flow inherently simpler. No inter-agent state to synchronize. This is a significant architectural win that simplifies the state machine composition.

4. **The trait-based store abstraction** (`HierarchyStore`, `DispatchStore`, `LinkageStore`) is well-factored. The in-memory test implementation validates the interface contract. The Dolt implementation is straightforward.

5. **The phased implementation plan** is realistic and correctly ordered. Pipeline state (Phase 2) before triage (Phase 3) before hierarchy (Phase 4) is the right dependency order.

### What Needs Attention

| Priority | Issue | Section |
|----------|-------|---------|
| P0 | Define write ordering at dispatch touch point (bead first, pipeline after) | 3, F1 |
| P1 | Add phase_status to PipelineState for unambiguous recovery | 5 |
| P1 | Split conflict resolution: bead-flow = backend wins, agent-flow = conductor wins | 4 |
| P2 | Cross-reference pipeline vs bead state on recovery (F2 invariant check) | 3, F2 |
| P2 | Conductor needs periodic reconciliation sweep, not just startup recovery | 3, F3 |
| P3 | Define diagnostic comment mechanism for stuck bead visibility | 2 |
| P3 | Enforce valid_transitions in persist_status regardless of caller | 6.4 |

### Suggested Next Steps

1. Amend ADR-004 with the write ordering constraint for touch points (P0).
2. Add `phase_status` field to `PipelineState` in `store.rs` and schema in `store_dolt.rs` (P1).
3. Document the conflict resolution split (bead vs agent flow ownership) in the ADR (P1).
4. Implement startup recovery with cross-reference: pipeline state vs bead state (P2).
5. Design the periodic reconciliation sweep as a conductor GenServer (P2).

---

## Appendix A: Formal State Machine Diagrams

### Bead Flow (B) -- from bead.rs

```
backlog --[promote]--> open
open --[triage]--> queued
queued --[acquire semaphore]--> dispatched
dispatched --[agent exits]--> verifying
verifying --[all tiers pass]--> done
verifying --[tier fails]--> rejected
verifying --[needs human]--> blocked
rejected --[backoff expires]--> open
blocked --[resolved]--> open
stale --[refresh]--> open
done --[terminal]
```

### Agent Flow (A) -- reconstructed from ADR + reconcile.rs

```
idle --[bead selected]--> pending
pending --[dispatched]--> executing
executing --[agent exits ok]--> verify
executing --[agent exits fail]--> failed
executing --[timeout]--> failed
verify --[all tiers pass]--> completed
verify --[tier fails]--> failed
completed --[has next phase]--> pending  (advance)
completed --[no next phase]--> idle      (clear pipeline)
failed --[retries < max]--> pending      (retry with backoff)
failed --[retries >= max]--> deadletter
deadletter --[terminal for this pipeline]
```

### Product Invariants

```
(B=dispatched OR B=verifying) <=> (A in {executing, verify, completed, failed})
(B=done) => (A=idle)
(B=open AND A=pending) => dispatch imminent
(B=blocked) => (A=deadletter OR A=idle)
```

These invariants define the valid region of B x A. Any state pair outside this region indicates a consistency violation requiring recovery action.
