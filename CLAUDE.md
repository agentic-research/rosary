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
task check          # canonical verification gate: contract + rules + compile + lint + test + smells
task test           # run all tests
task lint           # fmt + clippy -D warnings + semgrep
task smells         # mache structural-smell ratchet (vs docs/smell-baseline.json)
task install        # build release, codesign, install to ~/.local/bin
task all            # alias for `task check`
```

CI (`.github/workflows/ci.yml`) delegates to `task check` — the single canonical
gate — and `scripts/check-taskfile-contract.sh` enforces that CI runs nothing
else. `task ci` is also an alias for `task check`.

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

| File                           | Purpose                                                                                                                                                              |
| ------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| src/serve/mod.rs               | MCP server (stdio + HTTP) + Linear/GitHub webhook handlers                                                                                                           |
| src/serve/handlers/mod.rs      | MCP tool implementations (41 tools)                                                                                                                                  |
| src/serve/github_webhook.rs    | GitHub merge webhook → advance bead + unblock dependents                                                                                                             |
| src/reconcile/mod.rs           | Reconciliation loop: scan → triage → dispatch → verify                                                                                                               |
| src/bead.rs                    | Bead model, BeadState enum, Comment struct (audit-trail), Linear type mapping                                                                                        |
| src/bead_ops.rs                | Shared bead-op core (the API in CLI↔MCP): BeadCreateArgs + validate/create/close gates enforced 1:1 across `rsry bead` (CLI) and `rsry_bead_*` (MCP)                 |
| src/bead_sqlite/mod.rs         | `connect_bead_store` — the single entry point for ALL bead I/O; `SqliteBeadStore` reads `.beads/beads.db` directly (rusqlite) when there's no `.beads/dolt/`         |
| src/bead_dolt.rs               | `DoltBeadStore` — `BeadStore` over the Dolt MySQL client (used when `.beads/dolt/` exists)                                                                           |
| src/bead_migrate.rs            | `migrate_store` + field-level `verify_migration` — Dolt→SQLite bead-store migration (ADR-0021 slice 4). `rsry bead migrate --to sqlite` dry-runs it; `restore_status`/`restore_dependency` are the verbatim-write primitives it needs (state machine + cross-repo edges bypass the opinionated create/add paths) |
| src/dispatch/mod.rs            | Agent dispatch, pipeline mapping, execution                                                                                                                          |
| src/dispatch/providers.rs      | AgentProvider trait — claude / gemini / **codex (`codex exec`)** / acp / plugin-kind="dispatch"; `resolve_launch_env` cred routing (OAuth vs API key, rosary-1be3b8) |
| src/dispatch/codex_runtime.rs  | Codex app-server **runtime** — `classify_turn_signal` + `run_turn_loop` (read past `turn/start`'s ack, answer approvals, observe real completion) + `CodexAppServerRuntime` (oneshot-backed session; `wait()` reflects the turn). Protocol verified vs `codex app-server generate-json-schema` v0.142.5 |
| src/dispatch/codex_transport.rs| Persistent `CodexWebSocketConnection` + `CodexUnixSocketConnector` over the codex Unix control socket; `send_turn_interrupt` (cooperative `kill`)                       |
| src/dispatch/provenance.rs     | `FailureClass` classifier — reads the agent stderr tail → a *classified* dispatch outcome (`failure:auth` / `:skew` / `:missing-binary` / …) so the failure record is the diagnosis (subsumes b1495c/82caac/ACP-skew) |
| src/dispatch/sweep.rs          | Orphan-dispatch detection — self-heals stuck `Dispatched` beads (rosary-67c43d)                                                                                      |
| src/epic.rs                    | Semantic clustering, dedup, file overlap detection                                                                                                                   |
| src/dolt/mod.rs                | Dolt client (per-repo beads, server mode) — used only when `.beads/dolt/` exists                                                                                     |
| src/store_dolt.rs              | Dolt backend for orchestrator state (pipeline, dispatches, cross-repo deps)                                                                                          |
| src/store.rs                   | Backend-agnostic store traits (HierarchyStore, DispatchStore, LinkageStore)                                                                                          |
| src/observation/mod.rs         | ADR-0010 substrate: Observation, FieldName, FieldAlgebra, Observer trait                                                                                             |
| src/observation/algebra\_\*.rs | Per-field algebras: chain-max, LWW-register, OR-set, flat-lattice                                                                                                    |
| src/observation/log.rs         | In-memory G-set (used by tests + integration_tests)                                                                                                                  |
| src/observation/log_sqlite.rs  | Persistent G-set + quarantine via orchestrator SQLite                                                                                                                |
| src/observation/registry.rs    | Field/algebra dispatch via OnceLock singleton                                                                                                                        |
| src/observation/fold.rs        | Per-field per-source fold + cross-source flat-lattice for Status                                                                                                     |
| src/observation/tree_fold.rs   | BDR Decade ⊃ Thread ⊃ Bead catamorphism (rollup status)                                                                                                              |
| src/observation/quarantine.rs  | Cert-validity filter + queryable quarantine surface                                                                                                                  |
| src/observation/shadow.rs      | R4b: read persisted observations back (JSON + legacy flat), fold, `derived_status` (terminal-aware) — shadow-compare vs persist_status                               |
| src/observation/audit.rs       | `rsry lattice audit` — fold every bead, diff derived status vs persist_status (corpus evidence for the source-of-truth flip)                                         |
| src/skills.rs                  | Deterministic skill discovery — resolve skill name → SKILL.md + blake3 digest, fail-loud pre-dispatch (rosary-cf52cf)                                                |
| src/handoff.rs                 | Structured context transfer between pipeline phases                                                                                                                  |
| src/verify.rs                  | Ordered verify tiers (compile → test → review → close-condition); `VerifyTier` trait                                                                                 |
| src/reconcile/verify.rs        | `verify_completed` — verify + pipeline decision; the **feedback-contract gate** (downgrade pass→retry when no `feedback` run-event, rosary-0908bc)                   |
| src/reconcile/completion.rs    | Retry/deadletter logic; `on_fail` writes `.rsry-retry.md` for **fix-forward** retries                                                                                |
| src/pipeline.rs                | `PipelineEngine` — issue_type→agent sequence, DispatchStore delegation, `dispatch_left_feedback` (run-start-gated feedback check)                                    |
| src/dispatch/prompt.rs         | `build_prompt`/`build_system_prompt` — job contract incl. mandatory `feedback` run-event + `<previous_attempt>` fix-forward section                                  |
| src/workspace/mod.rs           | Git/jj worktree creation and isolation                                                                                                                               |
| src/linear.rs                  | Linear sync CLI (`rsry sync`)                                                                                                                                        |
| src/linear_tracker.rs          | IssueTracker trait impl for Linear (cached states, configurable)                                                                                                     |
| src/github_mirror.rs           | `rsry sync --github` — bead context → PR comment                                                                                                                     |
| src/sync.rs                    | Backend-agnostic sync engine                                                                                                                                         |
| src/config/mod.rs              | Configuration (repos, linear, http, tunnel, backend, plugins)                                                                                                        |
| src/plugin.rs                  | Plugin registry + PluginKind axis (Hook / Mcp / Dispatch / StateSink)                                                                                                |
| src/pool.rs                    | Connection pool for multi-repo Dolt access                                                                                                                           |
| src/main.rs                    | CLI entry + shared helpers (`generate_bead_id`, `resolve_beads_dir`)                                                                                                 |
| src/init.rs                    | `rsry init` — onboarding primitive (store + metadata + managed AGENTS.md section, replacing a legacy bd block); handler adds hooks + global register (rosary-aaffb0)  |
| src/capture.rs                 | `rsry capture --from-session/--from-code` — text → BeadSpecs via LLM                                                                                                 |
| src/notes.rs                   | `rsry notes rotate` — age recipient rotation for scoped notes                                                                                                        |
| src/scan_assay.rs              | `rsry scan --assay` — stale-ref → P3 chore beads via assay.scan plugins                                                                                              |
| src/bdr_enrich.rs              | LLM atom extraction (lenient JSON parse — tolerates fences + trailing prose)                                                                                         |
| src/decompose.rs               | `rsry decompose --stub-output` — TechnicalSpec/Constraint atoms → Rust stubs                                                                                         |
| src/secrets.rs                 | Write-time secret pattern scrubber for bead/comment writes                                                                                                           |
| crates/bdr/src/parse.rs        | ADR markdown parser — frontmatter + section → atom extraction                                                                                                        |
| crates/bdr/src/decompose.rs    | Atom → BeadSpec mapper (incl. `doc_coverage_min` field, cross-repo routing)                                                                                          |
| crates/bdr/src/thread.rs       | Thread grouping + Decade assembly from atoms                                                                                                                         |
| crates/bdr/src/accrete.rs      | Bottom-up: bead completions → decade state transitions                                                                                                               |
| crates/bdr/src/provenance.rs   | ProvenanceRef variants: Doc / Session / Code / Meeting / SlackThread                                                                                                 |

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
    └── GOLDEN_RULES.md   # 12 rules all agents operate under
```

