# Configuration

Rosary uses TOML configuration files. Settings are merged from multiple sources (highest priority first):

1. `$RSRY_CONFIG` (env var) — explicit config path
1. `~/.rsry/config.toml` — global user config (repos, Linear, GitHub, backend)
1. `./rosary.toml` — project-level config (overrides for this repo)
1. `./rosary-self.toml` — self-management config (dogfooding)

## Sections

### `[[repo]]` — Repository Registration

Register repos for scanning, dispatch, and sync.

```toml
[[repo]]
name = "rosary"          # Display name
path = "~/remotes/art/rosary"  # Absolute or ~ path
lang = "rust"            # Language hint (auto-detected if omitted)
self = true              # This repo IS rosary (dogfooding flag)
```

Required fields: `name`, `path`. Repos without `.beads/` directories are skipped during scan.

### `[linear]` — Linear Integration

Bidirectional sync with Linear (issue tracker).

```toml
[linear]
team = "AGE"                    # Linear team key
api_key = "lin_api_..."         # Or set LINEAR_API_KEY env var
webhook_secret = "lin_wh_..."   # For webhook verification

[linear.states]
# Override bead status → Linear state mapping
# dispatched = "Working"
# verifying = "Peer Review"

[linear.phases]
# Map phase numbers to Linear project names
# "1" = "Phase 1: Foundation"
# "2" = "Phase 2: Integration"
```

### `[backend]` — Orchestrator State Store

Cross-repo state: decades, threads, pipeline tracking, dispatch history. Required for thread-aware features (thread/decade MCP tools, thread-aware triage, Linear sub-issue projection).

```toml
[backend]
provider = "dolt"                # Only "dolt" supported currently
path = "~/.rsry/dolt/rosary"    # Database directory (auto-initialized)
```

If omitted, hierarchy features degrade gracefully — beads work but threads/decades are unavailable.

### `[github]` — GitHub Integration

PR creation from the dispatch pipeline.

```toml
[github]
token = "gho_..."           # Fine-grained PAT
owner = "agentic-research"  # Default org for PRs
base = "main"               # Default base branch
auto_pr = false             # Auto-create PR on pipeline completion
```

### `[compute]` — Compute Provider

Where agents run. Default is local subprocesses.

```toml
[compute]
backend = "local"    # "local" or "sprites"

[compute.sprites]
token_env = "SPRITES_TOKEN"     # Env var for API token
cpu = 4                         # Default CPU cores
memory_mb = 8192                # Default memory
checkpoint_on_complete = true   # Snapshot on completion
fallback_to_local = true        # Fall back if sprites fails
network_allowlist = ["github.com", "crates.io"]
```

### `[dispatch]` — Pipeline Behavior

```toml
[dispatch]
provider = "claude"              # Default: "claude", "gemini", "acp"
adversarial_provider = "gemini"  # Provider for review phases
max_concurrent = 3               # Max parallel dispatches
```

### `[http]` — HTTP Transport

For `rsry serve --transport http`. Exposes MCP over Streamable HTTP + webhook receiver.

```toml
[http]
port = 8383

[http.tunnel]
provider = "cloudflare"      # Tunnel for public access
domain = "rsry.example.com"  # Custom domain (optional)
account_id = "..."
zone_id = "..."
token_env = "CF_API_TOKEN"
```

## Environment Variables

| Variable                | Purpose                   | Config equivalent             |
| ----------------------- | ------------------------- | ----------------------------- |
| `RSRY_CONFIG`           | Config file path          | —                             |
| `LINEAR_API_KEY`        | Linear API key            | `[linear] api_key`            |
| `LINEAR_TEAM`           | Linear team key           | `[linear] team`               |
| `LINEAR_WEBHOOK_SECRET` | Webhook signing           | `[linear] webhook_secret`     |
| `SPRITES_TOKEN`         | Sprites compute API token | `[compute.sprites] token_env` |

## File Locations

| File                                  | Purpose                                        |
| ------------------------------------- | ---------------------------------------------- |
| `~/.rsry/config.toml`                 | Global config                                  |
| `~/.rsry/dolt/rosary/`                | Backend state DB (decades, threads, pipelines) |
| `~/.rsry/worktrees/{repo}/{bead-id}/` | Agent worktree isolation                       |
| `{repo}/.beads/`                      | Per-repo bead database (Dolt)                  |
| `{repo}/.beads/dolt-server.port`      | Dolt server port                               |
| `{repo}/.beads/metadata.json`         | Database name + settings                       |
| `./rosary.toml`                       | Project-level config                           |
| `./rosary-self.toml`                  | Self-management config                         |
