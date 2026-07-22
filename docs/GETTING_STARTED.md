# Getting Started

How to go from zero to productive with rosary and the ART toolchain.

## Prerequisites

Rosary is developed and tested on **macOS (Apple Silicon)**. Linux mostly works for
building and running the CLI/MCP server; the `task install` launchd setup and the
`codesign` step in the install task are macOS-only.

```bash
# Build toolchain
brew install rustup task capnp                  # rustup for cargo, task for the Taskfile,
rustup-init                                     # capnp for the build.rs codegen step
                                                # (apt: capnproto on Debian/Ubuntu)

# Beads storage — SQLite by default (a local `.beads/beads.db`, no daemon).
# Dolt is OPTIONAL: only needed if you opt a repo into server mode with
# `rsry init --dolt` (version-controlled, multi-writer). Install it only then:
brew install dolt
dolt config --global --add user.email "you@example.com"   # dolt init refuses
dolt config --global --add user.name  "Your Name"         # without an identity

# Version control + FUSE bridge (macOS uses fuse-t; on Linux fuse3 is enough)
brew install jj fuse-t

# ART tools (mache provides structural code intel; rosary uses it via MCP)
brew tap agentic-research/tap
brew install mache
# Note: the `bd` (beads) CLI is NOT required and is never invoked. Rosary reads
# beads in-process — directly against a local SQLite `.beads/beads.db`, or over
# MySQL to a per-repo Dolt server when `.beads/dolt/` exists. Installing `beads`
# is unnecessary unless you want it for ad-hoc scripts outside rosary.

# Claude Code (the AI pair-programming CLI)
npm install -g @anthropic-ai/claude-code
```

