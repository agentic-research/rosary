# ADR-0010: Observation Lattice — Multi-Source Reconciliation Substrate

**Status:** Proposed
**Date:** 2026-05-05
**Depends on:** ADR-0005 (reactive store), ADR-0009 (cross-repo linkage)
**Relates to:** rosary-7023a9 (substrate bead), rosary-45518d (CRDT-lattice draft superseded)

## Context

Today rosary treats `Bead` as the source of truth and pushes derived state into
Linear and GitHub via one-off paths (`persist_status`, `linear_tracker`,
`github_mirror`). External systems are demoted to "downstream sinks." The author
wants to invert this: every external system — Linear, GitHub, git remotes, fork
state, beads themselves, future Slack/Notion/calendar — is a **peer source of
observations** about the same underlying work item. Rosary becomes the substrate
that ingests observations, computes a derived view via deterministic fold, and
surfaces conflicts + staleness without the user having to run commands.

Three forces shape the design:

1. **Anti-shadowban constraint.** Aggressive polling got the author's GitHub
   account flagged as a crypto miner in March 2026. The substrate must be
   **webhook-first**, with rate-budgeted polling as fallback only, and writes
   identity-attributed via signet ephemeral certs.

2. **Late, replayed, out-of-order observations.** Webhooks fail and re-send.
   Polls double-emit during retries. Manual user input arrives whenever. The
   fold must be invariant under re-ordering and idempotent on duplicates.

3. **Honest formalism.** A prior draft (rosary-45518d) called this a "CRDT
   lattice" with a chain `backlog < open < ... < done`. Math review (this ADR's
   companion analysis) found that framing decorative: `BeadState` has no
   `Ord` impl, `valid_transitions()` has back-edges (`Verifying → Open`), and
   the situation is observers recording an external object — not replicas
   doing concurrent edits. We adopt only the pieces that are actually
   load-bearing.

## Decision

### Substrate shape: G-set of observations + deterministic fold

The substrate is **append-only set of authenticated observations** plus a
**per-field deterministic fold** that produces the derived view. We do not
claim "CRDT" in the Shapiro 2011 sense — we have observers, not replicas.
Convergence here means: same set of observations → same derived view, in any
order. That follows from set membership (G-set) plus per-field algebras
chosen below; it does not require a global semilattice over the whole record.

```
Observation = {
    work_item: WorkRef,           // (repo, bead_id) identity, see ADR-0009
    source: SourceId,             // "linear", "github", "git", "user", ...
    source_event_id: String,      // the webhook id / poll cursor / manual ack
    field: FieldName,             // "status" | "assignee" | "pr_url" | ...
    value: FieldValue,            // typed per field
    observed_at: Timestamp,       // wall-clock of the observation event
    cert: Option<SignetCert>,     // ephemeral cert; None for inbound webhooks
    payload_hash: ContentHash,    // BLAKE3 of canonical (source, field, value)
}
```

Dedup happens **before** fold ingest, on
`(source, source_event_id, payload_hash)`. Idempotence of the join is necessary
but not sufficient for correctness — content-hash dedup is what stops a
replayed webhook from being counted as two distinct observations.

### Per-field algebra (this is what's actually load-bearing)

Each field declares its algebra; the fold dispatches accordingly.

| Field | Algebra | Conflict semantics |
|-------|---------|-------------------|
| `status` | **Flat lattice** with explicit `⊤ = Conflict` | Any two distinct values join to `⊤`; same value is idempotent |
| `assignee` | LWW-register, tiebreak `(observed_at, source_id)` | Latest valid observation wins |
| `pr_url` | LWW-register, tiebreak `(observed_at, source_id)` | Latest wins; nulled out only by explicit "unset" obs |
| `merge_sha` | LWW-register | Latest wins; immutable in practice once set |
| `deadline` | LWW-register | Latest wins |
| `ahead`, `behind` | LWW-register (numeric, replaces) | Latest poll wins; not accumulated |
| `comments` | OR-set with unique tags `(source, source_event_id)` | Add/remove are commutative; no LWW needed |
| `labels` | OR-set | Same as comments |

