# rosary

Rust-based agent orchestration and work tracking. Backbone of the ART (Agentic Research Toolkit) platform.

## What Rosary Does

1. **Scans** repositories for work items (beads) stored in `.beads/` (a SQLite `beads.db`, or a Dolt server when `.beads/dolt/` exists) — read in-process, no `bd` CLI
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
| src/serve/mod.rs            | MCP server (stdio + HTTP) + Linear/GitHub webhook handlers                  |
| src/serve/handlers.rs       | MCP tool implementations (31 tools)                                         |
| src/serve/github_webhook.rs | GitHub merge webhook → advance bead + unblock dependents                    |
| src/reconcile/mod.rs        | Reconciliation loop: scan → triage → dispatch → verify                      |
| src/bead.rs                 | Bead model, BeadState enum, Comment struct (audit-trail), Linear type mapping |
| src/bead_sqlite.rs          | `connect_bead_store` — the single entry point for ALL bead I/O; `SqliteBeadStore` reads `.beads/beads.db` directly (rusqlite) when there's no `.beads/dolt/` |
| src/bead_dolt.rs            | `DoltBeadStore` — `BeadStore` over the Dolt MySQL client (used when `.beads/dolt/` exists) |
| src/dispatch/mod.rs         | Agent dispatch, pipeline mapping, execution                                 |
| src/dispatch/providers.rs   | AgentProvider trait — claude / gemini / plugin-kind="dispatch" backends     |
| src/dispatch/sweep.rs       | Orphan-dispatch detection — self-heals stuck `Dispatched` beads (rosary-67c43d) |
| src/epic.rs                 | Semantic clustering, dedup, file overlap detection                          |
| src/dolt.rs                 | Dolt client (per-repo beads, server mode) — used only when `.beads/dolt/` exists |
| src/store_dolt.rs           | Dolt backend for orchestrator state (pipeline, dispatches, cross-repo deps) |
| src/store.rs                | Backend-agnostic store traits (HierarchyStore, DispatchStore, LinkageStore) |
| src/observation/mod.rs      | ADR-0010 substrate: Observation, FieldName, FieldAlgebra, Observer trait    |
| src/observation/algebra_*.rs| Per-field algebras: chain-max, LWW-register, OR-set, flat-lattice            |
| src/observation/log.rs      | In-memory G-set (used by tests + integration_tests)                          |
| src/observation/log_sqlite.rs| Persistent G-set + quarantine via orchestrator SQLite                       |
| src/observation/registry.rs | Field/algebra dispatch via OnceLock singleton                                |
| src/observation/fold.rs     | Per-field per-source fold + cross-source flat-lattice for Status             |
| src/observation/tree_fold.rs| BDR Decade ⊃ Thread ⊃ Bead catamorphism (rollup status)                     |
| src/observation/quarantine.rs| Cert-validity filter + queryable quarantine surface                          |
| src/handoff.rs              | Structured context transfer between pipeline phases                         |
| src/workspace.rs            | Git/jj worktree creation and isolation                                      |
| src/linear.rs               | Linear sync CLI (`rsry sync`)                                               |
| src/linear_tracker.rs       | IssueTracker trait impl for Linear (cached states, configurable)            |
| src/github_mirror.rs        | `rsry sync --github` — bead context → PR comment                            |
| src/sync.rs                 | Backend-agnostic sync engine                                                |
| src/config.rs               | Configuration (repos, linear, http, tunnel, backend, plugins)               |
| src/plugin.rs               | Plugin registry + PluginKind axis (Hook / Mcp / Dispatch / StateSink)       |
| src/pool.rs                 | Connection pool for multi-repo Dolt access                                  |
| src/main.rs                 | CLI entry + shared helpers (`generate_bead_id`, `resolve_beads_dir`)        |
| src/capture.rs              | `rsry capture --from-session/--from-code` — text → BeadSpecs via LLM        |
| src/notes.rs                | `rsry notes rotate` — age recipient rotation for scoped notes               |
| src/scan_assay.rs           | `rsry scan --assay` — stale-ref → P3 chore beads via assay.scan plugins     |
| src/bdr_enrich.rs           | LLM atom extraction (lenient JSON parse — tolerates fences + trailing prose)|
| src/decompose.rs            | `rsry decompose --stub-output` — TechnicalSpec/Constraint atoms → Rust stubs|
| src/secrets.rs              | Write-time secret pattern scrubber for bead/comment writes                  |
| crates/bdr/src/parse.rs     | ADR markdown parser — frontmatter + section → atom extraction               |
| crates/bdr/src/decompose.rs | Atom → BeadSpec mapper (incl. `doc_coverage_min` field, cross-repo routing) |
| crates/bdr/src/thread.rs    | Thread grouping + Decade assembly from atoms                                |
| crates/bdr/src/accrete.rs   | Bottom-up: bead completions → decade state transitions                      |
| crates/bdr/src/provenance.rs| ProvenanceRef variants: Doc / Session / Code / Meeting / SlackThread        |

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

Beads are the distributed work tracking system. Each repo has `.beads/` with either a Dolt database (`.beads/dolt/`, server mode) or a SQLite `beads.db` (the default for single-user/local repos). Rosary reads/writes both in-process via `connect_bead_store` (`src/bead_sqlite.rs`) — it never invokes the `bd` CLI. See [ADR-0014](docs/adr/0014-decouple-rosary-from-bd.md).

