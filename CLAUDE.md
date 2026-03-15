# rosary

Rust-based agent orchestration and work tracking. Backbone of the ART (Agentic Research Toolkit) platform.

## What Rosary Does

1. **Scans** repositories for work items (beads) stored in `.beads/` (Dolt)
2. **Dispatches** agents to execute work — see `agents/` directory
3. **Reconciles** state via a k8s-controller-style loop: scan → triage → dispatch → verify
4. **Syncs** bidirectionally with Linear (beads are source of truth, Linear is UI)
5. **Serves** MCP over stdio and HTTP (Streamable HTTP transport)
6. **Receives** Linear webhooks for real-time state sync

## Development

```bash
task build          # debug build
task test           # run all tests (211+)
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

### Services (launchd)
- `dev.rsry.serve` — HTTP server on :8383
- `dev.rsry.tunnel` — Cloudflare tunnel routing `rsry.q-q.dev` → localhost:8383
- Plists in `~/Library/LaunchAgents/`

### Config
- `~/.rsry/config.toml` — global config (repos, linear settings, webhook secret)
- `rosary.toml` — local/project config
- `rosary-self.toml` — self-management (dogfooding)

## Key Source Files

| File | Purpose |
|------|---------|
| src/serve.rs | MCP server (stdio + HTTP) + Linear webhook handler |
| src/reconcile.rs | Reconciliation loop: scan → triage → dispatch → verify |
| src/bead.rs | Bead model, BeadState enum, Linear type mapping |
| src/dispatch.rs | Agent dispatch and execution |
| src/dolt.rs | Dolt database client |
| src/linear.rs | Linear sync CLI (`rsry sync`) |
| src/linear_tracker.rs | IssueTracker trait impl for Linear (cached states, configurable) |
| src/sync.rs | Backend-agnostic sync engine |
| src/config.rs | Configuration (repos, linear, http, tunnel) |
| src/pool.rs | Connection pool for multi-repo Dolt access |

## Agent Definitions

```
agents/
├── dev-agent.md          # Implementation quality (function-level)
├── staging-agent.md      # Test validity (adversarial review)
├── prod-agent.md         # Production quality (module-level)
├── feature-agent.md      # Cross-file coherence
├── pm-agent.md           # Strategic perspective (cross-repo)
└── rules/
    └── GOLDEN_RULES.md   # 10 rules all agents operate under
```

Agents map to Linear **labels** (not users — one seat, all perspectives via labels).

## Beads (Issue Tracking)

Beads are the distributed work tracking system. Each repo has `.beads/` with a Dolt database.

```bash
# MCP tools (via rsry serve)
rsry_bead_create / rsry_bead_search / rsry_bead_comment / rsry_bead_close
rsry_status / rsry_list_beads / rsry_scan / rsry_dispatch / rsry_active

# CLI
bd create / bd search / bd comments / bd close
rsry sync --dry-run    # bidirectional Linear sync
```

## MCP Integration

Rosary exposes 10 MCP tools via `rsry serve`. Accessible from:
- Claude Code (stdio transport, configured in MCP settings)
- Claude web (HTTP transport via `rsry.q-q.dev`)
- Any MCP client

Mache (`mache` MCP) provides structural code intelligence for exploring any repo.
