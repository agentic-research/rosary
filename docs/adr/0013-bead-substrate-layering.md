# ADR-0013: Bead Substrate — Adopt bd/Dolt as Shared Store, rosary On Top (SUPERSEDED)

**Status:** Superseded by [ADR-0014](0014-decouple-rosary-from-bd.md)
**Date:** 2026-06-23 (superseded 2026-06-24)
**Depends on:** ADR-0010 (observation lattice)
**Relates to / revises:** rosary-133346 (NDJSON-substrate eval), rosary-b1495c (dispatch auth), rosary-3f8515 / rosary-3fcd02 (bead-ID prefix)

> **⚠️ SUPERSEDED (2026-06-24).** This ADR proposed coupling rosary's bead storage
> to `bd` (drive `bd … --json`, treat bd/Dolt as the owning substrate). The
> operator decided **not to couple** — beads are a useful *primitive*, but rosary
> should *speak* the bead format without *depending* on `bd` or Dolt. See
> **[ADR-0014: Decouple rosary from bd](0014-decouple-rosary-from-bd.md)** for the
> adopted decision. This file is retained for history and for the still-valid
> research it captured (DoltLite exists; the jj incompatibility was git *hooks*,
> not Dolt; server mode works for assay). Renumbered 0012 → 0013 to resolve a
> collision with [ADR-0012 (personal/root bead substrate)](0012-personal-bead-substrate.md).

## Context

This ADR was first drafted as "drop Dolt, make an append-only NDJSON observation
log the source of truth, fold via the ADR-0010 lattice, project to SQLite." A
2026-06-23 research pass (4 agents, primary sources) **overturned the premises**
of that draft, and a live audit of the fleet confirmed it. The corrected
findings:

1. **"Dolt is jj-incompatible" — wrong.** jj doesn't run *git hooks*; we wired
   `dolt push` into hooks. `DOLT_PUSH()`/`DOLT_PULL()` are SQL procedures, and
   `bd` already auto-pushes from app code, not hooks. The fix is to drive sync
   from rosary's loop — not to drop Dolt. ([jj #403](https://github.com/jj-vcs/jj/discussions/403), [Dolt procedures](https://www.dolthub.com/docs/sql-reference/version-control/dolt-sql-procedures))
1. **"Dolt is heavy (server, 103MB, write tax)" — overstated.** Embedded
   mode / DoltLite is a library; write perf ~matches MySQL; the server is a
   deployment choice, not inherent.
1. **The "rsry sees 0, bd sees 125" bug was ours:** we read `.beads/beads.db`
   (a SQLite file `bd` *deleted* at v0.57.0), instead of driving `bd --json` /
   connecting to the Dolt server. bd's docs say plainly: drive the CLI, don't
   read the store. The `loom-`/prefix chaos is the same root cause (co-mingling
   at the storage layer).
