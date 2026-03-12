# loom

Cross-repo task orchestrator. Weaves [beads](https://github.com/gastown/beads) (per-repo issues), [Linear](https://linear.app) (cross-repo projects), and review layers into coordinated work.

## Commands

```
loom scan       # Discover issues across repos, create beads (bottom-up)
loom plan <id>  # Decompose Linear ticket into repo-scoped beads (top-down)
loom sync       # Bidirectional sync: beads ↔ Linear status
loom status     # Aggregated view across all repos
loom dispatch   # Send a bead to a Claude Code agent in an isolated worktree
loom serve      # MCP server (stdio or HTTP)
```

## Flow

```
Linear PRD ──plan──▶ beads (per-repo, file-scoped)
                          │
beads ◀──scan── repo issues (lint, idiom, duplication, docs)
                          │
              dispatch ───▶ Claude Code agent (worktree)
                          │
              sync ───────▶ Linear status update
```

Bidirectional: top-down (Linear → decompose → beads) and bottom-up (scan → beads → sync to Linear).

## Review Layers

| # | Layer | What |
|---|-------|------|
| 1 | Style | lint + format (clippy, golangci-lint) |
| 2 | Idiom | structural patterns (tropo + mache + ley-line) |
| 3 | Cohomology | change coherence (tropo sheaf) |
| 4 | Clone detection | duplication (ley-line embeddings) |
| 5 | Hygiene | no binaries/experiments in remote |
| 6 | Doc coverage | assay |

## Config

`loom.toml` lists repos to manage:

```toml
[[repos]]
name = "mache"
path = "~/remotes/art/mache"

[[repos]]
name = "loom"
path = "~/remotes/art/loom"

[linear]
team = "ART"
```

## Self-Management

Loom manages its own development. `loom scan` finds issues in loom, `loom dispatch` fixes them, `loom sync` tracks in Linear. If it can't manage itself, it can't manage anything else.

## Build

```
cargo build
cargo test
```
