# Production Readiness Review: ADR-004 Dual State Machine

**Reviewer**: production-readiness-reviewer
**Date**: 2026-03-15
**Scope**: ADR-004 (dual state machine design), rsry/conductor boundary, data consistency, failure modes
**Verdict**: CONDITIONAL SHIP -- address critical items before wiring Phase 2 MCP tools

---

## Files Analyzed

| File | Purpose |
|------|---------|
| `docs/adr/0004-dual-state-machine.md` | The ADR under review |
| `src/store.rs` | Backend store traits (HierarchyStore, DispatchStore, LinkageStore) |
| `src/store_dolt.rs` | Dolt implementation of backend store |
| `src/serve.rs` | MCP HTTP server, session management, tool dispatch |
| `src/config.rs` | BackendConfig, connection configuration |
| `conductor/lib/conductor/orchestrator.ex` | Periodic orchestration loop |
| `conductor/lib/conductor/agent_worker.ex` | Per-bead agent lifecycle GenServer |
| `conductor/lib/conductor/rsry_client.ex` | Elixir JSON-RPC client to rsry MCP |
| `conductor/lib/conductor/pipeline.ex` | Pipeline data structure and templates |
| `conductor/lib/conductor/agent_supervisor.ex` | DynamicSupervisor for agents |

---

## CRITICAL ISSUES (Production Blockers)

### C1. Split-brain state on partial MCP write failure

**What**: The ADR proposes that the conductor writes pipeline state to rsry via MCP on every phase transition. But bead status and pipeline state live in different Dolt databases (per-repo `.beads/` vs `~/.rsry/dolt/rosary/`) with no distributed transaction. A phase transition requires TWO writes:

1. `rsry_pipeline_upsert` -- write pipeline state to backend Dolt
2. `rsry_bead_close` or `rsry_bead_comment` -- write bead state to per-repo Dolt

If write 1 succeeds and write 2 fails (or vice versa), the two databases disagree.

**Why this matters**: The conductor currently does `bead_comment` and `bead_close` calls in `on_success()` and `on_failure()` (agent_worker.ex lines 282-367). When Phase 2 adds `pipeline_upsert` calls alongside these, a network timeout between them creates a window where pipeline state says "phase 2 complete" but bead state still says "dispatched". On conductor restart, `list_active_pipelines()` will return stale data that contradicts the bead's actual status.

**Blast radius**: A single stuck bead per occurrence. But since beads drive Linear sync, a stuck bead means Linear shows stale status indefinitely, and the bead may be re-dispatched (double-dispatch).

**How to trigger**: Kill rsry between the two writes. Or: rsry's Dolt connection pool is exhausted for one database but not the other. Or: one Dolt server is restarting while the other is up.

**Fix**: Define a canonical write ordering and idempotent recovery:

1. Always write pipeline state FIRST (backend Dolt). This is the "intent" record.
2. Then write bead state (per-repo Dolt). This is the "effect".
3. On conductor restart recovery, for each active pipeline: read the bead's actual status from rsry. If the pipeline says "phase N complete" but the bead still says "dispatched", re-apply the bead state transition. This is the "backend wins, re-derive bead state" pattern the ADR already mentions -- but it needs to be codified as an explicit recovery procedure, not just a philosophical statement.
4. Consider adding a `last_synced_bead_status` field to `PipelineState` so the recovery loop can detect divergence mechanically.

### C2. No fencing on concurrent dispatch of the same bead

**What**: The `dispatched` MapSet in `orchestrator.ex` (line 13) is the only guard against double-dispatch. This set is in-memory and ephemeral. If the conductor restarts, the set is empty. The first tick after restart will call `fetch_dispatchable_beads()`, which calls `list_beads("open")`. If a bead's status is still "open" because the previous agent hasn't closed it yet (or the status write failed), it will be dispatched again.

