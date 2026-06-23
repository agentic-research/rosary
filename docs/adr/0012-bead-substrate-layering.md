# ADR-0012: Bead Substrate — Layering, Storage, and Sync

**Status:** Proposed
**Date:** 2026-06-23
**Depends on:** ADR-0010 (observation lattice), ADR-0005 (reactive store)
**Relates to:** rosary-133346 (substrate eval → NDJSON), rosary-e4d471 (this ADR), rosary-b1495c (dispatch auth), rosary-3f8515 / rosary-3fcd02 (bead-ID prefix)

## Context

rosary tracks work as **beads**. Today it integrates with the upstream `bd`
tool ([steveyegge/beads](https://github.com/steveyegge/beads)) by **reaching
around `bd`'s CLI directly into its storage** (`rsry`'s own `bead_dolt` /
`bead_sqlite` readers hit `.beads/` directly). This is the wrong seam, and it
has produced concrete, recurring pain this cycle:

1. **Two ID generators on one store** → `loom-` / `rsry-` / `rosary-` prefix
   chaos (bd mints hash IDs `bd-a1b2`; rsry mints `{repo-name}-{hex}`; the
   prefix tracked the repo's renames loom→rsry→rosary). Malformed `.-xxxxxx`
   IDs from an unsanitized empty prefix (fixed: rosary-3f8515).
2. **Two store-access paths** → lectio reports 0 beads via rsry while `bd`
   shows 125: bd's source of truth is `.beads/embeddeddolt` (embedded Dolt);
   rsry read the empty `.beads/beads.db`. rsry silently reported 0 instead of
   "I can't read this backend" (rosary-21e2d4).
3. **Dolt sync is jj-incompatible.** bd/rosary's `dolt push`/`dolt pull` were
   wired as git `post-push`/`post-merge` hooks. **jj does not run git hooks**,
   so under the maintainer's jj workflow they never fire — beads silently
   diverge across machines with zero backup (rosary-133346 finding; verified
   `.beads/dolt/` is gitignored and never committed).
4. **"We need Dolt-commit here, jj there, SQLite elsewhere — but not always."**
   This is the symptom of conflating two layers that have different needs.

Two schools of issue-tracker storage frame the choice:

- **Versioned-DB school** — Dolt (bd), Fossil (SQLite). The DB *is* the
  version-control. Strong cell-merge + temporal SQL; heavy (server / 103MB
  binary / write tax); sync via Dolt remotes, **not** git/jj.
- **Append-only-log school** — [git-bug](https://github.com/git-bug/git-bug),
  [epiq](https://github.com/ljtn/epiq) (event-sourced), git-native-issue. Issues
  are an append-only event log in the VCS; current state is a fold; merge is
  append-commutativity. Travels with the repo; VCS-agnostic.

rosary already built an app-level CRDT (the ADR-0010 observation lattice). It is
the **log school**. `bd` is the **DB school**. Running bd's Dolt cell-merge
*under* rosary's lattice pays for conflict resolution twice, and the two can
fight. The mismatch — and the around-the-CLI coupling — is the root problem.

## Decision

### D1 — rosary owns the substrate; `bd` is relegated, never *below*

rosary's substrate is the source of truth. `bd` is at most an optional
**read-only viewer on top** (or dropped). rosary MUST NOT read or write bd's
store directly. If bd ergonomics are wanted, drive its **CLI** (`bd … --json`,
the intended integration seam) — never its `.beads/embeddeddolt` guts.

### D2 — Split the LOG (source of truth) from the PROJECTION (throwaway)

The "storage needs vary" tension dissolves into two layers with one rule each:

- **LOG** — an **append-only NDJSON observation log**, the single source of
  truth, **committed to git/jj on change**. This is the "commit on change"
  need — satisfied by **git/jj commit, not Dolt commit**. Versioning = git
  history; sync = `jj git push` (no hook dependency); merge = append-
  commutativity + the ADR-0010 lattice as the merge authority. Shape already
  exists as `.beads/interactions.jsonl`.
- **PROJECTION** — current state folded from the log into **throwaway SQLite**
  (or in-memory): fast queries, regenerable, no versioning, never synced.

This unifies the per-context confusion: it is **always** git/jj-committed log +
throwaway SQLite projection. There is no per-place engine switch.

### D3 — Dolt drops out

Once merge happens at the log level (append-commutativity + lattice), Dolt's
only unique value (cell-merge) is redundant. No `dolt sql-server`, no 103MB
binary, no write tax, no jj-hostile sync. (DoltLite is alpha + single-player;
not a path today.)

### D4 — bead-ID prefix is a substrate concern, resolved + sanitized by rosary

Prefix is owned by the rosary substrate, not bd. Resolve by precedence
(explicit per-repo config → repo name → git remote → dir basename), always
sanitized (rosary-3f8515). Do **not** couple prefix resolution to bd's
`.beads/config.yaml` (that is the layer being removed). rosary-3fcd02's part 2
targets the rosary-owned store, and bd reconciliation becomes moot.

## Consequences

- **Positive:** one storage/sync model; works under jj; backup + sync +
  clone-visibility for free (git history / `jj git push` / clone gets the log);
  lattice is the single merge authority; the `loom-`/divergence/0-beads class is
  fixed at the source; no Dolt operational weight.
- **Negative / costs:** append-only log grows unbounded → needs a
  compaction/snapshot strategy (cf. lectio ADR-0010 recoverable compaction);
  bead-state changes become git/jj churn in the code repo's history (merge-clean
  at least); one-time migration of existing Dolt/embeddeddolt beads into seed
  observations.
- bd-tracked repos (e.g. lectio's embeddeddolt) are migrated or read via bd's
  CLI as an interim, not via rosary's direct store readers.

## Alternatives considered

- **A — keep current (bd below rosary, direct store access).** Rejected: the
  status quo; produces all the divergence above.
- **B — rosary drives bd's CLI (bd's intended model).** Clean integration, but
  keeps Dolt → jj-hostile sync + double-merge under the lattice. Rejected for
  the same reasons bd's substrate is rejected; viable only as an interim bridge
  for repos already on bd.
- **C — keep Dolt, move sync into rosary's reconcile loop (not hooks).** Fixes
  the jj-hook problem but retains Dolt weight + double-merge. Rejected.

## Migration (high level)

1. Persist the G-set as a git/jj-tracked append-only NDJSON log.
2. Wire the ADR-0010 lattice fold as the read path.
3. SQLite projection rebuilt from the log.
4. Migrate existing Dolt / embeddeddolt beads → seed "initial observation" lines.
5. Relegate `bd` to optional CLI-driven viewer; delete dead `docs/git-hooks/`
   dolt-sync templates (rosary-130050 leftover).

## References

- rosary-133346 (substrate eval, decision toward NDJSON), rosary-e4d471 (this ADR)
- rosary-b1495c (dispatch auth — credential propagation, not TTY), rosary-3f8515 / rosary-3fcd02 (prefix), rosary-21e2d4 (fail-loud on unreadable backend)
- ADR-0010 (observation lattice), ADR-0005 (reactive store)
- bd: [ARCHITECTURE](https://github.com/steveyegge/beads/blob/main/docs/ARCHITECTURE.md) · log school: [git-bug](https://github.com/git-bug/git-bug), [epiq](https://github.com/ljtn/epiq) · DB school: [Fossil](https://en.wikipedia.org/wiki/Fossil_(software))
