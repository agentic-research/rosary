# Bead Lifecycle: Dispatch → Pipeline → Review

The full loop from bead creation to human review.

```mermaid
sequenceDiagram
    participant O as Orchestrator
    participant D as dev-agent
    participant S as staging-agent
    participant P as prod-agent
    participant F as feature-agent
    participant PM as pm-agent
    participant V as Verification
    participant H as Human (Linear)

    O->>O: tick → fetch open beads
    O->>D: dispatch (ACP)

    Note over D: Phase 1: Implementation Quality
    D->>D: fix code, run tests, commit
    D-->>O: exit 0
    O->>V: verify (compile, test, lint, diff)
    V-->>O: pass
    O->>O: Pipeline.advance → staging-agent

    O->>S: dispatch (ACP)
    Note over S: Phase 2: Test Validity
    S->>S: adversarial test review
    S-->>O: exit 0
    O->>V: verify
    V-->>O: pass
    O->>O: Pipeline.advance → prod-agent

    O->>P: dispatch (ACP)
    Note over P: Phase 3: Production Quality
    P->>P: resource leaks, error handling, perf
    P-->>O: exit 0
    O->>V: verify
    V-->>O: pass
    O->>O: Pipeline.advance → done

    Note over O: Orchestrator Final Check
    O->>O: collect agent findings from bead comments
    O->>O: generate summary report
    O->>H: notify via Linear (status → "In Review")

    Note over H: Human reviews PR + agent findings
    H->>O: approve (Linear status → "Done")
```

## Pipeline Stages

| Stage | Agent | What it checks | Permission | Validation |
|-------|-------|----------------|------------|------------|
| 1 | dev-agent | Complexity, dead code, TODOs, hardcoded values | :implement | `task test` every 5min |
| 2 | staging-agent | Fake tests, mock abuse, coverage gaps | :read_only | `task test` every 5min |
| 3 | prod-agent | Resource leaks, error swallowing, concurrency | :read_only | `task test` every 5min |
| 4 | feature-agent | Circular deps, scattered functionality, API drift | :read_only | — |
| 5 | pm-agent | Cross-repo coherence, scope creep, abandoned work | :plan | — |

Not all beads go through all 5 stages. Pipeline templates by issue type:

| Type | Pipeline |
|------|----------|
| bug | dev → staging |
| feature | dev → staging → prod |
| task/chore | dev |
| review | staging |
| epic/design/research | pm |

## Orchestrator Final Check

After the pipeline completes, the orchestrator:

1. **Collects findings** — reads all bead comments from each agent phase
2. **Checks Golden Rules** — validates rule compliance (Rule 6: validate recursively)
3. **Generates report** — summary of what each agent found/changed
4. **Updates Linear** — moves issue to "In Review" with the report
5. **Waits for human** — the human reviews the PR + agent findings

The human sees:
- The code changes (PR diff)
- Each agent's findings (bead comments)
- The verification results (compile/test/lint per phase)
- The pipeline history (which agents passed, retries, timing)

## Rule 6: Self-Validation

The release gate for rosary itself is this same loop. Rosary dispatches agents to work on rosary beads, through the full pipeline, proving the methodology by using it. If the system can't orchestrate its own development, it's not ready to orchestrate anyone else's.

The `self_managed: true` flag in config marks rosary as its own subject. The conductor treats it like any other repo — but the meta-level validation is: **the tool works because it built itself**.