1. **Flat NDJSON at rest is a documented anti-pattern.** Every mature system
   (Fossil→SQLite, git-bug→objects+cache, Kafka→segmented+indexed) materializes
   into an indexed store; [Grite](https://github.com/neul-labs/grite) built our
   exact "git log + CRDT fold" design and was forced to add an embedded DB +
   snapshots. The maintainer's "JSON rows in SQLite" instinct is best practice.
1. **bd is shared-infrastructure by design** — `bd setup <agent>`, `BEADS_DIR`
   per-repo/monorepo isolation, git-free/`--stealth` (`no-git-ops`), `bd backup`
   migrate/restore, an MCP server, and `bd ready/create/update --claim/dep/ prime/remember` — i.e. exactly the orchestrator surface rosary reimplemented.
1. **Server mode over a Unix socket** gives concurrent writers, is
   sandbox-friendly ("file-level access control simpler than network
   allowlists — e.g. Claude Code", per the bd README), and lets rosary connect
   **via the MySQL client it already speaks** — so there is **no Rust-FFI
   blocker** (the one genuine reason the research found to hesitate on Dolt).

**Decisive live evidence:** the fleet already runs this. `assay` (and ~13 other
repos) use **bd server-mode Dolt**; `rsry` reads them over MySQL today
(`rsry_list_beads(repo=assay)` → 24 open, prefix correct, live ESTABLISHED
connections). **lectio is the outlier** (embedded Dolt) that rsry can't read.
So "adopt bd server-mode as the shared substrate" is not a new build — it is the
fleet's existing, working state, with lectio the exception to fix.

## Decision

### D1 — Adopt bd/Dolt as the shared bead substrate; rosary sits on top

Do not reinvent storage/merge. `bd` (Dolt) owns the bead store and bead IDs.
rosary integrates through the **supported seams**: drive `bd … --json` for
mutations, and read over **MySQL** from the dolt sql-server (as it already does
for assay et al.). rosary MUST stop reading `.beads/` files directly and stop
minting parallel `{repo}-{hex}` IDs into bd's namespace.

### D2 — Deployment: server mode over Unix socket (for orchestrated/sandboxed use)

Use `bd init --server` with `--server-socket` / `BEADS_DOLT_SERVER_SOCKET` where
concurrent writers (rosary fan-out dispatch) or sandboxing (Claude Code) matter.
Embedded mode is fine for simple single-writer local use; orchestrated repos run
server mode (the assay state). lectio migrates embedded → server.

### D3 — Sync from app code, never git hooks (jj-safe)

`bd dolt push`/`pull` (or `DOLT_PUSH/PULL`) driven from rosary's reconcile loop /
bd's background auto-push — not git hooks. This is the real jj fix and is needed
regardless of backend. Beads ride `refs/dolt/data` (git-backed remote), so a
clone + `bd dolt pull` brings them.

### D4 — cloister-packaged tools connect to an external dolt sql-server via binding

Embedded Dolt cannot run inside a workerd/v8 isolate (no local FS, no in-isolate
process). cloister-packaged Workers connect to an **external** dolt sql-server
through a cloister **service binding**, with cloister mediating credentials
(its purpose). This reinforces D2 (server mode), not in tension with it.

### D5 — Migration via `bd backup`

Use `bd backup init/sync` + `bd init [--server]` + `bd backup restore` to move
repos between modes and to repair stranded stores (lectio's embeddeddolt; any
stale `beads.db`). Pin the bead prefix in `.beads/config.yaml` `issue-prefix` so
bd mints the right namespace (fixes `loom-` at the source; both tools then agree).

### D6 — rosary's observation lattice (ADR-0010) is ON TOP, multi-source only

rosary's genuinely-novel value is the **cross-repo + multi-source observation
lattice** — beads are one of many sources (Linear, GitHub, git, sessions). That
is a **separate** store/concern from the per-repo bead DB, and is the **only**
place a custom semantic merge (set-union / chain-max / per-field join-semilattice)
is justified — because Dolt's merge is *syntactic* (cell compare) and cannot
compute a semantic join across heterogeneous sources. It is **not** a beads
replacement.

### D7 — bead-ID prefix is bd's concern

bd owns `bd-<hash>` (or the configured `issue-prefix`) IDs. rosary stops minting
`{repo}-{hex}` for beads. This **re-scopes** rosary-3fcd02: the prefix lives in
bd (`.beads/config.yaml`), not a rosary central config. rosary-3f8515's
sanitizer stays useful for any rosary-owned (non-bead, lattice) IDs.

## Consequences

- **Positive:** fixes `loom-`/0-beads at the source; deletes rosary's
  store-reading code; no Rust FFI; concurrent writers; sandbox- and
  cloister-compatible; rides a maintained 17k-star ecosystem; **matches the
  fleet's already-working assay state.**
- **Costs / risks:** ride bd's release cadence (it changed storage layout 3× in
  ~6 months — pin a version; driving the CLI insulates from churn). bd is
  single-DB-per-repo and prescribes a small live working set (\<~500) — cross-repo
  aggregation stays rosary's job (it already does this). bd footguns
  (`bd doctor --fix`) — avoid.
- **Revises rosary-133346:** its jj+merge *symptom* analysis was right, but its
  *conclusion* (drop Dolt) rested on the overstated premises above; corrected here.

## Alternatives considered

- **A — build our own NDJSON-log + lattice substrate (the prior 0012 draft).**
  Rejected: reinvents Dolt's storage + 3-way merge; flat NDJSON at rest is a
  documented anti-pattern (Grite tried it and retreated to an embedded DB);
  the FFI/maturity risks are only "avoided" by also rebuilding everything.
- **B — embedded bd everywhere.** Rejected: single-writer, and won't run inside
  cloister isolates.
- **C — keep reading bd's store directly (status quo).** Rejected: the bug.
- **D — keep Dolt but move sync into hooks differently.** Subsumed by D3.

### D8 — Load-bearing invariants (from the theoretical-foundations review, 2026-06-23)

The layering in D6 is sound **only while these hold** — make them explicit and
enforced, not incidental:

1. **Observations are write-once.** The `observations` table is append-only,
   content-addressed on `(source, source_event_id, payload_hash)`, never
   `UPDATE`d. This is *why* Dolt's syntactic cell-merge of the log and the
   lattice's semantic G-set join **coincide** (both are set-union on new rows,
   no-op on existing) — the thing that makes the two layers not fight.
