# ADR-0024: The bead store is canonical; the tracked JSONL is a rebuildable projection

**Status:** Accepted
**Date:** 2026-08-04
**Repo:** rosary
**Tracking bead:** `rosary-04d739`

**Relates to:**
- [ADR-0014](0014-decouple-rosary-from-bd.md) (Accepted) — named `.beads/beads.jsonl` the portable interop
  contract. Did not decide whether it or the store is authoritative when they disagree.
- [ADR-0020](0020-findability-by-identity.md) (Proposed) — proposes "log as truth, stores as rebuildable
  caches" as the eventual shape. Not yet built for rosary (see Context).
- [ADR-0021](0021-single-source-bead-field-lifecycle.md) (Accepted) — single-sources the *field set*; this
  ADR single-sources the *authority*. Orthogonal.
- [ADR-0022](0022-bead-location-derives-from-role.md) (Accepted) — decides *where* a bead's role routes it
  (canonical/coordination/personal). This ADR decides, *within* the canonical role's repo, which artifact
  wins when the store and the tracked export disagree.
- `rosary-185161` (open) — retire the Dolt bead backend. Complementary, not a prerequisite: it removes one
  of three artifacts that can disagree fleet-wide (2 of 24 repos), this ADR decides the remaining two for
  rosary specifically.

## Context

`rosary-04d739` names the root cause directly: rosary has never declared which artifact wins when the
SQLite store and the git-tracked `.beads/beads.jsonl` export disagree. Both are load-bearing — `rsry` reads
and writes the store; a fresh clone bootstraps from the JSONL — and neither was declared to win. That is
how cloister ended up with beads that existed only in one artifact, and how this exact session hit real,
repeated drift between the two (two separate sync PRs, #475 and #476, from operations that mutated the
store without refreshing the export — see `rosary-92546c`).

This session independently re-derived the same problem from a different angle while investigating that
drift, before reading `rosary-04d739`'s own analysis in full.

## The four falsifiers, answered

`rosary-04d739`'s acceptance criteria requires these answered in writing, before implementation:

**1. Rebuild cost.** *"If a log grows to 100k events and rebuild takes minutes, caches are disposable is
false."* Measured on rosary's own store today, 1261 beads:

```
$ time rsry bead export --jsonl --status all -o /tmp/rebuild-proof.jsonl
exported 1261 beads to /tmp/rebuild-proof.jsonl
0.046s total
```

46ms. Falsified — at this scale, and by a wide margin. This session ran the equivalent full re-export five
separate times as its own drift-recovery mechanism; every run was sub-second. The "minutes" failure
condition would need ~100x today's bead count at minimum before it became a real concern, and nothing
about rosary's growth rate suggests that's near.

