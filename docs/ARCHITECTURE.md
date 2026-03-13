# Rosary Architecture

Rosary is a cross-repo task orchestrator that strings beads (per-repo work items), Linear tickets, and review layers into coordinated autonomous development work.

## System Overview

```mermaid
graph TB
    subgraph "rosary"
        CLI[rsry CLI]
        RC[Reconciler]
        SC[Scanner]
        TR[Triage / Queue]
        DI[Dispatcher]
        VE[Verifier]
    end

    subgraph "Storage"
        D1[Dolt Server<br/>repo-A/.beads/]
        D2[Dolt Server<br/>repo-B/.beads/]
        DN[Dolt Server<br/>repo-N/.beads/]
    end

    subgraph "External"
        LN[Linear API]
        CC[Claude Code CLI]
        GH[GitHub]
    end

    CLI --> RC
    RC --> SC --> D1 & D2 & DN
    RC --> TR
    TR --> DI --> CC
    DI --> VE
    RC -.-> LN
    CC --> GH
```

## Reconciliation Loop

The core of rosary is a Kubernetes-controller-style desired-state loop. Every iteration:

```mermaid
flowchart LR
    SCAN["1. SCAN<br/>Discover beads<br/>across repos"]
    TRIAGE["2. TRIAGE<br/>Score & enqueue<br/>eligible beads"]
    DISPATCH["3. DISPATCH<br/>Spawn Claude<br/>Code agents"]
    VERIFY["4. VERIFY<br/>Tiered checks<br/>on results"]
    REPORT["5. REPORT<br/>Update status,<br/>log events"]
    SLEEP["6. SLEEP<br/>Wait interval"]

    SCAN --> TRIAGE --> DISPATCH --> VERIFY --> REPORT --> SLEEP --> SCAN
```

## Bead State Machine

Each bead follows a Labeled Transition System with 8 states:

```mermaid
stateDiagram-v2
    [*] --> open

    open --> queued : triage selects<br/>(score >= threshold)
    queued --> dispatched : semaphore acquired
    dispatched --> verifying : agent exits
    verifying --> done : all tiers pass
    verifying --> rejected : tier fails
    verifying --> blocked : needs human

    rejected --> open : retry after backoff
    blocked --> open : dependency resolved

    done --> [*]

    stale --> open : content changed

    note right of rejected
        Exponential backoff:
        30s * 2^retries
        (cap 30min, max 5)
    end note
```

## Module Layout

```mermaid
graph LR
    subgraph "src/"
        main["main.rs<br/>(CLI, clap)"]
        reconcile["reconcile.rs<br/>(loop orchestrator)"]
        scanner["scanner.rs<br/>(multi-repo scan)"]
        queue["queue.rs<br/>(priority queue)"]
        dispatch["dispatch.rs<br/>(agent spawning)"]
        verify["verify.rs<br/>(tiered checks)"]
        dolt["dolt.rs<br/>(MySQL client)"]
        bead["bead.rs<br/>(data model)"]
        config["config.rs<br/>(TOML loader)"]
        linear["linear.rs<br/>(Linear GraphQL)"]
        serve["serve.rs<br/>(MCP server)"]
    end

    main --> reconcile
    main --> scanner
    main --> dispatch
    main --> serve

    reconcile --> scanner
    reconcile --> queue
    reconcile --> dispatch
    reconcile --> verify

    scanner --> dolt
    dispatch --> dolt
    dolt --> bead
    scanner --> bead
    queue --> bead

    main --> config
    reconcile --> config
```

## Triage Scoring

Beads are scored with a weighted composite to determine dispatch priority:

```
score = 0.4 * priority_score    # P0=1.0, P4=0.2
      + 0.3 * dependency_score  # 1.0 if ready, 0.0 if blocked
      + 0.2 * age_score         # linear ramp over 1 week
      + 0.1 * retry_penalty     # 1/(1+retries)
```

Higher score = dispatched first. Beads in backoff are skipped until `not_before` expires.

## Verification Pipeline

Five tiers run in sequence; first failure short-circuits:

```mermaid
flowchart LR
    T0["Tier 0<br/>Commit<br/>exists?"]
    T1["Tier 1<br/>Compile<br/>cargo check"]
    T2["Tier 2<br/>Test<br/>cargo test"]
    T3["Tier 3<br/>Lint<br/>clippy"]
    T4["Tier 4<br/>Diff Sanity<br/>≤10 files, ≤500 lines"]

    T0 -->|pass| T1 -->|pass| T2 -->|pass| T3 -->|pass| T4 -->|pass| DONE["done"]
    T0 -->|fail| REJECT["rejected<br/>(fundamental)"]
    T1 -->|fail| REJECT
    T2 -->|fail| RETRY["rejected<br/>(retry eligible)"]
    T3 -->|fail| RETRY
    T4 -->|fail| PARTIAL["blocked<br/>(needs human)"]
```

