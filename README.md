# loom

Loom keeps track of work across multiple code repos and gets it done automatically.

It finds issues, decides what to work on next, hands tasks to AI agents, checks their work, and reports back. Think of it as a project manager that never sleeps.

## What it does

1. **Finds work** — scans your repos for open issues (stored as "beads" in each repo)
2. **Prioritizes** — scores each issue by urgency, age, and dependencies
3. **Dispatches** — sends the highest-priority work to Claude Code agents
4. **Checks results** — runs a gauntlet of checks: does it compile? do tests pass? is the diff reasonable?
5. **Retries or moves on** — if the agent's fix didn't work, it tries again with backoff. After too many failures, it flags the issue for a human.

## Quick start

```
cargo build
cargo test

# See what's out there
loom scan

# Let it run (dry run first to see what it would do)
loom run --once --dry-run

# For real — single pass, max 3 agents at a time
loom run --once --concurrency 3

# Continuous loop, checking every 30 seconds
loom run
```

## Commands

| Command | What it does |
|---------|-------------|
| `loom scan` | Look at all your repos, find open issues |
| `loom status` | Summary of what's open, ready, blocked |
| `loom dispatch <id>` | Hand one specific issue to an AI agent |
| `loom run` | The main loop — find work, do work, check work, repeat |
| `loom plan <ticket>` | Fetch a Linear ticket and display details (decomposition coming soon) |
| `loom sync` | List open Linear issues for your team (bidirectional sync coming soon) |
| `loom serve` | Expose loom as an MCP tool server (stdio transport) |

## How the loop works

```
  find issues ──► pick the best one ──► give it to an agent
       ▲                                       │
       │                                       ▼
   wait a bit ◄── update status ◄── check the agent's work
```

Each issue goes through these states:

```
open → queued → dispatched → verifying → done
                                      └→ rejected (retry)
                                      └→ blocked (needs human)
```

Failed issues get retried with increasing wait times. After 5 failures or 3 regressions in a row, loom gives up and flags it.

## Config

Tell loom which repos to watch in `loom.toml`:

```toml
[[repo]]
name = "my-app"
path = "~/code/my-app"

[[repo]]
name = "my-lib"
path = "~/code/my-lib"
```

## How issues are stored

Each repo has a `.beads/` directory with a local database (powered by [Dolt](https://www.dolthub.com/)). Loom talks directly to these databases over MySQL — no shelling out to CLI tools.

## Checking the agent's work

After an agent finishes, loom runs these checks in order:

1. Did it actually commit something?
2. Does the code compile?
3. Do the tests pass?
4. Does the linter approve?
5. Is the change a reasonable size?

If any check fails, it stops there. Compile failures mean something is fundamentally wrong. Test or lint failures get retried.

## Self-management

Loom is configured to scan its own repo (`self = true` in loom.toml). The goal is for loom to manage its own development — finding its own bugs, dispatching agents to fix them, and verifying the results. This isn't fully proven yet, but the plumbing is in place.

## MCP server

Expose loom as tools inside Claude Code:

```bash
# Register loom as an MCP server (one-time)
claude mcp add loom -- /path/to/loom serve --transport stdio

# Or with HTTP transport
loom serve --transport http --port 8383
```

This gives Claude access to `loom_scan`, `loom_status`, `loom_list_beads`, and `loom_run_once` — enabling the "agents orchestrating agents" pattern.

## Linear integration

Requires `LINEAR_API_KEY` (get one at https://linear.app/settings/api):

```bash
export LINEAR_API_KEY=lin_api_...
export LINEAR_TEAM=ART           # optional, defaults to ART

loom plan ART-123                # fetch ticket details
loom sync                        # list open issues for team
```

## Architecture

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full technical picture with diagrams.

## Build

```
cargo build
cargo test    # 66 tests
```
