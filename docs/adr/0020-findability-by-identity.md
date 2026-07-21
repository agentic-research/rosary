# ADR-0020: Findability by identity — BeadId is a content-address, everything else is a cache

**Status:** Proposed
**Date:** 2026-07-20
**Repo:** rosary

**Relates to:**
- **The full argument:** [`docs/design/findability-by-identity.md`](../design/findability-by-identity.md) — this ADR records the *decision*; the design doc carries the analysis, the 5-why, and the invariant derivation. Read it before contesting anything here.
- Local: [ADR-0010](0010-observation-lattice.md) (observation lattice — the field-algebra half, promoted and re-homed by this decision), [ADR-0012](0012-personal-bead-substrate.md) (personal substrate — becomes the `personal` role), [ADR-0014](0014-decouple-rosary-from-bd.md) (own the store — semantics ownership is kept; storage *location* stops being truth), [ADR-0016](0016-dispatch-via-cloister.md) (dispatch mediation — agent state effects become signed observation blobs), [ADR-0019](0019-harness-is-the-licensed-runtime.md) (provider seam — unchanged, but dispatch bookkeeping converges with bead state).
- cloister (the substrate half): [cloister ADR-0003](../../../cloister/docs/adr/0003-content-addressed-bead-store.md) (content-addressed bead DAG + CAS refs — Accepted, partially built; this ADR adopts it as the storage answer), cloister ADR-0052 (the convergence record + algebra-unification mandate).
- Evidence beads (all real incidents, not hypotheticals): `rosary-05fbe0` (binary store reverted by git — state lost twice), `rosary-617010` (symlink alias missed the repo pool), `rosary-6e5fc1` (aliasing and tenancy are one bug: surface address ≠ identity), `rosary-560953` (18 beads stranded in a phantom `~/.beads/beads.db`), `rosary-75af4d` (backend chosen by mechanic, not intent). The cloister live-Dolt-store-in-git footgun (same corruption class, latent) is filed as `cloister-f1a4a3`.

## Context

A bead today *is* its rows in a per-repo store, so its address is a chain of
mutable location facts: filesystem path → `.beads/` dir → backend → tenant
config. Five real failures (evidence beads above) are one absence surfacing
five ways: the bead has no first-class **identity**, no explicit **role**, and
no **sharing semantics** derived from that role — so storage mechanics (SQLite
file semantics, Dolt server semantics, git binary-blob semantics, path string
comparison) do the job an identity model should. Design doc §1 derives seven
invariants (a–g) any fix must satisfy; §2.1 shows the ecosystem already
contains the primitives, accepted and partially built: cloister ADR-0003's
blob monoid + CAS refs, rosary ADR-0010's per-field fold, `ScopeId`, signet
certs, ley-line's content store. The design's contribution is the connection,
not new invention.

## Decision

Adopt the identity model of `docs/design/findability-by-identity.md` in full:

1. **BeadId = digest of an immutable genesis blob.** The genesis blob is the
   canonical serialization of the bead's *creation event* (`bead-genesis/v1`:
   schema, role, home claim, title, created, creator cert fingerprint,
   entropy nonce). Event-addressing, not state-addressing: the id is stable
   for the bead's whole life because genesis never changes (design doc §2.2).
   Existing `{repo}-{6hex}` ids survive unchanged as default human aliases.

2. **Current state = the DAG tip behind `refs/beads/<BeadId>`, advanced by
   CAS.** The bead's history is a commit DAG (cloister ADR-0003 Layer 1);
   the ref maps stable name → current tip digest. Retrieval is verification:
   fetch, recompute digest, compare (invariant g is structural).

3. **Facts = content-addressed signed observations.** ADR-0010's G-set is
   re-homed into the CAS: each `Observation` becomes a blob; union = blob-set
   union; dedup = digest equality; attribution = signet cert. R4b (the
   fold-as-source-of-truth flip) is a prerequisite of this ADR, not a
   parallel effort (design doc §5).

4. **Role is declared at genesis and immutable:** `canonical` |
   `coordination` | `personal`. Promotion is derivation — a new canonical
   bead with `derived_from: <coordination BeadId>` — never a mutated bit
   (design doc §3.1).

5. **Everything else DERIVES from identity + role:** sharing/replication
   policy, storage backend, git-visibility, and multi-agent coordination are
   all *materialization policy*, per the role table in design doc §3.2–3.3.
   All addressing flows through one seam: `resolve(address, context) →
   BeadId` (classify → canonicalize via `realpath` → scope via RepoId →
   alias lookup → optional verify). RepoId is itself identity (root-commit
   digest + remote-URL aliases), not a path.

## Consequences