**Status uses a flat lattice, not the chain narrated in rosary-45518d.** The
chain (`backlog < open < queued < ...`) is a workflow ordering, not a
mathematical partial order; back-edges in `valid_transitions()` make it not a
poset at all. The substrate's job is to detect "two observers disagree" — that
is `⊤ = Conflict`. The workflow ordering is enforced separately (see LTS
monotonicity below) and is **not** part of the lattice.

### LTS monotonicity is enforced at write-time, not at fold-time

Workflow validity (only allow `Queued → Dispatched`, etc.) is a property of
the labeled transition system over `BeadState`. It is checked at the
**source** (where rosary writes back to a sink) and at the **observer**
(where rosary records its own bead-internal transitions). It is **not** the
fold's job to reject a backwards transition; the fold is a deterministic
function of the observation set and must converge on whatever set it sees.

Stale or invalid observations from an observer are **quarantined** with a
diagnostic, not silently dropped. Quarantine state is queryable.

### Cert validation is a filter, not a lattice element

Signet ephemeral certs authenticate **rosary-originated** observations
(rosary writing back to a sink, manual user observations, agent dispatch
events). Inbound webhooks from Linear/GitHub do not carry signet certs —
they're authenticated by HMAC at the receiver and tagged with `source` and
`source_event_id`.

A cert that fails validation produces a quarantined observation. Quarantined
observations are **not joined into the derived view**. There is no "valid
cert" lattice element; cert state is metadata that gates ingest.

### BDR parent-child is a tree fold, not a sheaf

Decade ⊃ Thread ⊃ Bead is a strict tree partition. There is no overlap and
no non-trivial restriction map — invoking "sheaf" would be decorative. Parent
state is computed by a deterministic bottom-up fold over child states (e.g.,
"thread is `done` iff all member beads are at terminal states"). This is a
catamorphism over the tree, not a CRDT operation.

## Architecture

### Observer trait

```rust
#[async_trait]
pub trait Observer: Send + Sync {
    fn id(&self) -> &str;                          // "linear", "github_webhook", ...
    fn cadence(&self) -> Cadence;                  // Webhook | Poll(Duration) | OnDemand

    /// Produce zero or more observations from a wake event.
    async fn observe(&self, ctx: &ObserveCtx) -> Vec<Observation>;
}
```

Three concrete observers ship in v1, ported from existing one-off code paths:

| Observer | Replaces | Cadence |
|----------|----------|---------|
| `LinearObserver` | `linear_tracker.rs` polls + `persist_status` push | Webhook + 5min budget poll |
| `GithubObserver` | `github_webhook.rs` + `vcs::poll_pr_status` | Webhook + 5min budget poll |
| `BeadObserver` | direct Dolt reads from triage | OnDemand (every reconcile tick) |

Future observers (Slack, Notion, calendar, fork-state) drop in without
changes to the fold.

### Storage

Observations append into an `observations` table (orchestrator SQLite, mirrors
to Dolt for cross-repo). The derived view is materialized on read and cached
per `WorkRef`. Cache key: `(WorkRef, max_observed_at_seen)` — invalidated on
new ingest.

Quarantined observations append to a sibling `observations_quarantine` table
with a `reason` column. `rsry status --quarantine` surfaces them.

### Rate budget

The substrate maintains a per-source rate budget. Polls consume budget;
webhooks do not. Budget exhaustion does **not** retry — it logs and waits for
the next webhook or explicit user trigger. This is the anti-shadowban
guarantee.

## Test contract — the 14 invariants

These tests must all pass for the substrate's framing to hold. Each maps to
a question raised in math review.

