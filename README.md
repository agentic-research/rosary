# 📿 rosary

Rosary keeps track of work across multiple code repos and gets it done automatically.

It finds issues, decides what to work on next, hands tasks to AI agents, checks their work, and reports back. Think of it as a project manager that never sleeps.

## What it does

1. **Finds work** — scans your repos for open issues (stored as "beads" in each repo)
2. **Prioritizes** — scores each issue by urgency, age, and dependencies
3. **Dispatches** — sends the highest-priority work to Claude Code agents
4. **Checks results** — runs a gauntlet of checks: does it compile? do tests pass? is the diff reasonable?
5. **Retries or moves on** — if the agent's fix didn't work, it tries again with backoff. After too many failures, it flags the issue for a human.

## Quick start

```bash
task build    # requires Task (taskfile.dev) — sets PKG_CONFIG_PATH for fuse-t
task test     # 129+ tests

# Register a repo
rsry enable ~/code/my-app

# See what's out there
rsry scan

# Let it run (dry run first to see what it would do)
rsry run --once --dry-run

# For real — single pass, max 3 agents at a time
rsry run --once --concurrency 3

# Continuous loop, checking every 30 seconds
rsry run
```

> **Note**: Use `task build` / `task test` instead of raw `cargo` — the Taskfile sets `PKG_CONFIG_PATH` for the fuse-t dependency via ley-line.

## Commands

| Command | What it does |
|---------|-------------|
| `rsry scan` | Look at all your repos, find open issues |
| `rsry status` | Summary of what's open, ready, blocked |
| `rsry dispatch <id>` | Hand one specific issue to an AI agent |
| `rsry run` | The main loop — find work, do work, check work, repeat |
| `rsry run --provider gemini` | Use Gemini instead of Claude for dispatch |
| `rsry enable [path]` | Register a repo in the global registry (`~/.rsry/repos.toml`) |
| `rsry disable <name>` | Unregister a repo |
| `rsry bead create <title>` | Create a new bead |
| `rsry bead close <id>` | Close a bead |
| `rsry bead list` | List open beads |
| `rsry bead search <query>` | Search beads by title/description |
| `rsry bead comment <id> <body>` | Add a comment to a bead |
| `rsry plan <ticket>` | Fetch a Linear ticket |
| `rsry sync` | List open Linear issues for your team |
| `rsry serve` | Expose rosary as an MCP tool server (8 tools, stdio transport) |

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

This gives Claude access to 8 tools: `rsry_scan`, `rsry_status`, `rsry_list_beads`, `rsry_run_once`, `rsry_bead_create`, `rsry_bead_close`, `rsry_bead_comment`, `rsry_bead_search`.

## Linear integration

Requires `LINEAR_API_KEY` (get one at https://linear.app/settings/api):

```bash
export LINEAR_API_KEY=lin_api_...
export LINEAR_TEAM=ART           # optional, defaults to ART

rsry plan ART-123                # fetch ticket details
rsry sync                        # list open issues for team
```

## Cross-repo tracking

Beads can reference work in other repos via `external_ref` (e.g., `kiln:ll-packaging`). During each reconciliation loop, rosary syncs these references — creating mirror beads in target repos and propagating status changes bidirectionally. This is the "thread" that strings beads across repos.

## Wasteland federation

Rosary can publish beads to the [Wasteland](https://github.com/steveyegge/gastown) wanted board. The `rosary-crypto` crate encrypts private fields (description, notes, design) via ChaCha20-Poly1305 while leaving public fields (title, status, priority) in cleartext. The pipeline: local beads → selective encryption → public GitHub repo → DoltHub wl-commons.

## Architecture

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full technical picture with diagrams.

## Build

```bash
task build     # debug build with fuse-t support
task test      # 121 tests
task lint      # clippy
task all       # fmt + check + lint + test
```

Pre-commit hooks enforce `cargo fmt` and `cargo clippy` on every commit.
