# rosary

Rust-based agent orchestration and work tracking. Backbone of the ART (Agentic Research Toolkit) platform.

## What Rosary Does

1. **Scans** repositories for work items (beads) stored in `.beads/` (Dolt)
1. **Dispatches** agents to execute work — see `agents/` directory
1. **Reconciles** state via a k8s-controller-style loop: scan → triage → dispatch → verify
1. **Syncs** bidirectionally with Linear (beads are source of truth, Linear is UI)
1. **Serves** MCP over stdio and HTTP (Streamable HTTP transport)
1. **Receives** Linear webhooks for real-time state sync

## Development

```bash
task build          # debug build
task test           # run all tests
task lint           # fmt + clippy -D warnings
task install        # build release, codesign, install to ~/.local/bin
task all            # fmt + check + lint + test
```

## Architecture

### Transports

- `rsry serve --transport stdio` — MCP over stdin/stdout (default, used by Claude Code)
- `rsry serve --transport http --port 8383` — MCP Streamable HTTP + webhook receiver

### Linear Integration

- **Push**: `persist_status()` mirrors every bead state transition to Linear
- **Pull**: `/webhook` endpoint receives Linear webhooks, updates beads via HMAC-verified payloads
- **State mapping**: type-based (`started`/`unstarted`/`completed`), not name-based — works on any Linear team config
- **Configurable**: `[linear.states]` overrides, `[linear.phases]` maps to Linear projects
- **Labels**: agent perspectives (`perspective:dev`, etc.) flow through as Linear labels

### Config

- `~/.rsry/config.toml` — global config (repos, linear, backend, compute)
- `rosary.toml` — local/project config
- `rosary-self.toml` — self-management (dogfooding)
- See `docs/CONFIGURATION.md` for full reference of all config sections

## Key Source Files

| File                        | Purpose                                                                     |
| --------------------------- | --------------------------------------------------------------------------- |
| src/serve.rs                | MCP server (stdio + HTTP) + Linear webhook handler                          |
| src/reconcile.rs            | Reconciliation loop: scan → triage → dispatch → verify                      |
| src/bead.rs                 | Bead model, BeadState enum, Linear type mapping                             |
| src/dispatch.rs             | Agent dispatch, pipeline mapping, execution                                 |
| src/epic.rs                 | Semantic clustering, dedup, file overlap detection                          |
| src/dolt.rs                 | Dolt database client (per-repo beads)                                       |
| src/store_dolt.rs           | Dolt backend for orchestrator state (pipeline, dispatches, cross-repo deps) |
| src/store.rs                | Backend-agnostic store traits (HierarchyStore, DispatchStore, LinkageStore) |
| src/handoff.rs              | Structured context transfer between pipeline phases                         |
| src/workspace.rs            | Git/jj worktree creation and isolation                                      |
| src/linear.rs               | Linear sync CLI (`rsry sync`)                                               |
| src/linear_tracker.rs       | IssueTracker trait impl for Linear (cached states, configurable)            |
| src/sync.rs                 | Backend-agnostic sync engine                                                |
| src/config.rs               | Configuration (repos, linear, http, tunnel, backend)                        |
| src/pool.rs                 | Connection pool for multi-repo Dolt access                                  |
| src/main.rs                 | CLI entry + shared helpers (`generate_bead_id`, `resolve_beads_dir`)        |
| crates/bdr/src/parse.rs     | ADR markdown parser — frontmatter + section → atom extraction               |
| crates/bdr/src/decompose.rs | Atom → BeadSpec mapper with cross-repo routing + success criteria           |
| crates/bdr/src/thread.rs    | Thread grouping + Decade assembly from atoms                                |
| crates/bdr/src/accrete.rs   | Bottom-up: bead completions → decade state transitions                      |

## Agent Definitions

```
agents/
├── dev-agent.md          # Implementation quality (function-level)
├── staging-agent.md      # Test validity (adversarial review)
├── prod-agent.md         # Production quality (module-level)
├── feature-agent.md      # Cross-file coherence
├── architect-agent.md    # System architecture, ADRs, BDR decomposition
├── pm-agent.md           # Strategic perspective (cross-repo)
├── janitor-agent.md      # Codebase hygiene (repo-wide scheduled sweeps)
└── rules/
    └── GOLDEN_RULES.md   # 11 rules all agents operate under
```