```bash
# MCP tools (via rsry serve) — 31 tools
# Beads
rsry_bead_create / rsry_bead_update / rsry_bead_search / rsry_bead_close
rsry_bead_comment / rsry_bead_comment_list / rsry_bead_comment_update / rsry_bead_comment_delete
rsry_bead_link / rsry_bead_import / rsry_status / rsry_list_beads / rsry_scan / rsry_active
# Dispatch + pipeline
rsry_dispatch / rsry_run_once / rsry_decompose
rsry_pipeline_upsert / rsry_pipeline_query / rsry_dispatch_record / rsry_dispatch_history
# Workspaces
rsry_workspace_create / rsry_workspace_checkpoint / rsry_workspace_cleanup / rsry_workspace_merge
# Hierarchy (BDR lattice)
rsry_decade_list / rsry_thread_list / rsry_thread_assign / rsry_thread_reparent
# Repo registry
rsry_repo_register / rsry_repo_list

# CLI
rsry sync --dry-run                          # bidirectional Linear sync
rsry sync --github                           # mirror bead context to PR comments
rsry scan                                    # scan all repos for beads
rsry scan --assay                            # run assay.scan plugins → P3 chore beads for stale refs
rsry status [--json]                         # aggregated counts; CLI text + JSON outputs agree (#192)
rsry bead create / list / search / close / reopen   # `close` requires a test command in description (or --force)
rsry bead move <id> <dest-repo>              # cross-repo relocation (no bd): provenance+comments fwd, source tombstoned (ADR-0014)
rsry bead backup <file> / restore <file>     # restorable store backup (SQLite VACUUM INTO); Dolt repos pointed at `dolt backup`. Distinct from export --jsonl (interop-only)
rsry bead comment add <id> <body>            # append a comment (rosary-a96b06)
rsry bead comment list <id> [--include-deleted]
rsry bead comment update <id> <comment_id> --body <text> [--reason <why>]
rsry bead comment delete <id> <comment_id> [--reason <why>] [--hard]   # --hard CLI-only; soft preserves audit trail
rsry close-merged                            # catch-up sweep: close beads whose PRs already merged
rsry thread-reparent <thread_id> <decade_id> [--name <new>]  # re-parent threads under a different decade
rsry capture --from-session <path>           # transcript → BeadSpecs via LLM (Session provenance)
rsry capture --from-code <repo> <path>       # source file → BeadSpecs via LLM (Code provenance)
rsry decompose <path> --stub-output <repo>   # also emit Rust stubs for design review
rsry notes rotate --scope <s> --add-recipient <r>  # re-encrypt scope with new age recipient list
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
| 0009 | Accepted | Cross-repo linkage — stratified acyclicity + modal evidence             |
| 0010 | Accepted | Observation lattice — G-set + per-field fold (substrate)                |
| 0011 | Accepted | Decision-of-record — authenticated-authority conflict resolution        |
| 0012 | Accepted | Personal/root bead substrate (storage/sync/tamper)                      |
| 0013 | Superseded | Bead substrate — adopt bd/Dolt as shared store (superseded by 0014)   |
| 0014 | Accepted | Decouple rosary from bd — speak the bead format, own the store          |

## BDR Hierarchy (Decade → Thread → Bead)

Beads are organized into threads (ordered related work) and decades (ADR-level groupings).
`rsry_decompose` parses ADR markdown into atoms, maps to BeadSpecs with frontmatter metadata
(depends_on, target_repo, success_criteria), and groups into the hierarchy.

Current decades:

| Decade                         | Threads                                          | Focus                                |
| ------------------------------ | ------------------------------------------------ | ------------------------------------ |
| `bdr-quality`                  | core, enrichment, active-dedup                   | BDR decompose quality + dedup        |
| `agent-dispatch`               | scope-reign, compute, pipeline, dispatch-quality | Agent hierarchy + dispatch           |
| `infra-workflow`               | linear, jj-git, build-release                    | Infrastructure + workflow            |
| `cross-repo`                   | service-boundaries, deps-severity, leyline-otp   | Cross-repo architecture              |
| `tool-constellation-substrate` | plugin-substrate, capture, github, assay-gates, scoped-notes | Meta-MCP plugin substrate + BDR I/O  |

BDR provenance variants (`crates/bdr/src/provenance.rs`): `Doc`, `Session` (transcript), `Code` (source file), `Meeting`, `SlackThread`. Capture commands produce beads with the matching variant in `derived_from`.

## Plugin System

Plugins extend rosary along an explicit `kind` axis (`src/plugin.rs`):

| Kind         | Purpose                                                                    |
| ------------ | -------------------------------------------------------------------------- |
| `hook`       | Pipeline hook (default) — runs at `pipeline.triage` / `verify` / `close`   |
| `mcp`        | Outbound MCP server — rosary connects to it as a client (planned)          |
| `dispatch`   | Alternative `AgentProvider` (e.g. local model, sprites)                    |
| `state_sink` | Mirrors bead state to an external system (planned)                         |

Plugin discovery: `~/.rsry/plugins/*.toml` and `<repo>/.rosary/plugins/*.toml`.

`pipeline.verify` plugins can return `coverage: f64` — when a bead has
`doc_coverage_min` set in its success criteria, rosary fails the verify gate
if the plugin reports coverage below the threshold.

## MCP Integration

Rosary exposes 31 MCP tools via `rsry serve`. Accessible from:

- Claude Code (stdio transport, configured in MCP settings)
- Claude web (HTTP transport via tunnel)
- Any MCP client

Mache (`mache` MCP) provides structural code intelligence for exploring any repo.
