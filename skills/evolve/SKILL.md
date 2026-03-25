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

## Scale — Intel before operation

Evolve operates at three scales. The assessment phase determines which:

| Scale | Scope | When | Intel source |
|-------|-------|------|-------------|
| **File** | Single file refactor | `--simplify` on small beads | LSP, mache get_impact |
| **Repo** | Cross-file coherence | Default mode, most beads | mache get_communities, dependency graph |
| **Ecosystem** | Cross-repo dependencies | Beads touching shared types/APIs | rosary.toml repo list, `../` traversal |

**Ecosystem scale** applies when:
- A bead references files in multiple repos (e.g., signet cert format + rig cert minting)
- A change in one repo breaks assumptions in another
- Imports or configs reference external repos

Scale detection is DERIVED, not configured:
1. Check if `rosary.toml` exists -- if yes, read the repo map
2. If not, check `../` for sibling repos (common monorepo/workspace pattern)
3. Check imports/configs for cross-repo references (go.mod, Cargo.toml, package.json)
4. If no cross-repo deps found, operate at repo scale

At ecosystem scale, the scoping-agent maps dependencies (rosary.toml OR `../` scan),
then checks if the bead's files touch any cross-repo seams (use `rosary:seam-discovery`).
If yes, plan.md must document which repos are affected and in what order.

The skill works with zero config. rosary.toml is a hint, not a requirement.

## Personas — Who works on what

Agents are personas (perspective + judgment) with abilities (skills + tools):

| Persona | Perspective | Skills | Constraints |
|---------|------------|--------|-------------|
| **scoping-agent** | Planner — enriches before expensive work | note | Haiku model, cheap |
| **dev-agent** | Implementer — finds complexity, fixes it | simplify | Full access |
| **principal-agent** | Does what is right, not what was asked | evolve, simplify, seam-discovery, note | Full access, Opus |
| **skeptic-agent** | Distrusts AI output, assumes wrong until proven | note | Read-only, no Write/Edit |
| **staging-agent** | Adversarial tester — tests test real behavior? | | Read-only |
| **prod-agent** | Finds resource leaks, error swallowing, concurrency | note | Read-only |
| **pm-agent** | Strategic — cross-repo overlap, scope creep, retro | note, evolve | Full access |

Mode determines which personas deploy:

| Mode | Generator | Evaluator |
|------|-----------|-----------|
| default | dev-agent | staging-agent |
| `--simplify` | principal-agent | skeptic-agent |
| `--security` | prod-agent | skeptic-agent |
| `--fe` | dev-agent | staging-agent |
| `--prune` | janitor-agent | skeptic-agent |

### Team Scaling — Match team size to task size

The scoping-agent determines team composition. Mode selects which AGENTS,
scale selects HOW MANY:

| Bead scope | Team | Rationale |
|------------|------|-----------|
| 1 file | Generator solo | No coordination overhead |
| 2-5 files, same module | Generator + evaluator | Minimal viable pipeline |
| >5 files OR cross-module | Scoping + generator + evaluator + skeptic | Full pipeline |
| `--simplify` (any size) | principal-agent + skeptic | Simplify always needs adversarial check |
| `--security` (any size) | prod-agent + skeptic | Security always needs adversarial check |

**How to determine scale:**
1. Count files in bead scope
2. Check if files span multiple directories (mache get_communities if available)
3. Mode flags (`--simplify`, `--security`) override file-count logic
4. Scoping-agent writes team composition into plan.md's `## Team` section

For 1-file scale: the generator agent does its own verification (no separate evaluator).
This avoids spawning a second agent just to re-run one lint command.

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

### 2. Derive the lifecycle -- Don't configure, discover

For each closable bead, the scoping-agent DERIVES the lifecycle from the code.
Do not ask the human to define these -- read the codebase and figure it out.
If something is NOT derivable, ASK the human before proceeding.

**Starting point** (derive from bead + code):
- Problem statement: what does the bead describe? Read the bead description.
- Current state: read the files the bead touches. What exists today?
- Outcome: what should be different after? Derive from bead title + description.
- If the bead is vague, ASK: "What does done look like for {bead-id}?"

**Stopping point** (derive from existing tests + types):
- Definition of done: does a test exist that would fail today and pass after?
  If yes, that's the acceptance criterion. If no, write one.
- Acceptance criteria: typecheck passes, existing tests don't regress, new test passes.
- If there are no tests for this area, ASK: "No tests cover {file}. Should I add one?"

**Validation** (derive from the dependency graph):
- What calls this code? (mache find_callers or grep)
- What does this code call? (mache find_callees)
- If callers > 3, the change is high-risk -- evaluator must verify each caller.
- If the file has no callers, it might be dead code -- flag for `--prune`.

