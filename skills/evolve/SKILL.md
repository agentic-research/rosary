---
name: evolve
description: >
  Continuous improvement loop — assesses open beads, classifies by effort,
  dispatches generator→evaluator pipeline with file-based handoff. No per-agent
  commits — evaluator gates all changes. Post-mortem refines agent prompts.
  Modes: --simplify (refactor only, no net new code), --security, --fe, --prune.
  Composable with /loop for cron scheduling.
user-invocable: true
argument-hint: "[--dry-run] [--simplify|--security|--fe|--prune] [--focus <area>]"
allowed-tools: "*"
version: "0.2.0"
author: "ART Ecosystem"
---

# /evolve — Continuous Improvement Loop

Autonomous improvement for any ART repo. Reads beads, picks up work, ships
fixes, closes beads. Humans review the summary, not permission prompts.

Composable: `/loop 10m /rosary:evolve --simplify` for continuous refactoring.

## Modes

| Mode | Constraint | Goal |
|------|-----------|------|
| (default) | Fix closable beads | Ship work |
| `--simplify` | **No net new code** — refactor only | Extract abstractions, reduce LOC |
| `--security` | Security findings only | Audit + fix vulnerabilities |
| `--fe` | FE consistency only | Design language, UX |
| `--prune` | **Delete only** — dead code, stale beads | Reduce surface area |
| `--dry-run` | Assess + plan, no changes | Preview what would happen |

## Architecture (Anthropic Harness Pattern)

Separate generator from evaluator. Self-evaluation creates confirmation bias;
external evaluation enables skepticism. File-based handoff between stages.

```
bead (contract: "what done looks like")
  -> scoping-agent (reads bead + files, writes plan.md)
    -> dev-agent (implements, stages changes -- does NOT commit)
      -> evaluator (runs typecheck + tests, writes eval.md)
        -> PASS: team lead commits + closes bead
        -> FAIL: feedback.md -> dev-agent retries (max 3)
          -> pm-agent (post-mortem, updates operating principles)
```

**Critical rules:**
- Agents stage changes, they do NOT commit
- Evaluator is the gate -- no code merges without passing eval
- File-based handoff: plan.md -> changes -> eval.md -> feedback.md
- Bead = sprint contract (what done looks like, not how to do it)

## What To Do

### 1. Assess

```
rsry bead list       -> open beads for this repo
mache get_overview   -> structural health (if available)
git log --oneline -20 -> recent momentum
```

Classify each open bead:
- **Closable now** -- small, files known, tests writable (<30 min)
- **Needs design** -- architecture decision, multiple approaches
- **Blocked** -- waiting on external (CF beta, pricing decision, other repo)
- **Stale** -- already done or superseded (auto-close these)

For `--simplify` mode: ignore beads. Scan codebase for:
- Duplicated patterns (mache get_communities)
- High-complexity files (LOC > 300, many imports)
- Unused exports, dead code paths
- Inconsistent error handling or response shapes
File ephemeral beads for each extraction, execute, close in same run.

### 2. Plan -- Contract negotiation

For each closable bead, scoping-agent writes `plan.md`:
- What files change
- What the test should verify (the "done" criteria)
- Estimated LOC change (positive = growth, negative = simplification)
- Risk level (low/medium/high)

For `--simplify`: plan must show LOC stays flat or decreases.

### 3. Execute -- Generator -> Evaluator pipeline

**Generator** (dev-agent, per bead):
- Reads plan.md
- Implements the fix
- Stages changes (`git add`)
- Writes `changes.md` summarizing what was done
- Does NOT commit

**Evaluator** (staging-agent, after ALL generators finish):
- Runs typecheck
- Runs tests (diff-aware: only affected pages if possible)
- Reads each `changes.md`
- Writes `eval.md` per bead: PASS or FAIL with specific feedback
- Auto-fixes mechanical issues (unused imports, formatting)

**On FAIL**: writes `feedback.md`, generator retries (max 3).
**On PASS**: team lead commits all passing beads as one batch.

### 4. Commit -- Team lead only

Only after evaluator passes ALL changes:
- One commit per bead: `[bead-id] type(scope): description`
- `git push origin main`
- Deploy if applicable
- Close beads via rsry

### 5. Post-mortem -- pm-agent reviews the run

After every run, pm-agent writes `postmortem.md`:

```
## /evolve post-mortem -- {date}

### What worked
- dev-agent extracted middleware correctly on first try

### What failed
- api-cleanup left unused imports (evaluator caught on retry)

### Operating principles (updated)
1. Always run typecheck before reporting done
2. JSX components: verify rendered, not just imported
3. as-any casts: document WHY, link upstream issue

### Agent prompt improvements
- dev-agent: add "run typecheck before done"
- staging-agent: add "check unused imports specifically"

### Metrics
- Beads closed: 4 | Retries: 1 | Human interventions: 0
```

Operating principles feed back into agent prompts. Each run makes agents better.

### 6. Report

```
## /evolve run -- {date} ({mode})

### Closed
- rig-abc123: Fixed X (3 files, 2 tests)

### Needs human
- rig-967ae1: Needs pricing decision

### Blocked
- rig-bf1493: Waiting on CF beta

### Health
- Typecheck: pass | Tests: 52/52 | Deploy: v{id}
- Beads: 39 -> 35 | LOC delta: -47

### Post-mortem
- 1 retry, 0 human interventions, 2 principles updated
```

## Scheduling

```bash
/rosary:evolve                        # full sweep
/rosary:evolve --simplify             # refactor pass
/rosary:evolve --dry-run              # preview only
/loop 30m /rosary:evolve --simplify   # continuous refactoring
/loop 4h /rosary:evolve               # full sweep every 4 hours
```

## Lifecycle conditions

**START:** bead open + files specified + description clear
**STOP:** evaluator passes + typecheck green + tests green
**ESCALATE:** critical issue found, or >3 files without tests
**RETRY:** evaluator fails (max 3 per bead)
**NEVER:** commit without eval pass, push with failing tests

## Provenance

Commits: `[bead-id] type(scope): description`
PRs: bead link + test results + files + agent name
Post-mortem: what agents learned + principles updated
Future: BDR-configurable output templates per repo (rig-d2b660)