You need org access to [agentic-research](https://github.com/agentic-research) on
GitHub for the sibling repos referenced below.

> **Not needed for normal use:** `krust` (the cross-compile driver used by
> `task image` to produce the distroless OCI image) is only required if you
> are building container images. It is not a Homebrew formula — install it
> from source if and when you need it.

## Clone

The Taskfile reads `../ley-line-open/rs/pkgconfig` to wire fuse-t into the
`leyline-vcs` build on macOS, so it expects `ley-line-open` as a **sibling**
of the rosary checkout.

```bash
mkdir -p ~/remotes/art && cd ~/remotes/art
git clone git@github.com:agentic-research/rosary.git
git clone git@github.com:agentic-research/ley-line-open.git   # sibling required on macOS
git clone git@github.com:agentic-research/mache.git
git clone git@github.com:agentic-research/venturi.git         # vulnerability intel (optional)
```

## Build rosary

```bash
cd ~/remotes/art/rosary
task install  # builds release, codesigns, installs to ~/.local/bin, sets up HTTP MCP service
```

On macOS this also installs a launchd service (`com.rosary.serve`) that runs the HTTP
MCP server on port 8383. It auto-restarts when the binary changes (i.e., after each
`task install`).

> **Linux users:** `task install` invokes `codesign` (macOS-only) and the launchd
> setup is gated to Darwin. Use `task release` to produce the binary, then either
> copy it into `~/.local/bin` yourself or run `rsry serve --transport stdio`
> directly from each Claude Code session.

Verify: `rsry status` should print bead counts (or zeros if no repos are registered yet).

## Register your repos

```bash
cd ~/path/to/your/project
rsry enable .
```

This registers the repo in `~/.rsry/config.toml`, creates a **SQLite** bead
store (`.beads/beads.db`) — or a Dolt server store (`.beads/dolt/<repo-name>/`)
if you pass `--dolt` — writes `.beads/metadata.json`, and installs git hooks.
You can also edit the config directly:

```toml
[[repo]]
name = "my-project"
path = "~/path/to/your/project"
lang = "rust"  # or "go", "python", etc.
```

See [CONFIGURATION.md](CONFIGURATION.md) for all options.

## Start Claude Code with the rsry MCP

From any registered repo:

```bash
claude
```

If rsry is configured as an MCP server in your Claude Code settings, you now have 41 tools for managing beads, dispatching agents, and creating workspaces — all available inside your Claude session.

To add rsry as an MCP server, add to your global Claude Code MCP config at `~/.claude/.mcp.json`:

> ⚠️ Use the **global** path. Avoid project-level `.mcp.json` for trusted infrastructure
> like rsry — project-level entries override global ones, so a hostile project shipping
> a `.mcp.json` with a hijacked `rsry` name can redirect a name you trust.

```json
{
  "mcpServers": {
    "rsry": {
      "type": "http",
      "url": "http://localhost:8383/mcp"
    }
  }
}
```

`task install` sets up the HTTP server automatically via launchd. All Claude Code sessions share one server — no stale binary problem after updates.

## The 0-to-1 workflow

### Phase 1: Ingest what you know

Dump your existing knowledge into the repo. Markdown files, docs, notes, analysis — anything you have. Claude is excellent at ingesting unstructured documents and extracting structure from them.

```
> Here are my findings so far [paste or reference files].
> Help me organize these into beads.
```

Each discrete finding, task, or question becomes a bead. Beads are atomic — one thing per bead, with a clear "done" condition.

### Phase 2: Define constraints, not tasks

Rather than writing detailed task descriptions, define measurable constraints that beads should satisfy. Think of it as **constraint-driven development**:

- "Every network endpoint must be documented with its data flow"
- "Files must stay under 200 lines"
- "No hardcoded credentials"
- "Every public function has a test"

Constraints that can be checked by code (linting, grep patterns, test suites) become verification tiers. Constraints that require judgment (architecture quality, naming clarity) become agent review criteria.

The [Golden Rules](../agents/rules/GOLDEN_RULES.md) are rosary's built-in constraints. You can add project-specific ones.

### Phase 3: Let Claude iterate with `/loop`

This is where the system starts compounding. Use `/loop` to have Claude periodically review and refine:

```
/loop 5m review all beads and docs, identify gaps, contradictions, or new threads to pull
```

Start with simple, low-risk tasks:

- Organizing findings into beads
- Cross-referencing docs
- Identifying missing test coverage
- Flagging constraint violations

Then scale up:

- Dispatching agents to fix beads
- Running the reconciliation loop
- Multi-repo coordination

### Phase 4: Dispatch

Once you have beads and confidence in the constraints, let rosary dispatch agents:

```bash
rsry run --once  # single reconciliation pass: scan → triage → dispatch → verify
```

Or from within Claude Code, use the `rsry_dispatch` MCP tool.

Rosary dispatches agents into isolated workspaces (jj workspaces or git worktrees). Each agent works a single bead, in isolation, against the verification pipeline. If the work passes all tiers (compile → test → lint → close-condition → diff sanity → review), it's done. If not, it retries with backoff or deadletters for human attention.

The default is 3 concurrent dispatches. Start with 1 (`max_concurrent = 1` in config) until you trust the loop.

## Key concepts

| Concept          | What it means                                                                                                                                                                   |
| ---------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Bead**         | Atomic work item. One clear task, one clear "done" condition. Lives in a repo's `.beads/` database.                                                                             |
| **Constraint**   | A measurable property that code must satisfy. Codifiable constraints become verification tiers. Judgment constraints become agent review criteria.                              |
| **Reconciler**   | The control loop: scan repos for beads → triage by priority and dependencies → dispatch agents → verify results. Kubernetes-controller style.                                   |
| **Workspace**    | Isolated VCS environment where an agent works. Created per-dispatch, destroyed after verification. Prevents agents from stepping on each other.                                 |
| **Pipeline**     | Sequence of agent perspectives a bead passes through. A bug gets `dev → staging`. A feature gets `dev → staging → prod`. Each phase is a different agent with a different lens. |
| **Verification** | Ordered tier check after agent work (`src/verify.rs`): commit exists → bead ref → compiles → tests pass → lint → close-condition (if declared) → diff sanity → mache blast-radius/duplication (advisory) → adversarial review. First failure short-circuits.                       |

See [glossary.md](glossary.md) for the full term reference.

### Authoring a bead

Every bead needs a **close condition** — a declared way to know it's done, so
an observation (a merged PR, a passing test) can actually close it (ADR-0010).
You don't have to spell one out for quick capture: an implementation bead
created without one defaults to the honest PR-merge signal (rosary's GitHub
merge webhook advances the bead when its linked PR merges).

```bash
# Frictionless — gets the PR-merge default close condition:
rsry bead create "Fix flaky retry backoff" --files src/retry.rs

# Recommended — declare a sharper close condition up front:
rsry bead create "Fix flaky retry backoff" --files src/retry.rs \
    --acceptance "cargo test retry::backoff_is_bounded"
```

`--acceptance` takes either a runnable command or a resolution statement.
Planning beads (`epic`/`design`/`research`/`review`) are exempt — they describe
work rather than ship a verifiable behavior. `rsry bead close` enforces the same
condition: a bead authored with `--force` (no condition) must either gain one
before it closes, or be closed with `rsry bead close --force` to bypass the gate
deliberately.

## Mache: structural code intelligence

Mache gives you (and Claude) structural understanding of codebases. Instead of grepping through thousands of files, you get:

```bash
# Start the mache service (if not already running via brew services)
brew services start mache
```

Then from Claude Code, the mache MCP tools are available:

- `get_overview` — structural map of a codebase
- `get_communities` — discover clusters of related code
- `find_definition` / `find_callers` / `find_callees` — symbol navigation
- `search` — structural pattern search

This is especially valuable when working with unfamiliar or decompiled codebases where you need to understand structure before you can make targeted changes.

## The reconciler

`rsry run --once` / `rsry run` drives the core loop: scan → triage → dispatch → verify → push branch → create PR. Agents work in isolated worktrees, verification runs compile/test/lint, and the terminal step rebases onto latest main and creates a PR via GitHub App.

Single-phase (dev-agent) by default; multi-phase pipelines (dev → staging → prod) advance per issue type, carrying structured handoffs between phases (see [ARCHITECTURE.md](ARCHITECTURE.md#pipeline-phase-advancement)).

## What to know about jj + git

Rosary uses [jj](https://martinvonz.github.io/jj/) (Jujutsu) for workspace isolation when available, with git worktrees as fallback. If you're using jj with colocated git repos:

- `jj git import` / `jj git export` keeps the two in sync
- `git stash` behaves unexpectedly in colocated mode — prefer jj's native workflow
- Agent workspaces are isolated from your working copy regardless of which VCS backend is used

You don't need to use jj yourself. Rosary handles workspace creation and cleanup. But if you see jj-related state, that's why.

## Two modes, not a ladder

Rosary has two operating modes. They're parallel, not sequential — pick the one that fits the work.

### Collaborative: human + Claude + `/loop`

You're in a Claude Code session. Beads track your work. MCP tools give you and Claude shared state over the same repos, the same beads, the same code. `/loop` lets Claude iterate on your behalf — reviewing, refining, cross-referencing — while you set direction.

```
/loop 5m review all beads, identify gaps, flag constraint violations
```

This is the mode for exploratory work, analysis, research, onboarding to a new codebase — anything where human judgment drives and Claude grinds.

### Autonomous: `rsry run`

No human in the loop. Rosary scans repos for open beads, triages by priority and dependencies, dispatches agents into isolated workspaces, verifies results (compile → test → lint → close-condition → diff sanity → review), and creates PRs. You review in the morning.

```bash
rsry run --once   # single pass
rsry run          # continuous reconciliation
```

This is the mode for well-defined work with clear constraints. The verification pipeline is the safety net — agents can only ship code that passes all tiers. Beads that fail too many times are deadlettered for human attention. The system stops rather than making things worse.

### Starting out

Begin with either mode depending on your work:

- **New to a codebase?** Collaborative. Ingest docs, explore with mache, capture findings as beads.
- **Have a backlog of well-scoped beads?** Autonomous. Let `rsry run --once` chew through them.

Both modes use the same beads, same constraints, same verification. The difference is who's driving.

## Troubleshooting

**`cargo build` fails on a missing `capnp` binary**: The `build.rs` step shells out
to the Cap'n Proto schema compiler to generate Rust bindings from
`schemas/cloister.capnp`. Install it with `brew install capnp` (macOS) or
`apt-get install capnproto` (Debian/Ubuntu).

**`rsry enable --dolt` errors with "dolt has no global user.name set"**: the
`--dolt` (server-mode) path runs `dolt init`, which refuses to operate without a
configured identity. (The default SQLite path needs none.) Configure once, then
retry:

```bash
dolt config --global --add user.email "you@example.com"
dolt config --global --add user.name  "Your Name"
rsry enable .
```

`rsry enable` now refuses up-front when the identity is missing rather than
leaving behind a half-initialized `.beads/` directory. (Older binaries
suppressed the dolt error; if you have a partial `.beads/` from one of
those, `rm -rf .beads` and rerun `rsry enable .`.) The full fresh-setup
flow is covered by `task e2e:fresh`, which builds a clean Ubuntu image and
asserts both the failure mode and the happy path.

**`rsry bead create … --type bug` returns "unexpected argument '--type' found"**:
The CLI flag is `--issue-type` (short `-t`). The README's quick-reference example
uses the long form `--issue-type` now; older copies of the docs may still show
`--type`.

**`[bead] migration warning … migration 001_add_user_id failed: ALTER TABLE issues`
appears on every command**: Fixed. The migration heuristic now recognizes
Dolt 2.x's `Column "..." already exists` error string in addition to the
classic `Duplicate column name` form, so migration 001 is correctly
recorded as applied on the first read. If you still see this warning, your
`rsry` binary predates the fix — rebuild with `task install`.

**`rsry status` shows nothing**: Your repos aren't registered or don't have `.beads/` directories. Check `~/.rsry/config.toml` and run `rsry enable <path>` in each repo.

**Dolt connection errors**: Only `.beads/` directories that contain a `dolt/` subdir run a Dolt SQL server; a `.beads/` with just `beads.db` uses SQLite directly (no server, no port file). For Dolt-mode repos, check `dolt sql-server` is on your PATH and that the port file (`.beads/dolt-server.port`) isn't stale.

**Agent dispatch fails immediately**: Check that the configured `[dispatch] provider` CLI (`claude`, `codex`, or `gemini`) is on your PATH.

**Workspace cleanup**: Abandoned worktrees live in `~/.rsry/worktrees/`. Safe to delete if no agents are running.

## Next steps

- [ARCHITECTURE.md](ARCHITECTURE.md) — system design, state machines, module layout
- [CONFIGURATION.md](CONFIGURATION.md) — all config sections and environment variables
- [glossary.md](glossary.md) — term reference
- [agents/rules/GOLDEN_RULES.md](../agents/rules/GOLDEN_RULES.md) — the 11 constraints all agents operate under
