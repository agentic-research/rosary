# ADR-0015: Execution-lineage capsules — a durable, resumable, proof-ready envelope for one orchestration attempt

**Status:** Proposed
**Date:** 2026-07-01
**Relates to:** [ADR-0010](0010-observation-lattice.md) (observation lattice / fold), [ADR-0011](0011-decision-of-record.md) (authenticated authority), [ADR-0014](0014-decouple-rosary-from-bd.md) (rosary owns the store), APAS (`docs/design/agent-provenance-standard.md`), `docs/design/codex-claude-dispatch-parity.md`

## Context

Rosary runs **nested orchestration**: a human plus a parent loop start a feature;
rosary then dispatches its own agents into isolated worktrees, advances them
through a phase pipeline, verifies, and folds results back. The state of one
in-flight attempt is today split across two homes with **opposite lifetimes**:

| What an attempt needs                             | Where it lives today                             | Durable? |
| ------------------------------------------------- | ------------------------------------------------ | -------- |
| Where in the pipeline (phase, status, retries)    | `PipelineState` in `~/.rsry/backend.db`          | yes      |
| Who ran / provider session                        | `DispatchRecord` (+`AgentSessionRef`)            | yes      |
| Fine-grained events ("must survive interruption") | `AgentRunEvent`                                  | yes      |
| What happened / decisions (the handoff chain)     | `.rsry-handoff-{phase}.json` **in the worktree** | **no**   |
| The orchestrator plan                             | `.rsry-orchestrator.json` **in the worktree**    | **no**   |
| Proof envelopes                                   | DSSE files **in the worktree** (`src/dsse.rs`)   | **no**   |

The failure mode this produces, confirmed in the recovery path:

1. `recover_stuck_beads()` restores only the **phase pointer** "so it resumes at
   the correct phase (not phase 0)" (`src/reconcile/persistence.rs:115`).
1. But every context read is `Handoff::read_from(&ws.work_dir, …)`
   (`src/reconcile/verify.rs:241`, `src/reconcile/workspace_ops.rs:87`), and
   `recover_orchestrators()` rehydrates the `FeatureOrchestrator` by reading
   `.rsry-orchestrator.json` **from the worktree** (`src/reconcile/orchestration.rs:282`).

So if the worktree is lost, resume knows *where* it stopped but not enough about
*what happened* to continue cleanly. The **worktree is doing two jobs at once** —
disposable execution surface **and** durable checkpoint/context store — and those
have incompatible lifetimes.