**Verification and attestation** (cite sources):
- Every claim in plan.md must have a citation:
  - File path + line number for code references
  - Bead ID for the work item
  - Test name for acceptance criteria
  - mache output for dependency claims
- If you can't cite it, you don't know it. ASK or investigate.

The scoping-agent writes all of this into `plan.md`:

```
## Plan: {bead-id} -- {title}

### Starting point
- Problem: {derived from bead description}
- Files: {list with line numbers}
- Current behavior: {what the code does now, with citations}

### Stopping point
- Done when: {test name} passes
- Acceptance: typecheck + {N} existing tests + 1 new test
- LOC delta: {estimate, negative for simplify}

### Validation
- Callers: {list from mache/grep, with file:line}
- Risk: {low|medium|high} because {reason}

### Verification
- Citations: {every claim has a file:line or bead-id}
- Questions for human: {anything not derivable}
```

If the `Questions for human` section is non-empty, STOP and ask before executing.
An empty questions section means the plan is self-contained and can proceed autonomously.

### 2.5 Discover Verification — Don't hardcode, probe

The scoping-agent discovers verification commands for the repo. This replaces
any hardcoded assumptions about what tools a repo uses.

**Principle: One path, multiple callers.**
Evolve uses the same commands a developer runs locally, CI runs remotely,
and pre-commit runs on commit. It never invents its own verification commands.

**Probe hierarchy** (scoping-agent checks in order, uses first match per category):

| Priority | File | Extract |
|----------|------|---------|
| 1 | Taskfile.yml | Parse task names — lint, test, check, typecheck, build |
| 2 | Makefile | Parse targets — lint, test, check |
| 3 | package.json | Parse scripts — test, lint, typecheck, build |
| 4 | Cargo.toml | `cargo test`, `cargo clippy`, `cargo build` |
| 5 | go.mod | `go test ./...`, `go vet ./...` |
| 6 | mix.exs | `mix test`, `mix format --check-formatted` |
| 7 | pyproject.toml | `pytest`, `ruff check .` |

**Fast-fail layer:** Before running any of the above, the evaluator runs
`mache get_diagnostics` on changed files. Type/syntax errors caught in
milliseconds — no point running a 30-second test suite if the code doesn't parse.

**Commit gate:** If `.pre-commit-config.yaml` exists, run `pre-commit run --all-files`
as a final gate. If not, suggest bootstrapping from the reference template
in `docs/pre-commit-reference.yaml`.

**Hard stop:** If NO verification tooling is found, STOP and ask the human:
"No verification tooling found in {repo}. What should I run?"
Do not guess. Do not proceed without verification.

The scoping-agent writes all discovered commands into plan.md's `## Verification`
section. Downstream agents read this section — they never probe on their own.

### 3. Execute -- Generator -> Evaluator pipeline

**Generator** (dev-agent, per bead):
- Reads plan.md (including `## Verification` section)
- Implements the fix
- Runs ALL verify commands from plan.md's Verification section
- Captures stdout/stderr of each command
- Stages changes (`git add`)
- Writes `changes.md` with:
  - Summary of what was done
  - **Verification Output** section with actual command output and exit codes
  - If any command failed, reports the failure — does NOT claim success
- Does NOT commit

**Evaluator** (staging-agent, after ALL generators finish):
- Runs `mache get_diagnostics` on changed files (fast-fail — ms)
- Re-runs ALL verify commands from plan.md independently (does NOT trust generator's output)
- Compares its results against generator's claimed results in changes.md
- If results differ, that itself is a finding (stale state, flaky test, or lie)
- Reads each `changes.md`
- Writes `eval.md` per bead: PASS or FAIL with:
  - Its own command output as evidence
  - Specific error messages if FAIL (not just "tests failed")
  - Diff against generator's claimed output if they diverge

**On FAIL**: writes `feedback.md` with the exact error output (command, exit code,
stderr). Generator retries with this specific error context (max 3). If all retries
fail, bead stays open with error details as a comment via rsry_bead_comment.

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

**START:** bead open + files specified + description clear + verification discovered
**STOP:** evaluator passes + ALL discovered verify commands green (with evidence)
**ESCALATE:** critical issue found, or >3 files without tests, or no verification tooling found
**RETRY:** evaluator fails with specific error output (max 3 per bead)
**NEVER:** commit without eval pass, push with failing tests, proceed without verification

## Provenance

Commits: `[bead-id] type(scope): description`
PRs: bead link + test results + files + agent name
Post-mortem: what agents learned + principles updated
Future: BDR-configurable output templates per repo (rig-d2b660)