Agents map to Linear **labels** (not users — one seat, all perspectives via labels).
Pipeline mapping: issue_type → agent sequence (dispatch.rs `agent_pipeline()`).

## Beads (Issue Tracking)

Beads are the distributed work tracking system. Each repo has `.beads/` with either a Dolt database (`.beads/dolt/`, server mode) or a SQLite `beads.db` (the default for single-user/local repos). Rosary reads/writes both in-process via `connect_bead_store` (`src/bead_sqlite/mod.rs`) — it never invokes the `bd` CLI. See [ADR-0014](docs/adr/0014-decouple-rosary-from-bd.md).

```bash
# MCP tools (via rsry serve) — 41 tools
# Beads
rsry_bead_create / rsry_bead_update / rsry_bead_search / rsry_bead_close
rsry_bead_link / rsry_bead_import / rsry_bead_history
# Comments
rsry_bead_comment / rsry_bead_comment_list / rsry_bead_comment_update / rsry_bead_comment_delete
# Status + triage/review
rsry_status / rsry_list_beads / rsry_scan / rsry_active / rsry_ticket_load / rsry_review / rsry_expand_ref
# Dispatch + pipeline
rsry_dispatch / rsry_run_once / rsry_decompose
rsry_pipeline_upsert / rsry_pipeline_query / rsry_dispatch_record / rsry_dispatch_history
# Agent sessions (agent-native run/session refs)
rsry_agent_run_event_record / rsry_agent_run_events
rsry_agent_session_addresses / rsry_agent_session_message_record
# Workspaces
rsry_workspace_create / rsry_workspace_checkpoint / rsry_workspace_cleanup / rsry_workspace_merge
# Hierarchy (BDR lattice)
rsry_decade_create / rsry_decade_list / rsry_thread_create / rsry_thread_list / rsry_thread_assign / rsry_thread_reparent
# Repo registry
rsry_repo_register / rsry_repo_list

# CLI
rsry sync --dry-run                          # bidirectional Linear sync
rsry sync --github                           # mirror bead context to PR comments
rsry scan                                    # scan all repos for beads
rsry scan --assay                            # run assay.scan plugins → P3 chore beads for stale refs
rsry status [--json] [--repo <name>]         # counts across ALL registered repos (scope with --repo); counts terminal beads too (done/closed), CLI text + JSON agree (#192)
rsry bead create / list / search / close / reopen   # `close` requires a close condition (acceptance_criteria / test command / --force); bare `create` defaults one to the PR-merge signal
rsry bead list --dispatchable / --status all # `--dispatchable` = ready + close-condition + bounded scope + refined (Bead::is_dispatchable); `--status all` includes terminal beads (list defaults to the active-only view)
rsry bead move <id> <dest-repo>              # cross-repo relocation (no bd): provenance+comments fwd, source tombstoned (ADR-0014)
rsry bead backup <file> / restore <file>     # restorable store backup (SQLite VACUUM INTO); Dolt repos pointed at `dolt backup`. Distinct from export --jsonl (interop-only)
rsry bead migrate --to sqlite                # DRY RUN Dolt→SQLite bead-store migration (ADR-0021): reads source → throwaway SQLite copy → field-level verify → reports; changes nothing. The `--commit` atomic swap is a follow-up (rosary-3a0e19)
rsry bead import <file>                       # import a rosary JSON *array* — RE-KEYS (fresh ids), for copying beads into another repo/instance
rsry bead import --jsonl <file>               # id-PRESERVING restore from the `bead export --jsonl` contract (inverse of export): original ids + status + deps + comments verbatim, idempotent (skips present ids). The bd-free `bd init --from-jsonl` (ADR-0014, rosary-9d4951). SQLite-only; the primitive a stash-clobbered store recovery needs
rsry bead merge-jsonl <O> <A> <B>            # git merge driver for the tracked `.beads/beads.jsonl` export (rosary-f9516f) — merges by RECORD not by line: 3-way per bead id, id-sorted output over `%A`, clean for unambiguous edits, LOUD conflict (non-zero + both records kept) when both sides changed the same bead. Never picks a winner. `.gitattributes` references it; `rsry hooks install` configures `merge.beads-jsonl.*`. TRANSITIONAL until dual bead state (rosary-610ad8)
rsry bead comment add <id> <body>            # append a comment (rosary-a96b06)
rsry bead comment list <id> [--include-deleted]
rsry bead comment update <id> <comment_id> --body <text> [--reason <why>]
rsry bead comment delete <id> <comment_id> [--reason <why>] [--hard]   # --hard CLI-only; soft preserves audit trail
rsry close-merged                            # catch-up sweep (gh): close beads whose PRs already merged
rsry close-merged --local                    # rsry-native: close beads from local `git log` squash commits ([bead-id] … (#N)) — no gh/webhook/tunnel; run by the git post-merge hook
rsry init [path] [--dolt] [--no-register]    # onboard a repo (bd-init equivalent, ADR-0014): create `.beads/` store (SQLite default, `--dolt` for server mode) + metadata.json + managed AGENTS.md section (replaces a legacy bd block) + `hooks install` + global register. Idempotent.
rsry hooks install / status                  # install/report the post-merge + post-push bead-sync hooks (post-merge runs `close-merged --local`) + the `beads-jsonl` merge driver config
rsry hooks audit                             # mechanical gate (exit non-zero on failure): .gitignore shadowing beads.jsonl, .beads/embeddeddolt+beads.db backend ambiguity, store/export drift (rosary-b5c8a1)
rsry thread-reparent <thread_id> <decade_id> [--name <new>]  # re-parent threads under a different decade
rsry capture --from-session <path>           # transcript → BeadSpecs via LLM (Session provenance)
rsry capture --from-code <repo> <path>       # source file → BeadSpecs via LLM (Code provenance)
rsry decompose <path> --stub-output <repo>   # also emit Rust stubs for design review
rsry notes rotate --scope <s> --add-recipient <r>  # re-encrypt scope with new age recipient list
rsry lattice audit [--repo <path>]           # fold each bead's observations, diff vs persist_status — R4b corpus evidence for the source-of-truth flip
rsry lattice backfill [--repo <path>] [--limit N] [--dry-run]  # replay the trunk's `[bead-id] … (#N)` squash merges into the lattice as `PipelineVerdict::Done` observations — the corpus `audit` needs. Behavior-neutral (writes `observation` events only; persist_status untouched), idempotent on the commit sha. Git witnesses the terminal MERGE, so a backfilled bead gets ONE Done observation, not a reconstructed lifecycle
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

## Verify & the Feedback Contract

After an agent completes, the reconciler verifies its work through ordered
**tiers** (`src/verify.rs`): compile → `test` (runs `cargo test`) → `review`
(adversarial, nonce-fenced) → `close-condition` (the bead's acceptance
command/criteria). `highest_tier` records how far it got; a tier failure
schedules a backoff retry until `max_retries` (default 5) → deadletter. The
targeted-run loop exits when the **target's own** deadletter id is recorded
(rosary-5361f4 — else it re-dispatches forever). Keep the suite green: a live
test that hard-fails (e.g. a stale-cred integration test) fails **every**
dispatch's `test` tier — mark such tests `#[ignore]` (rosary-59ff84).

**Feedback contract (native + enforceable, rosary-0908bc):** a run is not
complete until the agent records a native `feedback` run-event via
`rsry_agent_run_event_record` (the agent-native run/session substrate, #247).
`PipelineEngine::dispatch_left_feedback` checks for one recorded at/after this
run's start (parsed from the `{bead}-{millis}` dispatch_id, so a prior attempt's
feedback doesn't count) and **downgrades an otherwise-passing action to a retry**
when it's absent. Fail-open when no backend store is configured.

**Fix-forward retries:** on failure, `on_fail` (`src/reconcile/completion.rs`)
writes `.rsry-retry.md` (the failed tier) into the preserved workspace, and
`build_prompt` surfaces it as a `<previous_attempt>` section — so the retry
iterates instead of restarting blind.

**Providers** (`src/dispatch/providers.rs`): `claude` (Keychain OAuth or a
`claude setup-token` OAuth token, routed by token shape — rosary-1be3b8),
`codex` (`codex exec`, file-based `~/.codex/auth.json`, rosary-7643c9), `gemini`,
`acp`, plus the dormant `codex-native` (experimental-gated). Every dispatched
agent is granted `rsry_agent_run_event_record` for the feedback contract.

## ADRs

| ADR  | Status     | Topic                                                                                                                                                                                   |
| ---- | ---------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 0001 | Proposed   | Sprint planning protocol (Explore → Synthesize → Derive → Decompose)                                                                                                                    |
| 0002 | Accepted   | ACP integration (Agent Client Protocol)                                                                                                                                                 |
| 0004 | Accepted   | Dual state machine (bead lifecycle + pipeline phases)                                                                                                                                   |
| 0005 | Proposed   | Reactive persistent store ("local firebase" for agent IPC)                                                                                                                              |
| 0006 | Proposed   | Declarative tool registry (unified MCP/CLI/pipeline from single source)                                                                                                                 |
| 0007 | Proposed   | BDR enrichment pipeline (mache + haiku + sqlite-vec dedup)                                                                                                                              |
| 0008 | Proposed   | Agent hierarchy dispatch model (dev/feature/orchestrator tiers)                                                                                                                         |
| 0009 | Accepted   | Cross-repo linkage — stratified acyclicity + modal evidence                                                                                                                             |
| 0010 | Accepted   | Observation lattice — G-set + per-field fold (substrate; **built + unit-tested, not yet the live source of truth** — `persist_status` still is, promotion tracked as R4b/rosary-a66b3a) |
| 0011 | Accepted   | Decision-of-record — authenticated-authority conflict resolution                                                                                                                        |
| 0012 | Accepted   | Personal/root bead substrate (storage/sync/tamper)                                                                                                                                      |
| 0013 | Superseded | Bead substrate — adopt bd/Dolt as shared store (superseded by 0014)                                                                                                                     |
| 0014 | Accepted   | Decouple rosary from bd — speak the bead format, own the store                                                                                                                          |
| 0015 | Proposed   | Execution-lineage capsules — durable, resumable, proof-ready envelope                                                                                                                   |
| 0016 | Proposed   | Route agent dispatch through cloister's harness plane (rosary-side coordination) — defers to cloister ADR-0040 (control+cred+audit, L0 shipped)/0042/0044; harness runs host-side (workerd can't spawn); Max-OAuth = audit-not-custody; `--disallowedTools`/reaping are stopgaps until libkrun (ADR-0044) |
| 0018 | Accepted   | Structural smell gate via mache's committed-baseline ratchet (`docs/smell-baseline.json` + `docs/smell-rules/*.json` + `task smells`); retired the bash god-file/file-length scripts    |
| 0019 | Proposed   | Harness is the **licensed runtime**, not a swappable front-end — `RuntimeProvider` drives it (seat, `claude -p`), `ModelProvider` replaces it (needs an **API key**, not the subscription OAuth); local models via `ANTHROPIC_BASE_URL`/localhost collapse the wall. Generalizes the custody ceiling (cloister ADR-0040, rosary ADR-0016); empirical evidence rosary-470270. Frames rosary-c79331 |
| 0020 | Proposed   | **Findability by identity** — a bead's stable identity is the digest of its immutable **genesis blob**; state = CAS-advanced DAG tip behind `refs/beads/<BeadId>`; role (canonical/coordination/personal) declared at genesis; storage / sharing / git-visibility / multi-agent coordination all **derive** → SQLite/Dolt stores become rebuildable caches, binary-in-git corruption *structurally impossible*. Connects cloister ADR-0003 (bead DAG) + rosary ADR-0010 (lattice) — same math derived twice. Migration P1–P5 (decade `0020-findability-by-identity`). Evidence: rosary-05fbe0/617010/6e5fc1/560953/75af4d; full analysis `docs/design/findability-by-identity.md` |
| 0021 | Proposed   | **Single-source the bead field lifecycle** — one canonical field set; every surface (store read/write, MCP/CLI args, export, migration) *projects* from it instead of re-declaring it, enforced by a mechanical drift gate (CI fails if any surface omits a canonical field). Diagnoses one defect behind three symptoms: `rosary-4887d0` (create drops `acceptance_criteria` on a surface), the read-lossy trait (`get_bead` omits fields `list_beads` includes), and `rosary-3a0e19` (migration = a 7th hand-rolled field list). The data-model half of ADR-0006; orthogonal to ADR-0020 (which single-sources *identity*, this single-sources the *field set*). Migration falls out for free (read_canonical→write_canonical). Slices 1–4 in the ADR |
| 0022 | Accepted   | **Bead location derives from role** — canonical stays in-tree (`.beads/beads.jsonl`), coordination goes to `refs/agents/<dispatch_id>` (ADR-0020 P4), personal to a private `age`-blob repo (ADR-0012). Diagnosis: rosary has no "where do beads live" problem — it has THREE ROLES IN ONE LOCATION because the other two homes were designed and never built. Q1 answered empirically (`rsry bead diff` renders a readable diff from a git ref with nothing in the working tree → reviewability survives a move, so it is no longer an argument for either side); Q2 `beads.jsonl` is a CONTRACT (ADR-0014 portability); Q3 measured NO — `+refs/heads/*` means a plain clone / `actions/checkout` / GitHub UI see zero beads. Exit test: file a coordination bead, `.beads/beads.jsonl` unchanged. Epic `rosary-fa7167` |

## BDR Hierarchy (Decade → Thread → Bead)

Beads are organized into threads (ordered related work) and decades (ADR-level groupings).
`rsry_decompose` parses ADR markdown into atoms, maps to BeadSpecs with frontmatter metadata
(depends_on, target_repo, success_criteria), and groups into the hierarchy.

Current decades:

| Decade                         | Threads                                                      | Focus                               |
| ------------------------------ | ------------------------------------------------------------ | ----------------------------------- |
| `bdr-quality`                  | core, enrichment, active-dedup                               | BDR decompose quality + dedup       |
| `agent-dispatch`               | scope-reign, compute, pipeline, dispatch-quality             | Agent hierarchy + dispatch          |
| `infra-workflow`               | linear, jj-git, build-release                                | Infrastructure + workflow           |
| `cross-repo`                   | service-boundaries, deps-severity, leyline-otp               | Cross-repo architecture             |
| `tool-constellation-substrate` | plugin-substrate, capture, github, assay-gates, scoped-notes | Meta-MCP plugin substrate + BDR I/O |

BDR provenance variants (`crates/bdr/src/provenance.rs`): `Doc`, `Session` (transcript), `Code` (source file), `Meeting`, `SlackThread`. Capture commands produce beads with the matching variant in `derived_from`.

## Plugin System

Plugins extend rosary along an explicit `kind` axis (`src/plugin.rs`):

| Kind         | Purpose                                                                  |
| ------------ | ------------------------------------------------------------------------ |
| `hook`       | Pipeline hook (default) — runs at `pipeline.triage` / `verify` / `close` |
| `mcp`        | Outbound MCP server — rosary connects to it as a client (planned)        |
| `dispatch`   | Alternative `AgentProvider` (e.g. local model, sprites)                  |
| `state_sink` | Mirrors bead state to an external system (planned)                       |

Plugin discovery: `~/.rsry/plugins/*.toml` and `<repo>/.rosary/plugins/*.toml`.

`pipeline.verify` plugins can return `coverage: f64` — when a bead has
`doc_coverage_min` set in its success criteria, rosary fails the verify gate
if the plugin reports coverage below the threshold.

## MCP Integration

Rosary exposes 41 MCP tools via `rsry serve`. Accessible from:

- Claude Code (stdio transport, configured in MCP settings)
- Claude web (HTTP transport via tunnel)
- Any MCP client

Mache (`mache` MCP) provides structural code intelligence for exploring any repo.
