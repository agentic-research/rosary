# ADR-0022: Bead location derives from role — canonical stays in-tree, build the missing two homes

**Status:** Accepted
**Date:** 2026-07-27
**Repo:** rosary
**Tracking bead:** `rosary-fa7167`

**Relates to:**
- [ADR-0012](0012-personal-bead-substrate.md) (**Accepted**) — personal beads: `~/.rsry/personal.db` + `SyncBackend`/`GitRepoBackend` (private git repo of `age` blobs). **Unbuilt.**
- [ADR-0020](0020-findability-by-identity.md) (Proposed) — invariant **(c)** *"sharing derives from role, not repo/backend"* is the principle this ADR applies; **P4** specifies the coordination tier (`refs/agents/<dispatch_id>`). **Unbuilt.**
- [ADR-0014](0014-decouple-rosary-from-bd.md) (Accepted) — "speak the bead format, own the store"; the tracked JSONL is the portable interop artifact. Constrains Q2.
- [ADR-0018](0018-structural-smell-gate.md), [ADR-0021](0021-single-source-bead-field-lifecycle.md) — referenced in passing.
- `docs/prior-art/state-sync-sota.md` — the survey this ADR resolves.
- `docs/design/2026-07-27-two-axes.md` — why this is the *store* axis only.

## Context

`docs/prior-art/state-sync-sota.md` surveyed five local-first work trackers and
found a strong convergence:

> Four of five systems use refs (`refs/<ns>/<id>`, COBs, `refs/dolt/data`);
> Entire uses a branch. **None puts a mutable file in the working tree. That is
> now the strongest signal in this document.**

rosary is the outlier: canonical beads live in a git-tracked
`.beads/beads.jsonl` in the working tree. That was deliberate — it buys one
benefit no other system offers, *a bead change is reviewable in the PR diff* —
and the same doc records the price:

> commit-coupling (bead churn lands in code commits), merge conflicts against
> code changes, and a hook that must stage a file

and explicitly declines to call it final:

> PR #399 should be read as the *pragmatic step that makes state text and
> mergeable*, not as the endpoint. Deciding between "reviewable in-tree" and
> "clean out-of-tree" is a real fork.

**All three predicted costs were measured biting on 2026-07-27**, in one day:
the pre-commit hook swept unrelated beads into an unrelated code branch;
`.beads/beads.jsonl` needed a merge driver to survive a routine branch merge;
and 49 beads — including an entire decade — were store-only because the hook
that stages the file cannot publish a new record.

This ADR resolves the fork on the evidence of that day.

## Decision

**Bead location derives from role.** Three roles, three homes:

| role | home | rationale |
|---|---|---|
| **canonical** | in-tree tracked `.beads/beads.jsonl` | **stays** — Q3 |
| **coordination** | `refs/agents/<dispatch_id>` | ADR-0020 P4 — invisibility is the *point* |
| **personal** | private git repo of `age` blobs | ADR-0012 — outside any project repo |

The diagnosis this ADR records is that rosary does **not** have a "where should
beads live" problem. It has **three roles crammed into one location**, because
the other two homes were designed and never built. Every symptom measured on
2026-07-27 is that single absence — the hook sweeping unrelated beads into a
code branch is *coordination state living in canonical storage*.

### Q1 — Is PR reviewability recoverable out of tree? **YES (answered empirically)**

This was the load-bearing question, because reviewability was in-tree's *only*
unique advantage. Answered by building it (`rosary-fa7167`, `src/bead_diff.rs`,
`rsry bead diff`) and running two experiments against this repo:

1. **git revs** — a readable diff rendered across `HEAD~3..HEAD`, 57 added beads.
2. **a real ref** — the JSONL written to `refs/beads/experiment` via
   `git hash-object -w` + `git update-ref`, then a full diff rendered **from the
   ref, with nothing in the working tree.**

Reviewability does not depend on the file being checked out. It is in fact a
*better* review than the raw diff, which is 1100+ lines of JSONL noise.

**Consequence: reviewability is no longer an argument for either side.** It
survives a move, so it can no longer be cited to keep beads in-tree. The
decision therefore rests entirely on Q2 and Q3.

### Q2 — Is `beads.jsonl` a contract or an implementation detail? **A CONTRACT**

Code consumers are few and all ours (rosary 6 files, ley-line-open 4, mache 2;
cloister 0, rig 0), so a move is mechanically cheap. That is not the constraint.