- **The load-bearing one: SQLite/Dolt stores become rebuildable caches.**
  `.beads/beads.db`, `ephemeral.sqlite3`, and any Dolt materialization are
  derived from the log/CAS and gitignored; the only git-tracked bead artifact
  is append-only canonical-JSON text (`events.jsonl`, `merge=union`).
  Binary-in-git corruption (05fbe0, cloister-f1a4a3) becomes **structurally
  impossible** — a cache git reverts is rebuilt, not lost.
- Phantom stores stop losing beads: any materialization scan finds blobs
  carrying their own identity + `home` claim and re-homes them mechanically
  (560953's 18 strays become findable). Misfiled ≠ lost.
- Symlink aliasing and multi-tenancy collapse into the alias layer (6e5fc1):
  many addresses → one identity; one address → many identities by context.
- Backend choice stops carrying semantic weight (75af4d cannot recur): the
  governing intent is the role; the backend is a cache policy. Dolt is
  demoted to an opt-in SQL-history materialization with its own remote.
- Concurrent agents never share a mutable cell: they share an idempotent blob
  monoid and race only on a ref CAS, with per-field lattice merge on retry.
  Dispatch state effects become signed observation blobs under
  `refs/agents/<dispatch_id>`, composing with ADR-0016's receipts and
  ADR-0015's capsules.
- `connect_bead_store()` (ADR-0014) survives as the materialization manager:
  it stops *being* the truth and starts *serving* it.
- Honest costs (design doc §6.1): write-time index maintenance, real GC,
  two-phase writes (~2× implementation length per ADR-0003's own estimate),
  a forever bit-identical canonical-serialization contract across Rust and
  TS, and dedup-by-title stays a search problem (nonce in genesis).

## Alternatives rejected

Carried from design doc §6; summarized, argued fully there:

- **UUID + central registry** (§6.2): adds a mutable location instead of
  removing one; a UUID names a row but proves nothing about the bytes; every
  store-mechanic footgun survives; forfeits the already-accepted ADR-0003
  substrate.
- **Dolt everywhere, DoltHub as share plane** (§6.3): re-couples semantics to
  one engine (the ADR-0014 lesson); server infra per repo; cannot ride git
  remotes; cell-level merge is weaker than engine-free per-field lattices.
- **Pure CRDT document** (§6.4): loses history-as-object, LCA, and veto;
  litigated by ADR-0003 already and by ADR-0010's own walk-back. We take
  CRDT semantics per field, DAG history where the workflow needs it.

## Migration plan

Phased per design doc §7 — each phase pays for itself; P1–P2 are pure-rosary
with no substrate dependency. **P0 (shipped / in flight):** untrack binary
stores, JSONL export tracked, path canonicalization, SQLite-default backend;
the cloister-side Dolt-store surgery is tracked as `cloister-f1a4a3`. P0 is
procedural patching only — no identity yet — so it is not decomposed into a
phase bead here. (This section is `Implementation`-classified so BDR decomposes
each `###` phase below into a `Phase` atom → bead.)

### P1: Identity layer — genesis blob, BeadId digest, single resolve() seam

Define the `bead-genesis/v1` canonical serialization (sorted-key JSON, UTF-8,
LF, per cloister ADR-0003) and its digest → BeadId, with unit tests. Backfill a
genesis blob for every existing bead from its earliest known state (created_at,
title, creator where known; fresh entropy, flagged `backfilled: true`).
Existing `{repo}-{6hex}` ids become aliases in an alias namespace. Build the
single resolver seam `resolve(address, context) → BeadId`: unify ScopeId
parsing + `realpath` canonicalization (the `rosary-617010` fix, made mandatory
at one entry point) + `RepoId` = git root-commit digest with remote-URL
fallback aliases; delete every other path-string-keyed repo lookup
(`rosary-6e5fc1`). rosary-only. Files: `src/bead.rs`, `src/scope.rs`,
`src/bead_ops.rs`, `src/bead_sqlite/mod.rs`, `src/main.rs`. **Exit test:** the
18 phantom beads of `rosary-560953` (in `~/.beads/beads.db`) are findable and
re-homed by identity from a scan of the global store.

### P2: Log as truth, stores as caches — events.jsonl + rebuild + finish R4b

Per-repo `.beads/events.jsonl`: append-only, one canonical-JSON event per line,
`merge=union` in `.gitattributes`, git-committed — the only tracked bead
artifact. `beads.db` and `ephemeral.sqlite3` become rebuildable, gitignored
caches: implement `rsry rebuild` from the log; the post-merge hook
re-materializes. Finish R4b (`rosary-a66b3a`): flip the read path to the
ADR-0010 fold, reduce `persist_status` to one fold-driven writer, delete the
ratchet. This makes the `rosary-05fbe0` / `cloister-f1a4a3` binary-in-git
corruption class structurally impossible — a cache git reverts is rebuilt, not
lost. Files: `src/bead_sqlite/mod.rs`, `src/observation/shadow.rs`,
`src/observation/fold.rs`, `src/init.rs`, `src/reconcile/mod.rs`. **Exit
test:** `git checkout` / `stash pop` / revert against a repo with concurrent
bead writes loses zero bead state (rebuild + union-merge round-trip in CI).

### P3: CAS substrate — BlobStore/RefStore, write-then-CAS, equivalence test

Implement rosary-side `BlobStore`/`RefStore` per cloister ADR-0003's five
primitives (`put`/`get`/`has` + `cas`/`list`): local SQLite blob table and/or
ley-line content store; cloister DO in-cluster. Blobs for
genesis/commits/observations; write-all-blobs-then-CAS-the-ref as the single
linearization point (a lost CAS race retries against the new tip with a
per-field lattice merge). Cross-substrate equivalence test per cloister
ADR-0052: same bead → same digests → same fold, Rust vs workerd TS.
Digest-exchange sync replaces "which file is newer." Resolve open question 1
(SHA-256 addressing vs BLAKE3 interior). Files: `src/store.rs`,
`src/bead_sqlite/mod.rs`, `src/observation/mod.rs`. **Exit test:** the
cross-substrate equivalence test is green in CI — identical observation sets
produce identical blob digests and identical fold results on the Rust and
workerd implementations.

### P4: Roles + coordination tier — role at genesis, agent namespaces, promotion-as-derivation, GC

`role` (`canonical`/`coordination`/`personal`) lives in the genesis blob,
immutable, part of the BeadId. Dispatch writes agent run-events/comments/status
observations to `refs/agents/<dispatch_id>` coordination namespaces (cloister
ADR-0003 branch-per-agent); on verify, the reconciler folds them into the
canonical bead and CAS-advances `refs/beads/<BeadId>`. Promotion is derivation:
a new canonical bead with `derived_from: <coordination BeadId>` (BDR provenance
vocabulary), never a mutated role bit. GC policy for expired coordination
namespaces (open question 4 — interacts with ADR-0015 lineage). Ephemeral tier
keyed by identity, not path. Files: `src/bead.rs`, `src/dispatch/mod.rs`,
`src/reconcile/mod.rs`, `src/reconcile/verify.rs`,
`crates/bdr/src/provenance.rs`. **Exit test:** a full dispatch cycle writes
only to its `refs/agents/<dispatch_id>` namespace during the run, folds into
the canonical bead on verify-pass via ref CAS, and two concurrent dispatches on
the same bead converge with no lost observations.

### P5: Derived surfaces + tenancy — Linear/GitHub observer peers, ref-namespace tenancy, opt-in Dolt

Linear and GitHub complete their ADR-0010 promotion to observer peers: webhooks
and sync write content-addressed observation blobs (needs cert/attribution
mapping for inbound Linear observations — open question 6). Tenancy = ref
namespaces (`refs/tenants/<t>/aliases/…`): one surface address, many
identities, resolved by context (`rosary-6e5fc1`). Dolt offered as an opt-in
SQL-history materialization with its own remote (DoltHub/S3/self-host) — never
a place beads live. Files: `src/linear.rs`, `src/linear_tracker.rs`,
`src/serve/mod.rs`, `src/serve/github_webhook.rs`, `src/sync.rs`,
`src/store_dolt.rs`. **Exit test:** a Linear webhook state change lands as a
signed/attributed observation blob that folds into derived status (no
`persist_status` write), and the same alias resolves to different BeadIds under
two tenant contexts.

The P1→P5 beads decomposed from this section form a dependency chain (each
blocked on the previous).

## Open questions

Eight, tracked in design doc §8 — headline: (1) digest algorithm (SHA-256 for
addressing vs BLAKE3 interior — needs its own ADR), (2) RepoId under history
rewrite, (3) off-cluster ref consensus / offline race UX, (4) coordination-
tier GC policy vs ADR-0015 lineage, (5) backfilled-genesis fidelity,
(6) Linear observation attribution, (7) `home` claim vs residence (`bead
move` semantics), (8) query-index staleness budget. None block P1–P2.

## Cross-references

- Design doc (full analysis): `docs/design/findability-by-identity.md`
- Substrate half: cloister ADR-0003 (blob monoid + CAS refs, shared with the
  OCI registry); convergence + algebra-unification mandate: cloister ADR-0052
- Field algebras: ADR-0010 — one specification, two implementations
  (Rust + TS), pinned by the equivalence test (same observations → same
  digests → same fold)
- Ownership: ADR-0014 (rosary owns bead *semantics*; storage becomes
  materialization policy)
- Roles: ADR-0012 (`personal`), the ephemeral tier (`coordination`)
- Dispatch: ADR-0016 / ADR-0019 (state effects as signed observations;
  provider seam unchanged)
- Evidence: rosary-05fbe0, rosary-617010, rosary-6e5fc1, rosary-560953,
  rosary-75af4d; cloister-f1a4a3