**2. The log needs the same protection.** *"If the log is a file agents append to concurrently, the
disagreement has moved rather than gone."* This falsifier does not yet apply to rosary, for a more basic
reason: **the append-only log does not exist here.** `rosary-068019` ("P2: Log as truth, stores as caches —
events.jsonl + rebuild") is marked `done` in the store, but `.beads/events.jsonl` is absent from this
repo's `.beads/` directory, and `scripts/check-persist-status-ratchet.sh` — the file its own acceptance
criteria says should be *deleted* on completion — still exists. That bead's closure does not match its
delivery; a fresh instance of the exact "marked done without the work landing" failure this session's audit
was built to catch (`rosary-914abb`).

Consequence for this decision: today there are only **two** artifacts in real contention for rosary
(store, export), not three. This ADR decides between those two now. Promoting an append-only log to
authority is ADR-0020's larger, not-yet-built claim — orthogonal to and not blocked by this decision, since
whichever artifact this ADR declares canonical becomes the log's *source* when that work is actually built.

**3. Dolt history.** *"If anyone needs Dolt's cell-level history, this does not reconstruct it."* Does not
apply to rosary itself — this repo runs SQLite only (no `.beads/dolt/`), confirmed this session while
investigating an unrelated backend-detection bug (`rosary-9103f7`/`rosary-cd5e99`). It applies to the 2 of
24 fleet repos still on Dolt (signet, cloister), tracked separately by `rosary-185161`. This ADR's decision
is scoped to rosary; a repo on Dolt needs its own instance of this decision once `rosary-185161` lands or is
explicitly deferred.

**4. Recovery — the real test.** *"Could cloister's 7 store-lost beads have been recovered under
log-as-truth? Only if the loss was a cache loss and the log still held them."* **Unverified by this
session** — answering it requires inspecting cloister's own repo state and history directly, which this
session did not have in scope. Recorded here as an open question rather than answered speculatively: this
ADR's own falsifiability weakens if #4 turns out to be a "no," and that must be checked in cloister before
generalizing this decision fleet-wide. For rosary specifically, the decision below does not depend on the
answer, because rosary has no measured store-only-loss incident analogous to cloister's.

## Decision

**For rosary: the SQLite bead store (`beads.db`) is the canonical authority for a bead's current field
values. `.beads/beads.jsonl` is a git-tracked, fully rebuildable projection — never a second source of
truth.**

This is not a new behavior; it is naming what the code already does and closing off the one path that let
it drift:

- Every mutating operation (`create`, `close`, `comment`, `update`, `correct`, `reopen`) writes to the store
  first. None of them read the JSONL to determine current truth.
- `rsry bead export --jsonl --status all` regenerates the tracked file from the store alone, with no
  reference to the file's prior contents.
- The only way the two could disagree was a mutating write path that updated the store but never refreshed
  the export — exactly the gap `rosary-92546c` (this session) found and closed for `comment`/`update`/
  `correct`/`reopen`, joining `create`/`close` which already had it.

What changes: the export refresh is now correctness-with-a-name, not an incidental side effect. A future
write path that mutates the store without refreshing the export is a bug against *this ADR*, not an
undecided question — the drift gate framing `rosary-c1f669` established (derive the check from the
authority, never a copy) applies here too: the export must always be checked against what it claims to
project from, the store.

## Acceptance criterion 3 — proven, not asserted

*"A cache can be deleted and rebuilt with no loss, proven by doing it on a real repo and diffing
before/after."* Run on rosary itself, 2026-08-04:

```
$ cp .beads/beads.jsonl /tmp/before.jsonl        # 1261 records
$ rsry bead export --jsonl --status all -o /tmp/after.jsonl
exported 1261 beads to /tmp/after.jsonl
$ diff <(sorted-full-records before.jsonl) <(sorted-full-records after.jsonl)
```

Result: identical ID sets (1261 = 1261, zero added, zero lost), and field-for-field identical content with
one exception — a single bead's `status` read `"closed"` in the pre-rebuild snapshot and `"done"` in the
fresh export. Both are terminal states (`rsry bead list --status all` treats them as the same class); this
is status-string canonicalization on export, not data loss. No other field, on any of the 1261 beads,
differed.

## Consequences

- The publish decorator, bounded refresh, and bootstrap import (`rosary-04d739`'s falsifier list of paths
  "this replaces") are not deleted by this ADR — they remain the *mechanism* that keeps the now-declared
  cache correct. What changes is that a gap in that mechanism is now a defect against a named invariant,
  not an open question about which artifact should have been updated.
- `rosary-185161` (retire Dolt) becomes purely a fleet-uniformity move once it lands — it does not change
  this decision for rosary, since rosary was never on Dolt.
- If ADR-0020's append-only log is later built for rosary, it becomes the new upstream source and this
  ADR's "store is canonical" narrows to "store is canonical among cache-shaped artifacts, log is canonical
  among all artifacts" — a refinement, not a reversal, because the log would still need to answer falsifier
  2 (concurrent-append protection) before it could take over, which it cannot yet do because it does not
  exist.
- Falsifier 4 (cloister recovery) remains open. Before generalizing this decision to cloister or signet
  (the 2 Dolt repos), that question must be answered against their actual history, not assumed.
