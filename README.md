# rosary

Rosary keeps track of work across multiple code repos and gets it done automatically.

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
rsry scan

# Let it run (dry run first to see what it would do)
rsry run --once --dry-run

# For real — single pass, max 3 agents at a time
rsry run --once --concurrency 3

# Continuous loop, checking every 30 seconds
rsry run
```

## Commands

| Command | What it does |
|---------|-------------|
| `rsry scan` | Look at all your repos, find open issues |
| `rsry status` | Summary of what's open, ready, blocked |
| `rsry dispatch <id>` | Hand one specific issue to an AI agent |
| `rsry run` | The main loop — find work, do work, check work, repeat |
| `rsry plan <ticket>` | Fetch a Linear ticket and display details (decomposition coming soon) |
| `rsry sync` | List open Linear issues for your team (bidirectional sync coming soon) |
| `rsry serve` | Expose rosary as an MCP tool server (stdio transport) |

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

Failed issues get retried with increasing wait times. After 5 failures or 3 regressions in a row, rosary gives up and flags it.

## Config

Tell rosary which repos to watch in `rosary.toml`:

```toml
[[repo]]
name = "my-app"
path = "~/code/my-app"

[[repo]]
name = "my-lib"
path = "~/code/my-lib"
```

## How issues are stored

Each repo has a `.beads/` directory with a local database (powered by [Dolt](https://www.dolthub.com/)). Rosary talks directly to these databases over MySQL — no shelling out to CLI tools.

## Checking the agent's work

After an agent finishes, rosary runs these checks in order:

1. Did it actually commit something?
2. Does the code compile?
3. Do the tests pass?
4. Does the linter approve?
5. Is the change a reasonable size?

If any check fails, it stops there. Compile failures mean something is fundamentally wrong. Test or lint failures get retried.

## Self-management

Rosary is configured to scan its own repo (`self = true` in rosary.toml). The goal is for rosary to manage its own development — finding its own bugs, dispatching agents to fix them, and verifying the results. This isn't fully proven yet, but the plumbing is in place.

## MCP server

Expose rosary as tools inside Claude Code:

```bash
# Register rosary as an MCP server (one-time)
claude mcp add rosary -- /path/to/rsry serve --transport stdio

# Or with HTTP transport
rsry serve --transport http --port 8383
```

This gives Claude access to `rsry_scan`, `rsry_status`, `rsry_list_beads`, and `rsry_run_once` — enabling the "agents orchestrating agents" pattern.

## Linear integration

Requires `LINEAR_API_KEY` (get one at https://linear.app/settings/api):

```bash
export LINEAR_API_KEY=lin_api_...
export LINEAR_TEAM=ART           # optional, defaults to ART

rsry plan ART-123                # fetch ticket details
rsry sync                        # list open issues for team
```

## Architecture

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full technical picture with diagrams.

## Build

```
cargo build
cargo test    # 66 tests
```