**Why this matters**: Two agents working on the same bead in the same repo will create conflicting git commits, corrupt worktrees, and produce nonsensical PR states. This is the single most dangerous failure mode in a work orchestration system.

**Current code path** (orchestrator.ex lines 115-157):
```
do_tick -> fetch_dispatchable_beads() -> list_beads("open") -> start_agent(bead)
```
The `Enum.reject(&MapSet.member?(state.dispatched, &1["id"]))` filter only checks the in-memory set.

**How to trigger**:
1. Start conductor, dispatch bead X (status set to "dispatched" by rsry).
2. Kill conductor.
3. Restart conductor. `dispatched` MapSet is empty.
4. The bead_close or status update from the first agent hasn't landed yet (agent still running).
5. If the bead's status is still "open" in Dolt (race window), tick dispatches bead X again.

Even without restart: `fetch_dispatchable_beads()` filters on `status = "open"`. But `rsry_dispatch` tool (serve.rs line 534) sets the bead to "dispatched". So in the normal path, the bead won't appear in the next tick's fetch. But if the `update_status("dispatched")` call fails silently (it uses `let _ =` -- fire-and-forget at serve.rs line 534), the bead stays "open" and will be re-fetched.

**Fix**: Three layers of defense:

1. **Immediate**: Change serve.rs line 534 from `let _ = client.update_status(bead_id, "dispatched")` to a hard error. If the status update fails, the dispatch tool must return an error. The conductor must not proceed if dispatch returns an error.

2. **Phase 2**: On conductor startup recovery, read `list_active_pipelines()` from the backend store. Cross-reference against `active_dispatches()`. Populate the `dispatched` MapSet from this data before the first tick.

3. **Structural**: Add a `dispatched_by` field (e.g., conductor instance ID + timestamp) to pipeline_state. Before dispatching, check if a pipeline already exists for this bead. If it does, skip. This makes the backend store the distributed lock.

### C3. rsry session state is in-memory -- restart drops all conductor connections

**What**: rsry's MCP sessions (`AppState.sessions: Arc<RwLock<HashSet<String>>>` at serve.rs line 726) are stored entirely in memory. When rsry restarts, all session IDs are lost. The conductor's `RsryClient` holds a session ID (rsry_client.ex line 18) that becomes invalid.

**Why this matters**: The conductor's `RsryClient` does handle session expiration (rsry_client.ex lines 122-130) by reconnecting on 404. But this reconnection path calls `force_reconnect()`, which does a blocking `Process.sleep()` inside a GenServer `handle_call` (rsry_client.ex line 223). During this sleep:

- All pending MCP tool calls from AgentWorker processes are blocked (they call `GenServer.call(__MODULE__, {:tool, name, args}, 30_000)`)
- The 30-second timeout (rsry_client.ex line 74) starts ticking
- If reconnection takes >30 seconds (5 retries x 2-10s backoff), in-flight tool calls get `{:error, :timeout}`
- AgentWorkers calling `bead_comment` or `bead_close` may silently fail (fire-and-forget pattern in agent_worker.ex lines 293, 305, 349, etc.)

**Blast radius**: ALL active agents lose state-write capability simultaneously when rsry restarts. Comments and status transitions are lost. Agents continue working but their results are not recorded.

**Fix**:
1. The `RsryClient` GenServer is a bottleneck. Consider using a connection pool or allowing concurrent requests (currently serialized through GenServer mailbox). At minimum, the reconnect logic should not block the caller -- queue pending requests and retry after reconnection.
2. Add a circuit breaker: if rsry is unreachable, buffer critical writes (bead_close, status transitions) and replay them when the connection is restored. Non-critical writes (comments) can be dropped.
3. rsry could persist session IDs to disk so they survive restarts. But the simpler fix is making the conductor resilient to session loss.

---

## HIGH PRIORITY (Should Fix Before Deployment)

### H1. `let _ =` fire-and-forget on status update during dispatch

**What**: serve.rs line 534: `let _ = client.update_status(bead_id, "dispatched").await;`