[ADR-0014](0014-decouple-rosary-from-bd.md)'s thesis is *"speak the bead format,
own the store"*, with the tracked JSONL as the **portable interop artifact** —
the thing that makes a bead readable without rosary installed. Moving canonical
beads into a ref namespace makes them **tool-required again**, which is
precisely what ADR-0014 moved away from after bd "churned storage 3× in 6 months
and stranded 227 beads."

**Decided: `beads.jsonl` is a contract.** Its path and format are part of
rosary's public surface, not an implementation detail, and are covered by
ADR-0014's compatibility posture.

### Q3 — Do refs survive the tools actually in use? **NO (measured)**

```
remote.origin.fetch = +refs/heads/*:refs/remotes/origin/*
```

A plain `git clone` and `actions/checkout` fetch **only** `refs/heads/*`. A
custom namespace such as `refs/beads/*` requires an explicit refspec on every
clone, every CI checkout, and every consumer. Practically:

- a fresh clone has **zero** beads until configured
- CI sees **zero** beads by default
- GitHub's UI renders nothing — no blame, no history, no diff
- it is exactly Dolt's `refs/dolt/data` tradeoff: invisible without the tool

Combined with Q2, this is disqualifying **for the canonical role**. For the
coordination role the same property is a *feature* — agent chatter should not
clutter a repo or a clone.

## Consequences

- **Canonical beads stay in-tree.** The 2026-07-27 pain is not evidence of a
  wrong location; it is evidence of two missing locations.
- **The three predicted costs remain**, and are addressed at their own level:
  commit-coupling shrinks once coordination traffic stops writing to canonical
  storage; the publish gap is `rosary-bc1918`; the merge driver stays as
  ADR-0020 P2 describes — *transitional scaffolding*.
- **`rsry bead diff` is location-neutral by construction.** It reads files,
  revs, and refs alike, so it serves the coordination and personal tiers
  unchanged, and would need one line of CI (a fetch refspec) if canonical ever
  moves. Answering Q1 produced a tool that survives the decision going either
  way.
- **The differ declares no field set**, so it cannot drift the way the seven
  hand-rolled field lists in ADR-0021 have. A field appearing or vanishing is
  itself reported.
- **Not free:** two homes must actually be built, or canonical keeps absorbing
  their traffic and this ADR is words.

## Alternatives considered

- **Move canonical to `refs/beads/*`.** Rejected on Q3 (invisible to a plain
  clone, CI, and GitHub's UI) and Q2 (breaks ADR-0014's portability). The
  four-of-five convergence in the prior-art survey is real, but those systems
  ship their own client; rosary deliberately does not require one to *read* a
  bead.
- **Move canonical to a dedicated branch** (Entire's choice). Rejected: it
  carries Q3's invisibility without the ref model's namespacing benefit, and the
  survey shows it is the minority choice among the five.
- **Move canonical to a separate private repo.** Rejected *for canonical* —
  beads describing code belong with that code, and a second repo reintroduces
  the cross-repo publish gap measured on assay (45 beads, no ledger at all).
  Retained *for personal*, which is ADR-0012.
- **Do nothing / leave all three roles in one place.** Rejected — that is the
  status quo, and it is what produced every symptom on 2026-07-27.

## Migration

1. **Publish verb** — `rosary-bc1918` (**P0**). Must work cross-repo: filing
   into a repo nobody is committing in currently writes into a void.
2. **Coordination home** — ADR-0020 P4 (`rosary-16154e`). Once dispatch writes
   run-events and comments to `refs/agents/<dispatch_id>`, the pre-commit hook
   stops seeing them and commit-coupling shrinks without touching canonical.
3. **Personal home** — ADR-0012's `SyncBackend` + `GitRepoBackend`
   (`rosary-e52b24`).
4. **Notification** — `rosary-2268aa`, so a cross-repo filing is not silent.

**Exit test for this ADR** (from `rosary-fa7167`): file a coordination-role bead
and confirm `.beads/beads.jsonl` is **unchanged**.

## Open

- **Org-level beads** — truths belonging to no single repo (`rsry-efe159`).
  Not a fourth role; likely a canonical bead in a designated repo, but undecided.
- **`--published-from` conflates two jobs.** Correct for a *public* projection
  (`rosary-b75bec`, vigil pre-release); *exposure* for a repo's own record.
  Whoever builds `publish-one` must not simply drop the flag.
- **Dual state is already implemented, unnamed.** `beads.db` is the working set,
  the tracked JSONL is the published record, `--published-from` is the
  projection between them. So `rosary-610ad8` is not "design dual state" but
  "name it and add the promote verb". The full-vs-published set difference is
  additionally a free, programmatic *"created locally, not yet shared"* signal.