| # | Invariant | What it tests |
|---|-----------|---------------|
| 1 | `flat_lattice_idempotent` | `join(status=Open, status=Open) = Open` |
| 2 | `flat_lattice_distinct_is_top` | `join(status=Open, status=Done) = ⊤(Conflict)` with witnesses |
| 3 | `flat_lattice_top_absorbs` | `join(⊤, _) = ⊤` |
| 4 | `lww_tiebreak_total` | LWW with equal `observed_at` resolved by `source_id` lex |
| 5 | `lww_unset_explicit` | `pr_url=None` requires explicit observation; never inferred |
| 6 | `or_set_add_remove_commute` | Comment add(c, t1) + remove(c, t1) in any order = absent |
| 7 | `or_set_unique_tags` | Same comment from two sources is two distinct entries |
| 8 | `dedup_before_fold` | Replaying a webhook with same `(source, event_id, payload_hash)` is a no-op |
| 9 | `reorder_invariance` | `fold(perm(O)) = fold(O)` for any permutation, all field types |
| 10 | `quarantine_does_not_join` | An invalid-cert observation never appears in the derived view |
| 11 | `quarantine_is_queryable` | Quarantined obs are surfaced via dedicated path, not silently dropped |
| 12 | `lts_check_at_write_time` | Writing `Done → Open` to a sink fails at the LTS gate, not at fold |
| 13 | `tree_fold_deterministic` | Decade/Thread/Bead rollup is a pure function of child states |
| 14 | `convergence_under_partition` | `fold(O₁ ∪ O₂) = fold(fold(O₁), fold(O₂))` for arbitrary partition |

A `tests/observation_lattice.rs` integration test owns these 14. Anything
that breaks them is a substrate bug, not a fold-rule bug.

## What we are explicitly **not** claiming

- We are **not** claiming a CRDT in the Shapiro 2011 sense. We have observers,
  not replicas, and we use the discipline (G-set + per-field algebras) without
  importing the convergence theorem we don't need.
- We are **not** claiming `BeadState` is a poset under workflow transitions.
  It isn't (back-edges exist). The flat lattice is over **observed values
  for the field**, not over workflow steps.
- We are **not** claiming a sheaf for BDR hierarchy. It's a tree partition;
  the rollup is a catamorphism.
- We are **not** claiming progress monotonicity from the fold. The LTS gate
  is a separate write-time check at sinks.

## Migration path

Existing one-off paths port into `Observer` instances:

1. **Phase 1** (this ADR): land observer trait, observation table, fold,
   quarantine path, the 14 tests. No behavior change yet — observers are
   not wired.
2. **Phase 2**: port `linear_tracker` push/pull into `LinearObserver`. Run
   in shadow mode (logs both old + new derived view; alerts on disagreement)
   for one week.
3. **Phase 3**: port `github_webhook` + `vcs::poll_pr_status` into
   `GithubObserver`. Same shadow mode.
4. **Phase 4**: cut `rsry status` over to read from the derived view.
   Decommission the one-off paths.
5. **Phase 5**: drop the third observer (`BeadObserver`) into place so
   bead-internal status writes also flow through the substrate. At this
   point Dolt is no longer the SoR for status — the observation log is.

Phase 5 is the point at which Linear, GitHub, and Dolt are equal peers; the
preceding phases preserve current behavior while the substrate proves out.

## Consequences

**Wins:**
- Single conflict-detection point. No more "Linear says X, Dolt says Y, GitHub
  says Z, who's right?" — `⊤ = Conflict` with witnesses.
- New sources (Slack, Notion, calendar, fork-state) plug in via `Observer`
  with no fold changes.
- Lossless audit trail by construction (G-set of observations).
- The `notme.bot` failure mode (account flagged for aggressive polling) is
  structurally prevented by the rate budget.

**Costs:**
- Storage growth for the observation log (mitigated by per-source retention
  and cold-archive after N days).
- Read path is fold-on-read with cache; stale cache means stale UI for ~one
  reconciler tick.
- Observers must declare their algebra correctly. A mis-declared field type
  (e.g., treating a multi-value field as LWW) silently loses observations.
  Mitigated by explicit `FieldName → Algebra` registry, reviewed at PR time.

## References

- Math review (companion analysis to this ADR) — establishes which formal
  claims are load-bearing vs decorative.
- ADR-0005 (reactive persistent store) — was going to be Dolt-as-substrate;
  this ADR reframes it as observation-log-as-substrate.
- ADR-0009 (cross-repo linkage) — `WorkRef` identity model carries through.
- rosary-45518d — the prior "CRDT lattice" framing this supersedes.
- `notme.bot/why` — the shadowban incident motivating the rate budget.