1. **Dolt merges only the append-only log, never the derived projection.** If
   Dolt ever 3-way-merges the materialized view, it can fabricate a state no
   source emitted. The projection is derived and disposable; only the log syncs.
1. **Compaction must be join-preserving.** Retention/compaction may not delete
   observations that change the fold (the chain-max, the LWW winner, live G-set
   members); it must first write a synthetic *dominating snapshot* observation.
1. **Pin/version the `payload_hash` canonicalization** — changing it changes
   dedup identity and double-counts on replay.

## Theoretical review outcome (Thread #5 — resolved)

The theoretical-foundations review (2026-06-23) **validates the architecture**:
the per-field algebras (chain-max, LWW-register, G-set, flat-lattice) are genuine
join-semilattices, the cross-field composition converges (pointwise product), and
the lattice-on-top-of-Dolt layering is coherent *given D8*. The lattice is
justified — it computes a semantic join (cross-source set-union, monotone
chain-max) that Dolt's syntactic cell-merge cannot. The framing is honest (the
ADRs already disavow the decorative overclaims).

Two real correctness defects were found in the *existing* `src/observation/`
implementation (independent of this ADR) and filed as beads:

- **LWW tie-break is not strict-total** (`algebra_lww.rs`) — distinct values
  sharing `(observed_at, source)` resolve by first-seen → cross-machine
  divergence. Fix: extend the key with `payload_hash`.
- **Status pre-fold ignores `observed_at`** (`fold.rs`) — comment claims
  "LWW by observed_at" but resolves by iteration order → can surface stale
  status. Fix: resolve within-source by `observed_at` LWW.

Plus minor: rename G-set vs "OR-set"; the invariant-14 partition test is
tautological (fix or downgrade); the D8 invariants above.

## References

- Research (2026-06-23): [DoltLite](https://www.dolthub.com/blog/2026-03-25-doltlite/) · [Dolt SQL procedures](https://www.dolthub.com/docs/sql-reference/version-control/dolt-sql-procedures) · [bd README](https://github.com/gastownhall/beads) / [ARCHITECTURE](https://github.com/steveyegge/beads/blob/main/docs/ARCHITECTURE.md) · [jj #403 (no git hooks)](https://github.com/jj-vcs/jj/discussions/403) · [Grite (tried log+CRDT, added DB)](https://github.com/neul-labs/grite) · [Kafka log storage](https://notes.stephenholiday.com/Kafka.pdf)
- Live audit: assay (healthy server-mode), lectio (embedded outlier), rosary-b1495c (dispatch), rosary-3f8515 / rosary-3fcd02 (prefix), rosary-133346 (revised)
- ADR-0010 (observation lattice)