A second, related gap: APAS's hash chain (`Handoff::chain_hash`, shipped PR #117)
is a **forensic** read. When integrity breaks it *surfaces an alarm* ("chain
invalid"), but it does not offer a **continuation point** ("last good checkpoint +
accumulated context + next action"). Detecting a break is not the same operation
as resuming from one, and today only the forensic read exists.

The naïve fix — "persist a checkpoint blob out of the worktree" — under-shoots.
Optimizing only for resume yields opaque blobs; what we actually want is **typed,
hash-linked, authority-scoped, foldable** execution records, so that proof and
authority are not a later retrofit (which is how schema drift starts).

## Decision

### D1 — The primitive is a durable *execution-lineage capsule*, not a queue item, an audit log, or a crash checkpoint alone

A **Capsule** is the durable lineage record for **one bounded orchestration
attempt**. It is the *temporal/evidentiary* object, orthogonal to the structural
Decade ⊃ Thread ⊃ Bead hierarchy:

```
Decade ⊃ Thread ⊃ Bead     structural — what work, durable authority
              Capsule        temporal   — one bounded attempt at a bead's pipeline
              projections:   resume · proof (APAS) · authority (grant) · fold
```

Resume, proof, authority, and fold are **peer projections over the same capsule**,
each answering a distinct question:

```
Can I resume this?              → continuation projection
Can I prove what happened?      → APAS proof projection
Can I verify authority/scope?   → PlanGrant projection
Can I fold work back to root?   → merge/import projection
```

### D2 — One capsule per bead-orchestration attempt, spanning the phase pipeline

Granularity is deliberately the **attempt**, not the dispatch and not the thread:

- **Per-dispatch would be too small** — it loses the cross-phase story
  (scoping → dev → staging → prod) that resume, proof, and fold all need.
- **Per-thread/decade would be too large** — it becomes another giant context
  bucket, the very thing we are trying to avoid.

Per-phase **runs** live *inside* one capsule as lineage events. Capsule identity
follows generation semantics:

```
Same bead + same orchestration attempt         → same capsule
Retry within same plan/grant                   → same capsule, new phase-run event
Non-material plan clarification (same scope)    → same capsule, plan_revision event
Material rescope / expanded authority /         → new capsule GENERATION
  changed workspace base commit                     (or child capsule)
```

The generation boundary is drawn at **material** change only — a rescope, an
expansion of granted authority, or a new base commit. A plan edit that merely
clarifies within the existing scope is a `plan_revision` event on the same
capsule, not a new generation. This prevents generation spam from routine
clarifications.

### D3 — Home is the orchestrator store, with explicit capsule tables

Source of truth for capsule metadata, state, checkpoints, phase lineage, and fold
status is the orchestrator store (`~/.rsry/backend.db`; Dolt-backed where
configured). We add an **explicit, first-class lineage seam** — `capsules`,
`capsule_events`, `capsule_artifacts` — even though those rows *reference*
existing `PipelineState`, `DispatchRecord`, `AgentRunEvent`, and observations.

> The capsule is a first-class **lineage key**, not an implicit join convention.
> We do not cram capsule state opportunistically into existing tables.

Large artifacts (stream logs, diffs, relocated handoffs) are **content-addressed
blobs referenced by hash**, not inlined. A filesystem `.jobs/<id>/` bundle is a
**later export/portable format (V2), not the V1 authority** — the diagnosed bug is
worktree-coupled context, not the absence of a filesystem bundle.

### D4 — Worktree disposable, capsule durable, resume = rehydrate

```
worktree = disposable execution surface (may be nuked and rebuilt)
capsule  = durable context + checkpoint store, OUTSIDE the worktree
resume   = rehydrate a fresh worktree from workspace_base_commit + capsule context
```

Losing a worktree must not lose: phase handoffs, agent stream summaries,
tool/file-change evidence, the orchestrator plan, the last good checkpoint, the
pending next action, or the parent-loop handoff context. The context reads in
`verify.rs` / `workspace_ops.rs` / `orchestration.rs` move from `work_dir` to the
capsule; the worktree keeps only the working tree.

### D5 — Proof and authority are load-bearing *schema* constraints in V1 (signing is V2)

V1 does **not** require DSSE/Signet signing. It **does** require the record be
shaped so APAS can verify it later without a retrofit:

- Lineage events are **hash-linked** — each `event_hash` covers `event_seq`,
  `prev_event_hash`, the canonical payload, and any referenced artifact
  digest — using **SHA-256**, per APAS §4. BLAKE3 (adopted for CAS in PR #232)
  remains the **content-addressing** hash for blob storage; the two serve
  different masters and the boundary is documented, not blurred. **Any artifact
  pulled into the APAS chain therefore carries BOTH digests**: `cas_hash_blake3`
  (how the blob is stored/fetched) and `apas_hash_sha256` (what the event chain
  hashes). The event hash MUST use the SHA-256 digest — never the BLAKE3 one —
  so the chain stays APAS-verifiable without a re-hash step.
- **`event_hash` is defined explicitly**, so "canonical" is specified not assumed:
  ```
  event_hash = SHA-256( domain_separator || capsule_schema_version
                        || canonical_payload_bytes || sorted(artifact apas_hash_sha256)
                        || prev_event_hash )
  ```
- Hashing is over a **canonical serialization** (see D8). Hashing ad-hoc JSON is
  fragile — key ordering and whitespace make the same record produce different
  bytes. A stable chain requires stable bytes, which is why the payload encoding is
  a decision, not an implementation detail.
- Authority is **schema-present**: every phase-run carries `grant_id | permission_profile` and a `plan_hash` / `context_pack_hash` anchor. Enforcement
  (turning `permission_profile` from advisory into a real boundary — the parity
  doc's "prompt wording is not a permission boundary") lands in **V1.5**.
- APAS's `dispatch/v1` predicate — canonical type
  **`https://notme.bot/provenance/dispatch/v1`** (the `notme.bot` namespace, per
  the canonical spec at `signet/docs/apas/agent-provenance-standard.md`) — is the
  **proof projection over the very same capsule records**, not a parallel system.
  Its predicate fields (work-item ref, pipeline phases, agent/provider/model/
  permission-profile, orchestrator identity, execution, work, verification, cost,
  outcome, handoff chain) overlap the capsule schema almost exactly.

> **Layering, stated explicitly:** a capsule is **not** itself an APAS
> attestation — it is Rosary's durable execution-lineage substrate, from which
> APAS `dispatch/v1` attestations are *projected*. Capsule = operational truth;
> APAS = the verifiable proof view. (APAS L1 = forensic audit trail, does not
> prove non-tampering; L2 adds DSSE/in-toto signatures — which is exactly the
> V1-shape / V2-signing split above.)

### D6 — Vocabulary reconciliation: Capsule subsumes the scattered fragments

The "Job" framing from `docs/design/codex-claude-dispatch-parity.md` and the
job-capsule handoff maps onto existing rosary nouns — we adopt **one** name per
concept rather than a parallel vocabulary:

| External / "Job" term            | Rosary term                               | Status                                                   |
| -------------------------------- | ----------------------------------------- | -------------------------------------------------------- |
| Job / execution envelope         | **Capsule**                               | new (unifying key)                                       |
| AgentRun (one phase of one bead) | capsule **phase-run**                     | existing (`PipelineState` phase + `DispatchRecord`)      |
| Event log (hash-chained)         | `capsule_events`                          | existing shape (`AgentRunEvent`) relocated + hash-linked |
| RunProof                         | **proof projection** (APAS `dispatch/v1`) | existing standard                                        |
| ContextPack                      | prompt/handoff context, hash-anchored     | existing (`Handoff::format_for_prompt`)                  |
| PlanGrant                        | **authority projection**                  | schema V1, enforce V1.5                                  |
| Fold                             | `fold_status` + `observation/fold.rs`     | existing, extended to import                             |
| Job-local bead store             | —                                         | **explicitly not V1** (see D7)                           |

### D7 — A job-local bead subledger is explicitly *not* the first primitive

The immediate pain is stranded in-flight context, not job-local task
decomposition. A per-capsule `.beads/beads.db` subledger (and selective
fold/import of its beads) is deferred to **V2**. Building it first would recreate
the "one object wearing two hats" confusion this ADR exists to resolve.

### D8 — Typed capsule records are Cap'n Proto; SQLite is the index, not the schema

The store (D3) is the **index and query surface**; the **payload schema** is
Cap'n Proto — already a first-class rosary dependency (`capnp`/`capnpc`,
`build.rs` codegen, `src/serve/ipc.rs`). Concretely:

- Capsule manifest, lineage-event payloads, self-narrated handoffs, and the proof
  predicate are defined as `.capnp` structs. `capsule_events` stores the
  **canonical capnp bytes**; the SQL columns (`event_type`, `event_seq`,
  `event_hash`, …) are **queryable projections** of those bytes, not the source of
  truth.
- **Canonical capnp is what gets hashed** (D5) — deterministic bytes → stable
  APAS SHA-256 chain — and **what gets signed later** (V2). This is exactly the
  "passkey-signed canonical capnp payloads are the portable trust unit" model from
  [`capnp-issue-type-substrate.md`](../design/capnp-issue-type-substrate.md).
  "Canonical" means the single-segment, unpacked, canonical Cap'n Proto encoding
  (`Builder::set_root_canonical` / capnp canonical form), matching the encoding
  ley-line-open + cloister already pin (LLO ADR-0014).
- **Golden test vectors are mandatory, not optional.** Each capnp payload type
  MUST ship committed golden vectors proving (a) stable canonical bytes across
  encode/decode round-trips and (b) a stable SHA-256 over those bytes. Without
  them, "canonical" is an assumption that silently rots the moment a toolchain or
  field-order changes; the vectors turn it into a test.
- That doc's **self-narrated handoff** *is* the capsule's primary lineage-event
  payload. We do not build a second handoff mechanism: capsule = the durable
  envelope, the capnp self-narrated handoff = the resume/trust/federation payload
  inside it. One mechanism, three uses — now with a durable home.

Schema evolution rides capnp's additive-field discipline (mirrors the bead JSON
contract's `schema_version` in [ADR-0014](0014-decouple-rosary-from-bd.md)), so
old capsules stay readable as the record grows.

## Architecture

### Capsule schema (sketch)

> **`capsule_events` is the source of lineage truth; the `capsules` row is a
> materialized projection** — the latest fold over the event stream, exactly the
> ADR-0010 relationship (observation G-set → derived status). If a `capsules`
> column ever disagrees with the events (`phase` vs `phase_run`, `fold_status` vs
> `fold`), **the event stream wins** and the row is re-materialized. The columns
> exist for cheap querying, never as independent truth.

```
capsules                    -- MATERIALIZED PROJECTION of capsule_events (latest fold)
  capsule_id          text primary key
  generation          integer not null default 0   -- D2 generation semantics
  parent_capsule_id   text                          -- child capsule on rescope
  parent_bead_ref     text not null                 -- WorkRef (repo/scope/bead)
  thread_id           text
  phase               integer not null              -- current phase (projected from phase_run events)
  agent               text                          -- current agent persona
  provider            text
  workspace_base_commit text not null               -- D4 rehydrate anchor
  context_pack_hash   text                          -- APAS input anchor
  plan_hash           text                          -- authority anchor
  grant_id            text                          -- D5; permission_profile fallback
  permission_profile  text
  -- versions: required for replay when pipeline/prompt/schema changed since capture
  pipeline_version        text not null             -- agent-sequence shape at capture
  prompt_version          text not null             -- prompt-assembly version at capture
  capsule_schema_version  text not null             -- capnp payload schema version (hashed, D5)
  context_pack_schema_version text not null
  last_good_checkpoint text                          -- resume descriptor
  resume_hint         text
  parent_handoff_summary text                        -- deliberate handoff for the parent loop (D-parent)
  fold_status         text not null default 'pending' -- see fold-status table below
  created_at          text not null
  updated_at          text not null

capsule_events            -- APPEND-ONLY, hash-linked lineage — the SOURCE OF TRUTH
  capsule_id          text not null references capsules(capsule_id)
  event_seq           integer not null
  event_type          text not null                 -- phase_run|plan_revision|checkpoint|tool_call|
                                                     --   file_change|verdict|parent_handoff|fold
  actor               text not null
  payload_capnp       blob not null                 -- D8: canonical Cap'n Proto bytes (the record)
  prev_event_hash     text                          -- SHA-256 chain over canonical bytes (APAS §4)
  event_hash          text not null
  created_at          text not null
  primary key (capsule_id, event_seq)
  -- event_type/actor/created_at are PROJECTIONS of payload_capnp for querying (D8)

capsule_artifacts         -- content-addressed pointers, not inlined blobs
  capsule_id          text not null references capsules(capsule_id)
  kind                text not null                 -- stream_log|diff|handoff|manifest
  content_hash        text not null                 -- BLAKE3 CAS address
  uri                 text
  created_at          text not null
  primary key (capsule_id, content_hash)
```

**`fold_status` semantics** (the fold projection of D3):

| value      | meaning                                                                          |
| ---------- | -------------------------------------------------------------------------------- |
| `pending`  | proof/result captured, not yet folded by root                                    |
| `accepted` | root accepted the result and updated/closed the parent bead                      |
| `rejected` | root rejected the result                                                         |
| `partial`  | root accepted some artifacts / filed follow-ups but did **not** close the parent |
| `imported` | an external/portable capsule imported into the local store (**V2** — reserved)   |

**Parent-loop handoff** — the field/event that closes the original nested-orchestration
pain. A capsule emits a deliberate `parent_handoff` event (projected to
`parent_handoff_summary`): the artifact the parent loop grabs to resume, take
over, or fold **without** replaying raw events. It is *authored* at pause /
near-close / interrupt, not reconstructed after the fact.

### Two reads of one record

- **Resumption read** — walk `capsule_events` to `last_good_checkpoint`, load
  `resume_hint` + accumulated handoff context, rehydrate a worktree at
  `workspace_base_commit`, continue at `phase`.
- **Forensic read** — verify the `prev_event_hash` chain and project the capsule
  into an APAS `dispatch/v1` predicate for external verification.

## Migration

The worktree files do not vanish on day one. During migration the orchestrator
**dual-writes**: capsule store first (source of truth), then the existing
`.rsry-handoff-*.json`, `.rsry-orchestrator.json`, `.rsry-stream.jsonl`, and DSSE
files as **mirrors/caches**. Readers prefer the capsule store and fall back to
worktree files only for capsules created before the cutover (pre-migration runs
have no capsule rows). Once no in-flight run predates the cutover, the worktree
writes become artifact exports (D3) rather than a parallel store.

## Phasing

| Phase    | Scope                                                                                                                                                                                                                       |
| -------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **V1**   | Durable capsule + `capsules`/`capsule_events`/`capsule_artifacts` seam; hash-linked events (SHA-256); relocate handoff + plan out of the worktree; resume-from-capsule; `fold_status`. APAS-verifiable *shape*, no signing. |
| **V1.5** | PlanGrant enforcement + stricter scope validation (permission_profile becomes a real boundary).                                                                                                                             |
| **V2**   | DSSE/APAS signing (signet bridge cert + ley-line CMS); optional job-local `.beads/` subledger + selective fold/import; `.jobs/` portable export bundle; remote capsule store.                                               |
| **V3**   | Portable/remote capsules, cross-machine replay.                                                                                                                                                                             |

**V1 implementation order** (each step lands independently):

1. Capsule tables + schema/version fields.
1. Write handoffs + orchestrator plan to the capsule store first, worktree mirror second (Migration).
1. Event hash chain **with golden test vectors** (D1/D5).
1. Resume-from-capsule path.
1. `fold_status` + `parent_handoff_summary`.
1. *Only then* PlanGrant enforcement (V1.5) or DSSE signing (V2).

> **Non-negotiable:** `capsule_events` are **typed and hash-linked from day one**.
> Everything else can phase in, but a capsule whose events aren't typed+chained
> from the first write can never be retrofitted into a verifiable one.

## Consequences

- Worktree loss stops being catastrophic: an attempt can be rehydrated from the
  capsule. Crash recovery restores context, not just a phase pointer.
- The parent loop gets a **single durable handle** per attempt — it can resume,
  take over, or fold a partial result across the nesting boundary without
  spelunking a dead worktree.
- APAS advances from "forensic, worktree-resident" toward L1-complete: the proof
  projection reads durable, relocated, hash-linked records.
- New write path: the orchestrator must write handoff/plan/events into the capsule
  seam. Existing `.rsry-*` worktree files become a cache/mirror, not the source of
  truth, during migration.
- One more store surface to back up/restore (`rsry bead backup` analog for
  capsules) and to reason about in the Dolt-vs-SQLite backend split.
- New `.capnp` schemas + `capnpc` codegen in `build.rs` (D8); the capsule record
  becomes cross-language and hash/sign-ready, at the cost of a schema-evolution
  discipline (additive fields only).

## Alternatives considered

- **Filesystem `.jobs/<id>/` bundle as V1 source of truth.** Rejected: the bug is
  worktree-coupled context, not lack of a bundle. A bundle is the right *export*
  format later, but making it authoritative reintroduces a filesystem-lifetime
  coupling.
- **Capsule per dispatch.** Rejected (D2): too small; loses the cross-phase story.
- **Capsule per thread/decade.** Rejected (D2): too large; becomes a giant context
  bucket.
- **Resume-only checkpoint blobs, proof retrofitted later.** Rejected (D5):
  produces opaque blobs and guarantees schema drift when proof/authority are added.
- **Adopt the full "Job/AgentRun/RunProof/PlanGrant" vocabulary wholesale.**
  Rejected (D6): creates two names per concept — a worse version of the confusion
  being solved.

## What we are explicitly **not** claiming

- Not tamper-*proof*. V1 is tamper-*evident in shape* only; without signing (V2) a
  compromised orchestrator can still forge records (APAS §2, L1/L2 caveats).
- Not an authority boundary in V1. `permission_profile`/`grant_id` are recorded,
  not enforced, until V1.5.
- Not a job-local task tracker in V1 (D7).

## References

- `src/reconcile/persistence.rs` — `recover_stuck_beads()` (phase-pointer resume)
- `src/reconcile/orchestration.rs` — `recover_orchestrators()` (worktree-resident plan)
- `src/reconcile/verify.rs`, `src/reconcile/workspace_ops.rs` — `Handoff::read_from(work_dir)`
- `src/handoff.rs` — `Handoff::chain_hash` (forensic chain, PR #117 / #130)
- `src/store.rs` — `PipelineState`, `DispatchRecord`, `AgentRunEvent`
- `src/observation/fold.rs`, `src/observation/log_sqlite.rs` — fold + persistent G-set
- APAS (proof projection) — canonical: `signet/docs/apas/agent-provenance-standard.md`;
  namespace `notme.bot` (predicate `https://notme.bot/provenance/dispatch/v1`);
  local mirror `docs/design/agent-provenance-standard.md` (note: local copy still
  uses the stale `rosary.bot` namespace — reconcile to `notme.bot`)
- `docs/design/codex-claude-dispatch-parity.md` — provider-neutral run/grant contract
- `docs/design/capnp-issue-type-substrate.md` — canonical capnp payloads + self-narrated handoffs (D8 sibling)
- `Cargo.toml` (`capnp`/`capnpc` 0.21), `build.rs`, `src/serve/ipc.rs` — existing capnp toolchain
