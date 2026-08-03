# ADR-0023: Decouple dispatch/orchestration from task management — one-directional crate boundary

**Status:** Proposed
**Date:** 2026-08-03
**Repo:** rosary
**Tracking bead:** `rosary-c5ae10`

**Relates to:**
- [ADR-0015](0015-execution-lineage-capsules.md) (Proposed) — establishes the
  structural/temporal axis (`Decade ⊃ Thread ⊃ Bead` vs `Capsule`) this ADR
  extends with a third, orthogonal axis: **which crate owns which code**.
- [ADR-0014](0014-decouple-rosary-from-bd.md) (Accepted) — same "decouple"
  lineage: rosary already treats store ownership as a first-class boundary
  decision rather than an implementation detail.
- [ADR-0006](0006-declarative-tool-registry.md) (Proposed) — the MCP
  tool-exposure-by-domain consequence below (§Consequences) is a slice of
  this, not a competing decision.
- [ADR-0019](0019-harness-is-the-licensed-runtime.md) (Proposed) — a dispatch-side
  concern; unaffected by where the crate boundary sits.
- `docs/design/2026-07-27-two-axes.md` — orthogonal, not restated. That doc's
  STORE / INTERACTION-SURFACE axes are properties *of* the task-management
  crate this ADR carves out. This ADR is the axis that decides which code
  lives in which crate in the first place.
- signet `docs/apas/agent-provenance-standard.md` §3.2 — external validation
  (see Context).