Language-aware: Rust gets `cargo check/test/clippy`, Go gets `go vet/test/golangci-lint`.

## Stopping Conditions

| Condition | Default | Scope |
|-----------|---------|-------|
| Max retries per bead | 5 | Per-bead, then deadletter |
| Consecutive reverts | 3 | Per-bead, then deadletter |
| Agent timeout | 10 min | Per-dispatch, kill process |

A "revert" is when `highest_passing_tier` drops below its previous value after a dispatch. Three consecutive reverts means the agent is making things worse.

## Data Flow

```mermaid
sequenceDiagram
    participant R as Reconciler
    participant D as Dolt Server
    participant Q as WorkQueue
    participant C as Claude Code
    participant V as Verifier

    R->>D: scan_repos() → SELECT FROM issues
    D-->>R: Vec<Bead>
    R->>Q: triage_score() → enqueue()
    R->>Q: dequeue()
    Q-->>R: QueueEntry (highest score)
    R->>D: update_status("dispatched")
    R->>C: spawn("claude --print {prompt}")
    Note over C: Agent works in<br/>git worktree
    C-->>R: exit status
    R->>V: run(work_dir)
    V-->>R: VerifySummary
    alt all pass
        R->>D: update_status("done")
    else tier fails
        R->>Q: record_backoff()
        R->>D: update_status("open")
    end
```

## Dolt Connection Model

Each repo has a `.beads/` directory with a running Dolt server:

```
repo/.beads/
├── dolt-server.port     # TCP port (e.g., 53214)
├── metadata.json        # {"dolt_database": "rosary", ...}
├── dolt/                # Dolt data directory
│   └── (versioned SQL database)
├── config.yaml          # bd configuration
└── interactions.jsonl   # agent interaction log
```

Rosary connects via native MySQL wire protocol: `mysql://root@127.0.0.1:{port}/{database}`

Key tables: `issues` (51 columns), `dependencies`, `comments`, `events`

## Configuration

`rosary.toml` declares repos to manage:

```toml
[[repo]]
name = "rosary"
path = "~/remotes/art/rosary"
lang = "rust"
self = true  # dogfooding flag

[[repo]]
name = "mache"
path = "~/remotes/art/mache"
lang = "go"
```

## CLI Commands

| Command | Status | Description |
|---------|--------|-------------|
| `rsry scan` | Working | Discover beads across repos via Dolt |
| `rsry status` | Working | Aggregate view across repos |
| `rsry dispatch <id>` | Working | Spawn Claude Code for a single bead |
| `rsry run` | Working | Full reconciliation loop |
| `rsry run --once --dry-run` | Working | Single pass, print without spawning |
| `rsry plan <ticket>` | Working | Fetch Linear ticket details |
| `rsry sync` | Working | List open Linear issues (read-only) |
| `rsry serve` | Working | MCP server (stdio transport) |

## Design Influences

- **Kubernetes controllers**: Desired state vs actual state, reconciliation loop, generation tracking
- **driftlessaf** (Chainguard): Workqueue with priority, NotBefore scheduling, exponential backoff, provider overlay pattern
- **gem** (sibling repo): Tiered deterministic evaluation, consecutive-revert stopping, mode-aware dispatch
- **State machine design**: 8-state bead lifecycle with generation tracking and bounded retries

## Future Architecture

```mermaid
graph TB
    subgraph "Planned"
        SDK["Agent SDK<br/>(replace CLI)"]
        LIN["Linear Sync<br/>(bidirectional)"]
        LEY["ley-line Integration<br/>(tree-sitter, embeddings)"]
        JJD["jj Dispatch<br/>(workspaces)"]
        EVT["Event Bus<br/>(UDS, ADR-010)"]
        BEAD["Bead Management<br/>(rsry bead create/close)"]
    end

    subgraph "Implemented"
        REC["Reconciler"]
        SCA["Scanner"]
        DIS["Dispatcher"]
        VER["Verifier"]
        QUE["Queue"]
        MCP["MCP Server"]
        LINR["Linear Client"]
    end

    REC --> SDK
    REC --> LIN
    VER --> LEY
    DIS --> JJD
    REC --> EVT
    REC --> BEAD
```
