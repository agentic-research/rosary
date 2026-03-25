---
name: evolve
description: >
  Continuous improvement loop — spawns a team of rosary agents to audit,
  fix, and test the current repo. Reads open beads, prioritizes by effort,
  fans out small tasks to parallel agents with TDD (Playwright tests first).
  Reports what was done, what needs human input, and what's blocked.
  Designed for cron scheduling — runs autonomously, humans review summaries.
user-invocable: true
argument-hint: "[--dry-run] [--focus security|fe|code-quality|all]"
allowed-tools: "*"
version: "0.1.0"
author: "ART Ecosystem"
---

# /evolve — Continuous Improvement Loop

Autonomous improvement cycle for any ART repo. Reads beads, picks up work,
ships fixes, closes beads. Humans review the summary, not 15 permission prompts.

## What To Do

### 1. Assess — What needs work?

```
rsry bead list  →  open beads for this repo
mache get_overview  →  structural health
git log --oneline -20  →  recent momentum
```

Classify each open bead:
- **Closable now** — small task, files known, tests writable (<30 min)
- **Needs design** — architecture decision, multiple approaches
- **Blocked** — waiting on external (CF beta, pricing decision, other repo)
- **Stale** — already done or superseded

### 2. Plan — TDD-first execution

For each closable bead:
1. **staging-agent** writes Playwright test that FAILS (the spec)
2. **dev-agent** implements the fix
3. **feature-agent** checks cross-file coherence
4. **prod-agent** reviews for security/perf issues
5. Tests pass → commit → close bead

Fan out independent beads to parallel agents. Sequential only when files overlap.

### 3. Execute — Team dispatch

Spawn a team with these roles:

| Agent | Role | Mode |
|-------|------|------|
| staging-agent | Write failing tests first | auto |
| dev-agent | Implement fixes | auto |
| feature-agent | Validate coherence | read-only |
| prod-agent | Security/perf review | read-only |
| janitor-agent | Structural cleanup via mache | auto |

Each agent:
- Claims a bead via task ownership
- Works in isolation (fresh context)
- Marks task complete when done
- Team lead commits + pushes when typecheck passes

### 4. Report — What happened

Output a structured summary:

```
## /evolve run — {date}

### Closed
- rig-abc123: Fixed dashboard empty state (3 files, 2 tests added)
- rig-def456: Extracted rate limit middleware (1 file, test updated)

### Needs human
- rig-967ae1: Product tier gating — needs pricing decision
- rig-556a26: ADR — needs architecture vision

### Blocked
- rig-bf1493: Dynamic Workers — waiting on CF LOADER beta

### Stale (auto-closed)
- rig-e1c9a4: Transitions already correct per audit

### Health
- Typecheck: ✓
- Playwright: 48/48 passing
- Open beads: 43 → 38
```

### 5. Deploy — If all green

If typecheck passes AND all Playwright tests pass:
- `git push origin main`
- `task web:deploy` (CF Worker)
- Update Notion with session summary

If anything fails: stop, report the failure, don't push.

## Arguments

- `$1` — focus area: `security`, `fe`, `code-quality`, `all` (default: `all`)
- `--dry-run` — assess + plan only, don't execute
- Reads `$ARGUMENTS` for the full argument string

## Scheduling

Designed for `cron` or CC scheduled tasks:
```
/evolve --focus security    # nightly security pass
/evolve --focus fe          # weekly FE consistency
/evolve                     # full sweep
```

## Lifecycle conditions (rig-e7562f)

**START when:** bead is open + files are specified + bead has clear description
**STOP when:** tests pass + typecheck passes + no prod-agent objections
**ESCALATE when:** prod-agent finds critical issue, or >3 files changed without tests
**NEVER:** push to main if tests fail, merge without typecheck, close bead without verification

## Provenance (rig-e75618)

Every commit message references the bead ID: `[rig-abc123] fix: ...`
Every PR body includes: bead link, test results, files changed, agent that did the work.
Future: BDR-configurable provenance form per repo.