This silently ignores failures when marking a bead as dispatched. Combined with C2 above, this means dispatch can "succeed" from the conductor's perspective while the bead remains in "open" status in Dolt, creating a double-dispatch risk.

**File**: `/Users/jamesgardner/remotes/art/rosary/.claude/worktrees/cozy-purring-moler/src/serve.rs` line 534

**Fix**: Return an error from `tool_dispatch` if `update_status` fails. The conductor should treat a dispatch failure as retryable.

### H2. Network hop latency for every state write (Open Question 1)

**What**: The ADR identifies this as a negative consequence but does not resolve it. Every pipeline phase transition requires: conductor GenServer -> HTTP POST -> rsry JSON-RPC parse -> Dolt SQL query -> response. The `RsryClient` GenServer serializes all calls (single mailbox), so concurrent agents compete for the same bottleneck.

**Measured path**: AgentWorker calls `client().bead_comment(...)` which calls `RsryClient.bead_comment/3` (module function) which calls `GenServer.call(__MODULE__, {:tool, ...}, 30_000)`. Under load with 3 concurrent agents, each agent's state writes queue behind the others.

**Impact**: At 3 concurrent agents, if each phase transition involves 2-3 MCP calls (comment + pipeline_upsert + potentially bead_close), and each call takes 50-100ms round-trip, the RsryClient mailbox processes 6-9 calls sequentially. Worst case: ~900ms of serialized I/O per tick. Not catastrophic for 3 agents, but it will not scale to the 5 max_children configured in AgentSupervisor.

**Fix options**:
- **Short term**: Use `Req` directly from each AgentWorker (remove the GenServer serialization). The session ID can be stored in an ETS table or Agent. Each worker gets its own HTTP connection.
- **Medium term**: Batch pipeline state writes. Upsert pipeline + record dispatch + comment in a single MCP call (add a batch tool).
- **Long term**: Direct Dolt access from Elixir via MyXQL. Skip the HTTP/JSON-RPC layer entirely for the conductor. This eliminates the network hop but creates a tighter coupling.

### H3. Conductor crash during agent execution orphans OS processes

**What**: When AgentWorker terminates (agent_worker.ex lines 264-278), it calls `Port.close(state.port)`. Port.close sends SIGTERM to the agent process. But if the BEAM VM itself crashes (not just the GenServer), Erlang Ports are cleaned up by the OS -- the child process may or may not receive SIGTERM depending on the crash mode.

More importantly: if the conductor is killed with SIGKILL (e.g., `kill -9`), Ports are NOT closed. The spawned `claude` or `claude-agent-acp` processes continue running as orphans. They may still be holding locks on worktrees, writing to repos, and consuming API credits.

**Current state**: The existing `SessionRegistry` in rsry (session.rs, referenced at serve.rs line 537) tracks dispatched agents. But it uses PIDs that become stale across conductor restarts.

**How to trigger**: `kill -9 <beam_pid>` while agents are running.

**Fix**:
1. Store the OS PID of each agent in the backend Dolt `dispatches` table (the `DispatchRecord` already has a placeholder for this -- but `work_dir` is stored, not PID).
2. On conductor startup recovery, for each active dispatch: check if the OS PID is still alive. If yes, attempt to re-attach (reopen the Port). If no, mark the dispatch as failed.
3. Consider adding a reaper process that runs independently of the conductor and kills orphaned agent processes based on the dispatches table.

### H4. `String.to_existing_atom` in Pipeline.from_map is a crash vector

**What**: pipeline.ex line 297: `get.(h, :outcome) |> to_string() |> String.to_existing_atom()`

If the outcome string from Dolt contains a value that has not been previously interned as an atom (e.g., a new outcome type added to the Rust side but not yet known to the Elixir side), this will raise `ArgumentError`, crashing the Pipeline deserialization and potentially the AgentWorker or the recovery loop.

