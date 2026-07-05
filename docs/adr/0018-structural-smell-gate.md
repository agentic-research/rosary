# ADR-0018: Structural smell gate via mache's committed-baseline ratchet

**Status:** Accepted
**Date:** 2026-07-05
**Relates to:** ADR-0014 (own the store), rosary-4fe0b2, rosary-d1b23c (#316/#318)
**Upstream mechanism:** mache ADR-0018 (`find-smells` `--fail-on` / `--baseline`)

## Context

Rosary enforced two structural code-size rules with hand-rolled bash:
`scripts/check-god-files.sh` (a `wc -l` ratchet vs `origin/main`) and
`scripts/check-file-length.sh` (Golden Rule 2, files > 500 lines). Both
reinvented capability that **mache** — the ecosystem's structural code-
intelligence engine — already provides: rules are JSON-of-SQL over the AST
(`_ast`/`nodes`/`node_refs`), `--baseline` is a per-(rule,file) ratchet (mache's
own ADR-0018, the "W5 ratchet"), `--fail-on` is the gate, and `--format=sarif`
emits code-scanning output. mache indexes Rust via the ley-line backend, so
`long_file`/`god_file` fire on rosary's own tree.

Maintaining a second, weaker line-counting implementation in bash is exactly the
"don't reinvent the substrate" smell. It also drifts: the bash threshold and
mache's `long_file` (1501) diverged, and mixing metrics let a borderline file
slip a prior baseline.

## Decision

Adopt mache's rule engine as rosary's structural smell gate. Concretely:

1. **Custom rosary rules live as JSON** in `docs/smell-rules/` (loaded via
   `MACHE_SMELL_RULES_DIR`). `long_file_rosary.json` re-expresses Golden Rule 2
   (`_ast`, `DefaultMinMetric: 500`) as a mache rule.
2. **`docs/smell-baseline.json`** is the committed ratchet floor (grandfathers
   current findings; new findings above baseline fail).
3. **`task smells`** runs the gate locally and is wired into `task check`
   (skip-if-absent, so a mache-less environment doesn't fail).
4. **`.github/workflows/smells.yml`** runs the identical gate in CI via mache's
   reusable `find-smells` action (SARIF → code-scanning, same class as CodeQL) —
   deliberately a separate workflow so it doesn't collide with the `task check`-
   only contract that `check-taskfile-contract.sh` enforces on `ci.yml`.
5. **Retired** `scripts/check-god-files.sh` and `check-file-length.sh` and their
   pre-commit hooks.

Exception: the `persist_status` **ratchet** (R4b, ADR-0010) stays a rosary bash
grep — it counts one function's call sites, which mache's call-extraction
under-counts (16 of 21 real sites), so bash is the accurate ratchet there.
`check-versioned-files.sh` (Golden Rule 1, a filename policy) also stays — it is
not a structural AST smell.

## Consequences

- One rule engine, one ratchet mechanism, far broader coverage than the two bash
  scripts (god_file, long_file, dead_code, duplication, …) for free.
- The baseline is regenerated with `mache find-smells … --write-baseline` after
  an intentional change and committed.
- CI depends on the mache binary + ley-line backend (the action provisions both
  at `mache-version >= v0.13.0`).
- Future rosary-specific structural rules are added as JSON, not code.