- `rosary-c5ae10` — the tracking bead; independently reached the same
  conclusion on 2026-05-19 ("the bead store should not need to know about
  dispatch") before this ADR existed.

## Context

Rosary today is one crate with two domains wired together: **task
management** (what work exists, its state, its history — `bead.rs`,
`bead_ops.rs`, `bead_sqlite/`, `bead_dolt.rs`, `bead_backup.rs`,
`bead_move.rs`, `bead_diff.rs`, `bead_migrate.rs`, `bead_genesis.rs`,
`bead_ext/`, `store.rs`, `store_dolt.rs`, `store_sqlite.rs`) and **dispatch
/ orchestration** (how work gets executed — `src/dispatch/`,
`src/reconcile/`, `src/pipeline.rs`, `src/workspace/`). The user's framing
from `rosary-c5ae10` (2026-05-19): dispatch is where active iteration and
risk live, and a bug there should not be able to destabilize the tracker
that everything else (Linear sync, MCP bead tools, `rsry status`) depends on.

**Measured coupling** (this session, `grep`/`wc -l` over current `main`):

- Dispatch/orchestration side: **17,696 lines** (`src/dispatch/*.rs`: 7,592;
  `src/reconcile/*.rs`: 6,391; `src/pipeline.rs`: 568; `src/workspace/*.rs`:
  3,145).
- Task-management side (bead-store-scoped subset): **6,294 lines**
  (`bead.rs` + `bead_ops.rs` + `bead_sqlite/*.rs` + `bead_dolt.rs` +
  `store.rs`) — the broader domain (`bead_backup.rs`, `bead_move.rs`,
  `bead_diff.rs`, `bead_migrate.rs`, `bead_genesis.rs`, `bead_ext/`,
  `store_dolt.rs`, `store_sqlite.rs`) is larger still and belongs on the same
  side of the boundary.
- **13 files** under `dispatch/`/`reconcile/`/`pipeline.rs` directly import
  `BeadStore`, `connect_bead_store`, or `bead_ops`/`bead_sqlite`/`bead_dolt`
  types — expected, and the direction this ADR keeps.
- **The reverse direction is not currently clean**, which is the actual
  finding: `store.rs:19` imports `crate::dispatch::AgentSessionRef` (plus 6
  live test usages), and `bead_ops.rs:99` calls
  `crate::dispatch::default_agent(&self.issue_type)` **on a production code
  path** (bead creation resolves a default assignee by asking the dispatch
  module what agent a given `issue_type` maps to). Two concrete leaks, not a
  hypothetical risk.
- `src/dispatch/mod.rs:45` already carries a comment anticipating "when
  dispatch migrates fully to BeadStore" — the coupling is a recognized,
  named debt inside the code, not something this ADR is inventing.

**External validation.** Signet's APAS `dispatch/v1` predicate
(`docs/apas/agent-provenance-standard.md` §3.2) structures its own
attestation the same way this ADR proposes structuring rosary's crates: the
work item is referenced *by identity and content hash*
(`dispatchDefinition.workItemRef: {repo, workItemId, contentHash}`) from
inside the dispatch record — the dispatch predicate points at the work item,
never the other way around, and never embeds the work item's own structure.
A second, independently-designed system converged on the same one-directional
shape for the same two concepts.

**What this ADR is not.** It does not re-litigate
`docs/design/2026-07-27-two-axes.md`'s store-vs-interaction-surface findings
— those are properties of whichever crate ends up owning the store. It does
not decide the DoltBeadStore-vs-SqliteBeadStore backend question. It does
not cover Linear/GitHub sync (`linear.rs`, `linear_tracker.rs`,
`github_mirror.rs`, `sync.rs`) — those already only import bead types, so
they sit on the task-management side without change.

## Decision

**Task management never depends on dispatch, orchestration, pipeline, or
workspace code.** Concretely, none of `bead.rs`, `bead_ops.rs`,
`bead_sqlite/`, `bead_dolt.rs`, `bead_backup.rs`, `bead_move.rs`,
`bead_diff.rs`, `bead_migrate.rs`, `bead_genesis.rs`, `bead_ext/`,
`store.rs`, `store_dolt.rs`, `store_sqlite.rs` may `use crate::dispatch`,
`use crate::reconcile`, `use crate::pipeline`, or `use crate::workspace`.
`src/dispatch/`, `src/reconcile/`, `src/pipeline.rs`, and `src/workspace/`
continue to depend on task management freely — that direction is correct
and stays.

**Two-phase sequencing**, matching `rosary-c5ae10`'s existing acceptance
criteria rather than replacing them:

1. **Phase 1 — close the two known leaks, enforce with Cargo features.**
   - `bead_ops.rs:99`'s `default_agent` call: task management should not
     know what "an agent" is. Move default-assignee resolution to the
     dispatch side — either the caller (dispatch) supplies a
     `default_assignee` at `BeadCreateArgs` construction time, or bead
     creation leaves the field unset and dispatch fills it in at triage.
     Either removes the import; which one is an implementation call for
     whoever picks up `rosary-c5ae10`, not this ADR.
   - `store.rs:19`'s `AgentSessionRef`: this type is dispatch-session
     provenance, not bead state — it belongs in `src/dispatch/` (or a
     shared substrate module both sides depend on, e.g. `src/observation/`,
     which already plays that role for `Status` folding). Move the type
     definition; task management keeps only whatever opaque reference it
     needs to store, typed generically.
   - Land `rosary-c5ae10`'s `minimal` Cargo feature once both leaks are
     gone. The compiler enforces the boundary from that point forward:
     `cargo build --no-default-features --features minimal` failing to
     compile is the mechanical proof a future change reintroduced the
     reverse dependency, the same role ADR-0018's smell ratchet plays for
     structural rot.
2. **Phase 2 — promote to a Cargo workspace member, if standalone
   installability is actually needed.** A feature flag still ships one
   binary from one crate; it does not let someone `cargo install
   rosary-tasks` without the dispatch dependency tree. If/when that need is
   real (e.g. an agent or human wants the tracker with zero dispatch/Linear/
   provider surface), Phase 1's already-enforced boundary makes the crate
   split mechanical rather than a redesign. Not committed to a timeline
   here — Phase 1 alone delivers the stated goal ("a bug in `dispatch/`
   can't break `rsry status`") via the compiler, which is most of the value
   at a fraction of the migration cost.

`src/observation/` (the ADR-0010 lattice substrate) is read by both sides
and stays where it is — it is genuinely shared substrate, not owned by
either domain, the same status `docs/design/2026-07-27-two-axes.md` already
gives it.

## Consequences

**MCP tool exposure by domain (folds in the smell the user flagged, no
separate ADR).** `server.json` already declares an
`io.github.agentic-research.rosary/tool-categories` field with 8 categories,
including `"beads"` (7 tools) and `"dispatch-pipeline"` (7 tools) — a
domain-tag split that already maps cleanly onto this ADR's crate boundary.
Today nothing in `src/serve/*.rs` reads this field back; it is
declared-but-inert metadata. Once Phase 1's `minimal` feature exists,
`src/serve/mod.rs`'s tool registration becomes the natural place to filter
by category so a minimal build registers only the `"beads"`-tagged tools —
turning the existing-but-unused tag mechanism into the "surfaceable by tag"
behavior raised alongside this decision, for free, instead of a new
registry.

**Positive:**
- A stable tracker floor to iterate dispatch on top of — the user's stated
  motivation.
- `cargo build --no-default-features --features minimal` becomes a real CI
  gate (mirrors ADR-0018's ratchet pattern: mechanical, not aspirational)
  against this exact regression class.
- Opens "rosary as issue tracker, no orchestration" as a standalone offer
  later, without redesign.

**Cost:**
- Two call sites (`bead_ops.rs:99`, `store.rs:19`) need real code motion,
  not just a `#[cfg]` wrap — tracked as `rosary-c5ae10`'s existing
  acceptance criteria.
- CI test matrix doubles per `rosary-c5ae10`'s own time-box estimate (~2
  days, feature-gating touches many imports).

**Non-goals:** this ADR does not mandate Phase 2 on any timeline, does not
touch backend selection (Dolt vs SQLite), and does not move
Linear/GitHub/sync code, which is already correctly scoped to the
task-management side.
