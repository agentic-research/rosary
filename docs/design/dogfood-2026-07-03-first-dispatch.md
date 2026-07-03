# First dogfood dispatch — findings (2026-07-03)

Status: findings log
Relates: rosary-a66b3a (R4b), rosary-818ed4 (terminal-fail fold, closed),
rosary-fafb7c (pipeline progression)

## What we ran

```bash
RSRY_LATTICE_SHADOW=1 rsry run --once --bead rosary-765f42 --concurrency 1
```

A single, scoped dispatch (claude, authenticating via macOS Keychain) — the
first real end-to-end run since the observation write-path (R4b step 1) landed.
Two goals: validate the full pipeline, and grow the `rsry lattice audit` corpus
past N=1.

## What worked — the full pipeline, end-to-end, live

The complete **scan → triage → worktree → dispatch → verify → advance → retry**
loop ran correctly. From the shadow-fold trace:

```
recorded=Dispatched folded=Dispatched (1)   scoping-agent (phase 1) dispatched
recorded=Verifying  folded=Verifying  (2)   scoping completed, verifying
recorded=Pass       folded=Pass       (3)   scoping PASSED → phase advanced
recorded=Dispatched folded=Pass       (4)   dev-agent (phase 2) dispatched
recorded=Verifying  folded=Pass       (5)   dev completed, verifying
recorded=Fail       folded=Pass       (6)   dev verify FAILED
recorded=Dispatched folded=Pass       (7)   dev re-dispatched (retry)
recorded=Verifying  folded=Pass       (8)   ...
recorded=Fail       folded=Pass       (9)   dev retry failed again
```

- **Phase advancement on Pass** (scoping → dev) — proves rosary-fafb7c ("dispatch
  must progress the pipeline").
- **Retry on Fail** — the dev-agent's change failed verification, so it was
  re-dispatched. The pipeline caught a bad change and retried it: correct.
- **Observations accumulate** through the real lifecycle (9), deduped correctly.
- **Every fix from the July-3 session fired live**: OAuth-token routing injected
  `CLAUDE_CODE_OAUTH_TOKEN` (rosary-1be3b8), the required/optional MCP-tool gate
  warned on the absent `lectio` and proceeded (rosary-ea33b5), and the R4b
  shadow-fold recorded + folded each verdict (rosary-a66b3a steps 1–3).

## What it revealed — the multi-phase fold gap (R4b step-4 blocker)

After the run, `rsry lattice audit` grew from N=1 to N=2 and flagged a divergence:

```
lattice audit [rosary]: beads=950 comparable=2 agree=1 diverge=1
  DIVERGE rosary-765f42 : persisted=open folded=Some(Pass) (expected=verifying)
```

The bead is `persisted=open` (mid-retry: the dev-agent failed verify and it was
re-queued), but the lattice folds to `Pass`. Why: the `PipelineVerdict` field is
**one value per bead, chain-max folded across ALL phases** — and phase-1
scoping's `Pass` dominates. So the fold reports "verify passed" while the bead is
actually re-dispatching a *later* phase.

Root cause: the observation model records a single `PipelineVerdict` per bead,
but the pipeline is **multi-phase** (scoping → dev → staging → …), each phase
with its own verdict lifecycle. Chain-max conflates them: an early phase's `Pass`
masks a later phase's `Fail`/retry.

This is **distinct from rosary-818ed4** (terminal `Fail`/`Deadletter`). That fix
made *terminal* states absorb. This is *non-terminal* — a bead mid-retry of a
later phase after an earlier phase passed.

## Implications for R4b step 4 (the source-of-truth flip)

`persist_status` is authoritative and **correct** here (`open` = re-queued). The
lattice fold is **not yet equivalent** and cannot be source of truth while it
conflates multi-phase state. Before the flip, the fold must become phase-aware —
either record `PipelineVerdict` per `(bead, phase)`, or derive status from the
**latest** phase's verdict rather than a global chain-max. Tracked separately.

## Also noted (lower priority)

- The dev-agent's change to `rosary-765f42` failed verify twice. Worth a look
  (compile / lint / review / diff-sanity?) — but the pipeline correctly caught
  and retried it, which is the healthy behavior, not a pipeline bug.
- `[linear] 401 Unauthorized` during dispatch (Linear API key not configured) —
  non-blocking, cosmetic.

## Bottom line

The pipeline works end-to-end — the biggest unknown, now proven on real work. The
lattice machinery works. And run #1 already earned its keep: it surfaced a real,
fundamental R4b modeling gap (multi-phase fold) that blocks the source-of-truth
flip. Precisely why you run before you flip.
