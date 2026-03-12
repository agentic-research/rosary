# Loom Handoff

## Done This Session

- **Researched**: Claude Code `/loop` + `CronCreate` (session-scoped, 3-day max), Gas Town (Yegge's multi-agent orchestrator, beads, patrols), The Wasteland (federated trust network), Symphony (OpenAI's Linear-driven autonomous code factory in Elixir), OpenFang (too heavy), netstoat-labs/the-firm (multi-agent audit plugin)
- **Installed beads** (`bd init`) in mache repo -- 17 open beads created
- **Migrated** 3x `KNOWN_ISSUES.md` files to beads (validated each against code, 2 items were already fixed, deleted the files)
- **Scanned** `_agent_log/*.md` files for remaining work items, created beads for valid ones
- **Scaffolded loom** (Rust project at `~/remotes/art/loom`) -- compiles, 4 tests passing
- **Loom modules**: `main.rs` (CLI), `bead.rs` (bd JSON parser), `config.rs` (TOML), `scanner.rs` (multi-repo scan via bd), `linear.rs` (stubbed), `dispatch.rs` (stubbed)

## Architecture Decisions

- Loom is **Rust** (single binary, no runtime deps)
- Beads via `bd` CLI subprocess (not direct DB access -- beads uses Dolt, not SQLite)
- Linear via `cynic` crate + GraphQL schema (no official Rust SDK)
- MCP server via `rmcp` crate (official Rust MCP SDK)
- Claude Code spawning via `std::process::Command`
- Bidirectional flow: bottom-up (scan -> beads) AND top-down (Linear PRD -> beads)

## The Self-Management Thesis

Loom's proof of concept is that it manages its own development. Specifically:

1. `loom scan` should find issues in loom's own code and create beads
2. `loom dispatch` should fix those beads using Claude Code agents
3. `loom sync` should track progress in Linear
4. The review layers (lint, idiom, cohomology, duplication, hygiene, docs) should run against loom itself
5. `.beads/` in loom tracks loom's own work items
6. A `loom.toml` config at the repo root lists loom itself + all ART repos

This is the "eating your own dogfood" pattern -- if loom can't manage its own backlog, it can't manage anyone else's.

## Next Steps (Priority Order)

1. `git init` + first commit in loom
2. Create `loom.toml` config listing ART repos
3. Wire `loom scan` to actually run (it compiles but needs a config file)
4. Implement `loom status` aggregation
5. Fill in Linear GraphQL client (`cynic` + Linear schema)
6. Fill in dispatch (spawn Claude Code with bead context)
7. Add MCP server (`rmcp`)
8. Set up CI (GitHub Actions)
9. Create beads in loom for loom's own TODOs

## Key References

- **beads CLI**: `bd create`, `bd list --json`, `bd show <id> --json`
- **beads_rust (br)**: SQLite-backed alternative, `.beads/beads.db` readable by mache
- **Linear MCP**: official at https://mcp.linear.app/mcp (OAuth)
- **Claude Agent SDK (Rust)**: community crates `claude-agent-sdk-rs`, `anthropic-sdk-rust`
- **Symphony SPEC.md**: reference architecture for Linear-driven agent orchestration
- **Gas Town Patrols**: linked bead sequences for repeatable workflows

## Related Repos

| Repo | Path | Purpose |
|------|------|---------|
| mache | `~/remotes/art/mache` | Code intelligence, MCP, schema projection |
| assay | `~/remotes/art/assay` | Doc coverage verifier |
| tropo | `~/remotes/art/tropo` | Architecture analysis (layer violations, cycles) |
| ley-line | `~/remotes/art/ley-line` | Tree-sitter + MiniLM embeddings |
| crumb | `~/remotes/art/crumb` | Semantic discovery capture (has evidence field bug) |
| art-hooks | `~/remotes/art/art-hooks` | Hook system (lifecycle, crumb integration) |
| the-firm | `netstoat-labs/the-firm` | Multi-agent audit plugin |
| gt-infra | `netstoat-labs/gt-infra` | Gas Town infrastructure (cron, dispatch, hooks) |

## Review Layer Model

| # | Layer | Tool |
|---|-------|------|
| 1 | Style (lint + format) | golangci-lint / clippy |
| 2 | Idiom (structural patterns) | tropo + mache LSP + ley-line |
| 3 | Cohomology (change coherence) | tropo sheaf (novel, needs work) |
| 4 | Clone detection (duplication) | ley-line embeddings / semgrep |
| 5 | Hygiene (janitor) | git checks, no binaries/experiments in remote |
| 6 | Doc coverage | assay |
