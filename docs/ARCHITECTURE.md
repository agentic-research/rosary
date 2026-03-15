# Rosary Architecture

Rosary is a cross-repo task orchestrator that strings beads (per-repo work items), Linear tickets, and review layers into coordinated autonomous development work.

## System Overview

```mermaid
graph TB
    subgraph "rosary"
        CLI[rsry CLI]
        MCP[MCP Server<br/>stdio + HTTP]
        RC[Reconciler]
        SC[Scanner]
        TR[Triage / Queue]
        DI[Dispatcher]
        VE[Verifier]
        WS[Workspace<br/>jj / git / in-place]
        CP[ComputeProvider<br/>local / sprites]
    end

    subgraph "Storage"
        D1[Dolt Server<br/>repo-A/.beads/]
        D2[Dolt Server<br/>repo-B/.beads/]
        DN[Dolt Server<br/>repo-N/.beads/]
        SR[Session Registry<br/>~/.rsry/sessions.json]
    end

    subgraph "External"
        LN[Linear API + Webhooks]
        CC[Claude / Gemini CLI]
        SP[sprites.dev API]
        GH[GitHub]
    end

    CLI --> RC
    MCP --> RC
    RC --> SC --> D1 & D2 & DN
    RC --> TR
    TR --> DI
    DI --> WS --> CP
    CP --> CC
    CP --> SP
    DI --> VE
    DI --> SR
    RC -.-> LN
    CC --> GH
```

## Reconciliation Loop

The core is a Kubernetes-controller-style desired-state loop:

```mermaid
flowchart LR
    SCAN["1. SCAN<br/>Discover beads<br/>across repos"]
    TRIAGE["2. TRIAGE<br/>Score & enqueue<br/>eligible beads"]
    DISPATCH["3. DISPATCH<br/>Spawn agent in<br/>workspace"]
    VERIFY["4. VERIFY<br/>Tiered checks<br/>on results"]
    REPORT["5. REPORT<br/>Update status,<br/>sync Linear"]
    SLEEP["6. SLEEP<br/>Wait interval"]

    SCAN --> TRIAGE --> DISPATCH --> VERIFY --> REPORT --> SLEEP --> SCAN
```

## Bead State Machine

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
    blocked --> open : human /resume

    done --> [*]

    note right of rejected
        Exponential backoff:
        30s * 2^retries
        (cap 30min, max 5)
    end note
```

## Module Layout

```mermaid
graph LR
    subgraph "Core Loop"
        reconcile["reconcile.rs<br/>loop orchestrator"]
        scanner["scanner.rs<br/>multi-repo scan"]
        queue["queue.rs<br/>priority queue"]
        dispatch["dispatch.rs<br/>AgentProvider + AgentSession"]
        verify["verify.rs<br/>tiered checks"]
    end

    subgraph "Execution"
        backend["backend.rs<br/>ComputeProvider trait"]
        workspace["workspace.rs<br/>jj/git isolation"]
        session["session.rs<br/>session registry"]
        sprites["sprites.rs<br/>sprites.dev client"]
        sprites_prov["sprites_provider.rs<br/>SpritesProvider"]
    end

    subgraph "Data"
        bead["bead.rs<br/>data model"]
        dolt["dolt.rs<br/>MySQL client"]
        pool["pool.rs<br/>RepoPool"]
        epic["epic.rs<br/>bead clustering"]
    end

    subgraph "Integration"
        linear["linear.rs<br/>Linear sync CLI"]
        linear_tracker["linear_tracker.rs<br/>IssueTracker impl"]
        sync["sync.rs<br/>bidi sync engine"]
        thread["thread.rs<br/>cross-repo xrefs"]
    end

    subgraph "Interface"
        main["main.rs<br/>CLI (clap)"]
        serve["serve.rs<br/>MCP + webhooks"]
        config["config.rs<br/>TOML config"]
        acp["acp.rs<br/>Agent Client Protocol"]
        vcs["vcs.rs<br/>jj state versioning"]
    end

    reconcile --> scanner --> dolt
    reconcile --> queue --> bead
    reconcile --> dispatch --> workspace --> backend
    dispatch --> session
    backend --> sprites_prov --> sprites
    reconcile --> verify
    reconcile --> sync --> linear_tracker
    serve --> pool --> dolt
    main --> reconcile
    main --> serve
```

## Dispatch Architecture

Two orthogonal axes compose for agent execution:

```mermaid
graph TB
    subgraph "AgentProvider (WHICH model)"
        claude[ClaudeProvider]
        gemini[GeminiProvider]
        acp_cli[AcpCliProvider]
    end

    subgraph "ComputeProvider (WHERE it runs)"
        local[LocalProvider<br/>host subprocess]
        sprites_be[SpritesProvider<br/>sprites.dev containers]
    end

    subgraph "AgentSession (HOW to talk)"
        cli_session[CliSession<br/>wraps Child]
    end

    AgentProvider -->|"spawn_agent()"| AgentSession
    Workspace -->|"provision()"| ComputeProvider
