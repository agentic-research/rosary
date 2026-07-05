# ADR-0014: Decouple rosary from bd — speak the bead format, own the store

**Status:** Accepted
**Date:** 2026-06-24
**Supersedes:** [ADR-0013](0013-bead-substrate-layering.md) ("Adopt bd/Dolt as shared store")
**Relates to:** ADR-0010 (observation lattice), ADR-0012 (personal/root bead substrate), rosary-656967, cloister-65dfed

## Context

[ADR-0013](0013-bead-substrate-layering.md) proposed making `bd` (and its Dolt
storage) the **owning substrate** for beads — rosary would stop reading `.beads/`
directly, drive `bd … --json` for mutations, and connect to per-repo Dolt
sql-servers. Lived experience reversed that call:

1. **bd churns its storage.** `bd` v1.0.0 **removed the SQLite backend entirely**
   (Dolt-only) and has changed storage layout ~3× in ~6 months. Coupling
   rosary's storage to bd means inheriting that churn. A concrete casualty:
   `ley-line-open`'s 227 beads became unreadable *via the `bd` CLI* after the
   1.0.0 upgrade (rosary still reads them fine — see below).
1. **rosary already doesn't depend on bd.** Verified 2026-06-24: rosary invokes
   the `bd` binary **zero** times at runtime (it only ever spawns `dolt` and
   `claude`). All bead I/O goes through `connect_bead_store()` in
   `src/bead_sqlite/mod.rs`, which is pure Rust: a `DoltBeadStore` (MySQL wire to a
   per-repo dolt sql-server) when `.beads/dolt/` exists, otherwise a
   `SqliteBeadStore` reading `.beads/beads.db` directly via rusqlite. So "adopt
   bd as the owner" would have *added* a dependency rosary does not currently have.
1. **No clean Rust seam into bd.** The upstream Go package
   (`github.com/steveyegge/beads`) re-exports the model but its `Open()` requires
   CGO + embedded Dolt or a running Dolt server — i.e. depending on it re-couples
   to Dolt. There is **no official Rust binding**; DoltLite (the embedded,
   serverless engine) is a C library with **no Rust binding** either. The
   on-disk `.beads/issues.jsonl` file is **deprecated** for live integration.
1. **The stable thing is the *format*, not the tool.** Upstream documents a
   versioned bead JSON contract (`docs/JSON_SCHEMA.md`, carrying an integer
   `schema_version`, additive-change discipline). Beads is **MIT-licensed**, so
   speaking/implementing that contract ourselves is fine.

The operator's decision: **beads are a useful primitive; rosary should *speak*
the bead format without *being* (or depending on) bd.**

## Decision

### D1 — rosary's own store is the source of truth

rosary keeps owning its bead store via `connect_bead_store()`: `SqliteBeadStore`
(`.beads/beads.db`, no server, no Dolt, no git — the default for single-user
local repos) or `DoltBeadStore` (per-repo dolt sql-server over MySQL). rosary does
**not** take a runtime dependency on the `bd` binary.

### D2 — interop is via the documented bead JSON contract

"Speaking beads" means emitting/ingesting the documented bead JSON shape
(upstream `docs/JSON_SCHEMA.md`): parse leniently and additively, carry
`schema_version`. rosary's `Bead` model (`src/bead.rs`) is the internal type;
a thin (de)serialization layer maps it to/from the contract for import/export and
for any external consumer.

### D3 — `bd` is an optional adapter, never a hard dependency

If live round-trip with a `bd`-managed repo is ever required, it goes behind an
**optional adapter** that shells `bd … --json` (the upstream-recommended,
version-tolerant surface). This adapter is opt-in and isolated; nothing in
rosary's core path requires it.

### D4 — Dolt-server mode is an option, not a mandate

Per-repo Dolt server mode stays available and is the right choice **only** where
it earns its keep: concurrent writers (fan-out dispatch) or cloister/multi-machine
sharing. Single-user local repos use plain SQLite `beads.db`. Rosary must not
require every repo to run a `dolt sql-server`.

### D5 — consumers (incl. cloister) integrate through rosary, not bd's storage

External consumers that need bead data go through rosary's interface (its MCP/HTTP
API and the JSON contract), **not** a direct binding to per-repo Dolt sql-servers.
This revises [ADR-0013](0013-bead-substrate-layering.md) D4 (which had cloister
Workers bind to an external dolt sql-server). Tracked in cloister-65dfed.

## Consequences

- **Positive:** rosary is insulated from bd's storage churn; no new runtime
  dependency; single-user repos need no server process; the bead *format* (MIT,
  versioned) is the only contract we couple to; matches what the code already does.
- **Costs / risks:** rosary owns format-compatibility (must track the
  `schema_version` contract for interop); losing bd's git-backed Dolt history for
  repos that stay on plain SQLite (acceptable for single-user/local; opt into Dolt
  mode where history/concurrency matter).
- **Migration:** none forced. Repos already on SQLite keep working. Repos stranded
  by the bd-1.0.0 SQLite removal remain fully readable by rosary (it reads
  `beads.db` directly); they need bd's own export/restore only if the operator
  wants the `bd` CLI to read them again.

## Alternatives considered

- **A — adopt bd/Dolt as owning substrate ([ADR-0013](0013-bead-substrate-layering.md)).**
  Rejected: inherits bd's storage churn (v1.0.0 removed SQLite) and adds a runtime
  dependency rosary doesn't currently have, for no benefit to single-user local use.
- **B — depend on the upstream Go `beads` package.** Rejected: Dolt/CGO-bound and
  Go-only; re-couples to Dolt and is unreachable from Rust without FFI.
- **C — vendor bd / DoltLite via FFI.** Rejected for now: DoltLite has no Rust
  binding (hand-rolled C FFI only); large surface for little gain. Kept on the
  shelf only if bd-compatible *versioned in-process* storage is ever required.