**File**: `/Users/jamesgardner/remotes/art/rosary/.claude/worktrees/cozy-purring-moler/conductor/lib/conductor/pipeline.ex` line 297

**Fix**: Use `String.to_atom/1` (which creates atoms dynamically) or, better, validate against a known set of outcomes and fall back to `:unknown` for unrecognized values. Since this is deserializing from a database, defensive parsing is mandatory.

Similarly, pipeline.ex Step module line 396: `String.to_existing_atom(m)` for mode parsing -- same crash risk.

And pipeline.ex Step module line 400: `String.to_atom(a)` for `parallel_group` -- this creates atoms from user-controlled strings, which is a memory leak vector (atoms are never garbage collected in the BEAM).

### H5. No health check or readiness probe for rsry

**What**: The conductor's `RsryClient` calls `initialize` on startup (rsry_client.ex line 96), and if it fails, starts anyway with `session_id: nil`. The `maybe_reconnect` function (line 213) will attempt reconnection on the first tool call. But there is no periodic health check.

If rsry becomes unresponsive (e.g., Dolt server hangs, rsry is in a deadlock), the conductor will continue ticking, all tool calls will time out at 30 seconds each, and the RsryClient mailbox will fill with blocked callers.

**Fix**: Add a periodic health check (e.g., every 30 seconds, call `rsry_status`). If N consecutive health checks fail, pause the orchestrator (stop dispatching new beads). Resume when rsry responds. This prevents wasting agent API credits on beads whose state changes will be lost.

### H6. Validation command runs with shell injection risk

**What**: agent_worker.ex line 559: `System.cmd("/bin/sh", ["-c", command], cd: work_dir, ...)`

The `command` comes from pipeline step definitions (pipeline.ex line 58: `@validation_implement %{command: "task test", ...}`). Currently these are hardcoded, but the ADR mentions pipeline mutation at runtime ("Modified at runtime -- insert a review step", pipeline.ex line 24). If a pipeline step's validation command is ever derived from bead content or external input, this becomes a shell injection vector.

**Current risk**: LOW (commands are compile-time constants). Future risk: HIGH if pipeline steps become data-driven.

**Fix**: Document that validation commands MUST be from a trusted allowlist. If runtime pipeline mutation is implemented, validate commands against a regex whitelist (e.g., only `task <name>` patterns).

---

## MEDIUM PRIORITY (Technical Debt)

### M1. `list_active_pipelines` returns ALL pipelines, not just active ones

**What**: store_dolt.rs lines 432-443: `list_active_pipelines()` does `SELECT ... FROM pipeline_state` with no filter. In the in-memory test implementation (store.rs line 262-265), it returns `pipelines.clone()` -- everything. The trait name says "active" but the implementation returns all pipelines ever created until `clear_pipeline` is called.

This is technically correct if `clear_pipeline` is always called when a pipeline completes. But if it is ever missed (bug, crash, network failure during cleanup), completed pipelines accumulate and the conductor's startup recovery logic will attempt to resume them.

**Fix**: Either add a `status` column to `pipeline_state` (active, completed, failed) and filter on it, or rename the method to `list_all_pipelines()` to make the contract explicit. Add an index on the status column if filtering.

### M2. No request timeout on individual Dolt queries in store_dolt.rs

**What**: The `DoltBackend` uses a `MySqlPool` with default timeouts. If a Dolt query hangs (e.g., lock contention, disk I/O stall), the calling code (an MCP tool handler) will block indefinitely. Since the MCP HTTP handler is async, this blocks a tokio task but not the entire server -- but it can exhaust the connection pool.

**Fix**: Set connection and query timeouts on the `MySqlPool` via `MySqlPoolOptions`. Consider using `tokio::time::timeout` around each query for defense in depth.

### M3. Port file race condition in DoltBackend::connect

**What**: store_dolt.rs lines 63-70 and 88-112. The auto-start logic:
1. Reads `backend.port` file to get existing server port
2. If connection fails, allocates an ephemeral port
3. Starts `dolt sql-server` on that port
4. Writes the port to `backend.port`