Agents map to Linear **labels** (not users — one seat, all perspectives via labels).
Pipeline mapping: issue_type → agent sequence (dispatch.rs `agent_pipeline()`).

## Beads (Issue Tracking)

Beads are the distributed work tracking system. Each repo has `.beads/` with a Dolt database.

```bash
# MCP tools (via rsry serve) — 24 tools
# Beads
rsry_bead_create / rsry_bead_update / rsry_bead_search / rsry_bead_comment / rsry_bead_close
rsry_bead_link / rsry_status / rsry_list_beads / rsry_scan / rsry_active
# Dispatch + pipeline
rsry_dispatch / rsry_run_once / rsry_decompose
rsry_pipeline_upsert / rsry_pipeline_query / rsry_dispatch_record / rsry_dispatch_history
# Workspaces
rsry_workspace_create / rsry_workspace_checkpoint / rsry_workspace_cleanup / rsry_workspace_merge
# Hierarchy (BDR lattice)
rsry_decade_list / rsry_thread_list / rsry_thread_assign

# CLI
rsry sync --dry-run    # bidirectional Linear sync
rsry scan              # scan all repos for beads
rsry status            # aggregated counts
rsry bead create/list/search/close/comment
```

## Triage & Dispatch

The reconciler's triage phase applies multiple filters before dispatch:

1. State check (must be Open)
1. Severity floor (configurable min priority)
1. Skip epics (planning beads)
1. Dependency check (blocked beads deferred)
1. Per-repo busy check (one agent per repo)
1. Semantic dedup (`epic::is_dominated_by` — multi-signal similarity)
1. **File overlap detection** (`epic::has_file_overlap` — prevents concurrent edits to same files)

File overlap is also re-checked in Phase 4 (dispatch loop) to catch beads queued in the same triage pass.

## ADRs

| ADR  | Status   | Topic                                                                   |
| ---- | -------- | ----------------------------------------------------------------------- |
| 0001 | Proposed | Sprint planning protocol (Explore → Synthesize → Derive → Decompose)    |
| 0002 | Accepted | ACP integration (Agent Client Protocol)                                 |
| 0004 | Accepted | Dual state machine (bead lifecycle + pipeline phases)                   |
| 0005 | Proposed | Reactive persistent store ("local firebase" for agent IPC)              |
| 0006 | Proposed | Declarative tool registry (unified MCP/CLI/pipeline from single source) |
| 0007 | Proposed | BDR enrichment pipeline (mache + haiku + sqlite-vec dedup)              |
| 0008 | Proposed | Agent hierarchy dispatch model (dev/feature/orchestrator tiers)         |

## BDR Hierarchy (Decade → Thread → Bead)

Beads are organized into threads (ordered related work) and decades (ADR-level groupings).
`rsry_decompose` parses ADR markdown into atoms, maps to BeadSpecs with frontmatter metadata
(depends_on, target_repo, success_criteria), and groups into the hierarchy.

Current decades:

| Decade           | Threads                                          | Focus                         |
| ---------------- | ------------------------------------------------ | ----------------------------- |
| `bdr-quality`    | core, enrichment, active-dedup                   | BDR decompose quality + dedup |
| `agent-dispatch` | scope-reign, compute, pipeline, dispatch-quality | Agent hierarchy + dispatch    |
| `infra-workflow` | linear, jj-git, build-release                    | Infrastructure + workflow     |
| `cross-repo`     | service-boundaries, deps-severity, leyline-otp   | Cross-repo architecture       |

## MCP Integration

Rosary exposes 24 MCP tools via `rsry serve`. Accessible from:

- Claude Code (stdio transport, configured in MCP settings)
- Claude web (HTTP transport via tunnel)
- Any MCP client

Mache (`mache` MCP) provides structural code intelligence for exploring any repo.