```

`AgentProvider` decides the model and returns a `Box<dyn AgentSession>`.
`ComputeProvider` decides the infrastructure (local process vs remote container).
`Workspace` manages VCS isolation (jj > git worktree > in-place).

## Triage Scoring

```
score = 0.4 * priority_score    # P0=1.0, P4=0.2
      + 0.3 * dependency_score  # 1.0 if ready, 0.0 if blocked
      + 0.2 * age_score         # linear ramp over 1 week
      + 0.1 * retry_penalty     # 1/(1+retries)
```

Higher score = dispatched first. Beads in backoff are skipped until `not_before` expires.

## Verification Pipeline

Five tiers, first failure short-circuits:

```mermaid
flowchart LR
    T0["Tier 0<br/>Commit exists?"]
    T1["Tier 1<br/>Compile"]
    T2["Tier 2<br/>Test"]
    T3["Tier 3<br/>Lint"]
    T4["Tier 4<br/>Diff Sanity<br/>≤10 files, ≤500 lines"]

    T0 -->|pass| T1 -->|pass| T2 -->|pass| T3 -->|pass| T4 -->|pass| DONE["done"]
    T0 -->|fail| REJECT["rejected"]
    T1 -->|fail| REJECT
    T2 -->|fail| RETRY["retry"]
    T3 -->|fail| RETRY
    T4 -->|fail| BLOCK["blocked"]
```

Language-aware: Rust gets `cargo check/test/clippy`, Go gets `go vet/test/golangci-lint`.

## Stopping Conditions

| Condition | Default | Scope |
|-----------|---------|-------|
| Max retries per bead | 5 | Per-bead, then deadletter |
| Consecutive reverts | 3 | Per-bead, then deadletter |
| Agent timeout | 10 min | Per-dispatch, kill process |

## Data Flow

```mermaid
sequenceDiagram
    participant R as Reconciler
    participant D as Dolt Server
    participant W as Workspace
    participant A as Agent (Claude/Gemini)
    participant V as Verifier
    participant S as Session Registry

    R->>D: scan_repos()
    D-->>R: Vec<Bead>
    R->>R: triage_score() → enqueue()
    R->>W: Workspace::create(bead_id)
    W-->>R: isolated work_dir
    R->>D: update_status("dispatched")
    R->>S: register(bead_id, pid)
    R->>A: spawn_agent(prompt, work_dir)
    Note over A: Agent works in<br/>isolated workspace
    A-->>R: session completes
    R->>V: verify(work_dir)
    V-->>R: VerifySummary
    alt all pass
        R->>D: update_status("done")
        R->>W: teardown()
    else tier fails
        R->>R: record_backoff()
        R->>D: update_status("open")
    end
    R->>S: unregister(bead_id)
```

## Dolt Connection Model

Each repo has a `.beads/` directory with a running Dolt server:

```
repo/.beads/
├── dolt-server.port     # TCP port
├── metadata.json        # {"dolt_database": "rosary", ...}
├── dolt/                # Dolt data directory
│   └── (versioned SQL database)
├── config.yaml          # bd configuration
└── interactions.jsonl   # agent interaction log
```

Connected via MySQL wire protocol: `mysql://root@127.0.0.1:{port}/{database}`

## Linear Integration

Bidirectional sync with Linear as the human-facing UI:

- **Push**: `persist_status()` mirrors every bead state transition to Linear
- **Pull**: `/webhook` endpoint receives Linear webhooks (HMAC-SHA256 verified)
- **State mapping**: type-based (`started`/`unstarted`/`completed`), not name-based
- **Labels**: agent perspectives (`perspective:dev`, etc.) flow through as Linear labels
- **Phases**: `[linear.phases]` maps beads to Linear projects

## Cross-Repo Bead Tracking

Beads reference work in other repos via `external_ref`. The `thread.rs` module:

1. **Parse** — find all beads with `external_ref` set
2. **Mirror** — create corresponding bead in target repo with back-reference
3. **Sync** — propagate status changes (source wins on drift)

## Selective Field Encryption (rosary-crypto)

`crates/crypto/` provides ChaCha20-Poly1305 AEAD for Wasteland federation:

- **Public** (cleartext): id, title, status, priority, issue_type
- **Private** (encrypted): description, owner, branch, pr_url
- **Nonce**: SHA-256(bead_id || field_name)[0..12] — deterministic per field

## Design Influences

- **Kubernetes controllers**: desired state reconciliation, generation tracking
- **driftlessaf** (Chainguard): workqueue with priority, NotBefore scheduling, exponential backoff
- **beads** (steveyegge): AI-native issue tracking, Dolt-backed, VCS-agnostic