If two rsry instances start simultaneously (e.g., stdio + HTTP transports), both may:
1. Fail to connect to the existing server
2. Both allocate different ephemeral ports
3. Both try to start Dolt servers in the same directory
4. One succeeds, the other fails (Dolt acquires a directory lock)
5. The port file may end up with the wrong port

**Fix**: Use a filesystem lock (e.g., flock on a `.lock` file) around the auto-start section. Or: use a fixed port configured in BackendConfig instead of ephemeral allocation.

### M4. Conductor's `dispatched` MapSet grows unboundedly

**What**: orchestrator.ex line 13: `dispatched: MapSet.new()`. Bead IDs are added on dispatch (line 139) but never removed. Over time, this set grows with every dispatched bead. The `Enum.reject` filter (line 127) checks membership on every tick, turning into an O(N) operation where N is the total number of ever-dispatched beads.

**Fix**: Remove bead IDs from the MapSet when the AgentWorker terminates. This requires the Orchestrator to monitor AgentWorker processes (use `Process.monitor`) and handle the `:DOWN` message to clean up the set.

### M5. No correlation ID between conductor and rsry logs

**What**: The conductor logs with `[orchestrator]`, `[worker]`, `[pipeline]`, etc. rsry logs with `[rsry-mcp]`. There is no shared correlation ID (e.g., bead_id + dispatch_id) in HTTP headers or log context that would allow tracing a single bead's lifecycle across both processes.

**Fix**: When the conductor calls rsry MCP tools, include a `X-Request-Id` or similar header containing `{bead_id}/{dispatch_id}`. rsry should log this alongside its own output. This is essential for debugging stuck beads in production.

### M6. Backoff calculation in on_failure uses floating point

**What**: agent_worker.ex line 354: `backoff = min((30_000 * :math.pow(2, retries)) |> trunc(), 300_000)`

`:math.pow/2` returns a float. For `retries` values beyond ~50, floating point precision degrades. While `retries` is unlikely to exceed `max_retries` (2-3), the code path doesn't enforce this -- `can_retry?` is checked but the backoff is calculated regardless. If a bug allows retries to accumulate, the backoff calculation could produce unexpected values.

**Fix**: Use integer arithmetic: `backoff = min(30_000 * Bitwise.bsl(1, retries), 300_000)`. This is exact for any retry count.

### M7. Pipeline history is append-only with list concatenation

**What**: pipeline.ex line 188: `%{pipeline | history: pipeline.history ++ [entry]}`

The `++` operator on lists is O(N) where N is the length of the history. For long-running pipelines with many retries, this degrades. More importantly, the history is serialized to Dolt as JSON (via `to_map`). Large histories bloat the pipeline_state record.

**Fix**: Use a prepend (cons) operation and reverse when needed for display. Or: store history entries as separate rows in a `pipeline_history` table rather than as a JSON blob in the pipeline_state row.

---

## RECOMMENDATIONS

### R1. MCP is acceptable but not ideal for conductor-to-rsry communication

The ADR's Open Question 6 (implied): should MCP be the protocol between conductor and rsry?

**Pros of MCP**:
- Already implemented and working
- Same protocol used by Claude Code and other MCP clients
- Session management handles reconnection
- JSON-RPC is debuggable (curl-friendly)

**Cons of MCP**:
- No streaming -- each call is request/response
- No server-initiated notifications (SSE is not implemented, serve.rs line 885 returns 405)
- The RsryClient GenServer serializes all calls through one mailbox
- MCP adds overhead: JSON-RPC framing, session validation, MCP content wrapper

**For this use case**, the overhead is tolerable at the current scale (3-5 concurrent agents). The bigger concern is that MCP does not support server-push. When a bead's status changes externally (e.g., via Linear webhook -> rsry), rsry cannot notify the conductor. The conductor must poll on its tick interval.

**Recommendation**: Keep MCP for Phase 2. If latency becomes a problem in Phase 3+, consider either:
- Adding a lightweight notification channel (rsry sends UDP datagrams or uses a UNIX socket to signal "state changed, re-poll")
- Direct Dolt access from the conductor (eliminates the intermediary entirely, but couples the systems)
- gRPC with bidirectional streaming (overkill for localhost, but solves the push problem)

### R2. Add an explicit "fencing token" to prevent stale writes

When the conductor reads pipeline state on startup and decides to resume a bead, it should write a "fencing token" (e.g., `conductor_instance_id + monotonic_counter`) to the pipeline_state row. All subsequent writes from this conductor instance include the token. rsry rejects writes with stale tokens. This prevents a zombie conductor instance (e.g., one that was partitioned but not dead) from overwriting state written by the new instance.

### R3. Implement the "stuck bead detector"

Add an MCP tool `rsry_stuck_beads` that queries:
```sql
SELECT * FROM pipeline_state
WHERE updated_at < NOW() - INTERVAL 30 MINUTE
  AND backoff_until IS NULL
```
This returns beads whose pipeline hasn't been updated in 30 minutes and aren't in backoff. These are likely stuck due to a conductor crash or orphaned agent. Surface this in `rsry status` output and in Linear (as a label or comment).

### R4. Load testing recommendation

Before Phase 2 goes live, run a load test:
1. Start rsry and conductor
2. Create 10 beads with `issue_type: "task"` (single-step pipeline)
3. Set `max_concurrent: 5`
4. Use a mock agent (instant exit with code 0)
5. Measure: time from bead creation to pipeline completion, RsryClient queue depth, Dolt query latency
6. Then: kill rsry mid-flight, restart, verify recovery
7. Then: kill conductor mid-flight, restart, verify no double-dispatch

### R5. The ADR should resolve Open Question 1 explicitly

The ADR asks whether to write-through on every phase transition or batch at checkpoints. The answer should be: **write-through for status transitions, batch for comments**.

- Pipeline phase changes (dispatched, verifying, done, blocked, rejected) MUST be written immediately. These are the fencing points that prevent double-dispatch and enable recovery.
- Comments (progress notes) can be batched or even dropped on failure. They are informational, not structural.

This distinction should be documented in the ADR.

---

## Summary Assessment

**Architecture**: The dual state machine separation is sound. Bead flow and agent flow genuinely are different concerns with different lifecycles. The decision to make rsry the single persistence layer is correct -- it avoids the conductor needing its own database and keeps Dolt as the single source of truth.

**Primary risk**: The lack of distributed transactions between per-repo Dolt and backend Dolt. This is inherent to the architecture and cannot be fully solved without a single database. The mitigation (ordered writes + idempotent recovery) is viable but must be implemented explicitly, not assumed.

**Secondary risk**: The RsryClient GenServer is a serialization bottleneck and a single point of failure for all state writes. At current scale (3 agents) this is fine. At target scale (5+ agents, multiple repos) it needs to be addressed.

**The conductor's in-memory state is the real vulnerability**. The `dispatched` MapSet, the `Pipeline` struct, the Port references -- all lost on restart. Phase 2's write-through to backend Dolt addresses this, but only if the writes are reliable and the recovery logic is correct. The critical items above (C1, C2, C3) all stem from this gap.

**Verdict**: Ship Phase 2 with the following conditions:
1. Fix C2 (double-dispatch) before wiring any new MCP tools. The `let _ =` on status update (H1) is the immediate fix.
2. Design the recovery procedure (C1) before implementing `rsry_pipeline_upsert`. Write it down in the ADR as a "Recovery Protocol" section.
3. Accept C3 (session loss) as a known limitation with the RsryClient reconnection logic as mitigation. Add the health check (H5) as a fast-follow.
4. Fix H4 (`String.to_existing_atom`) immediately -- it is a runtime crash waiting to happen.
