# Rosary Handoff

## Done This Session

- **Researched**: Claude Code `/loop` + `CronCreate` (session-scoped, 3-day max), Gas Town (Yegge's multi-agent orchestrator, beads, patrols), The Wasteland (federated trust network), Symphony (OpenAI's Linear-driven autonomous code factory in Elixir), OpenFang (too heavy), netstoat-labs/the-firm (multi-agent audit plugin)
- **Installed beads** (`bd init`) in mache repo -- 17 open beads created
- **Migrated** 3x `KNOWN_ISSUES.md` files to beads (validated each against code, 2 items were already fixed, deleted the files)
- **Scanned** `_agent_log/*.md` files for remaining work items, created beads for valid ones
- **Scaffolded rosary** (Rust project, formerly "loom") -- compiles, 66 tests passing
- **Rosary modules**: `main.rs` (CLI), `bead.rs` (state machine), `config.rs` (TOML), `scanner.rs` (multi-repo scan via Dolt), `linear.rs` (GraphQL client), `dispatch.rs` (agent spawning), `reconcile.rs` (reconciliation loop), `verify.rs` (tiered checks), `queue.rs` (priority queue), `serve.rs` (MCP server), `dolt.rs` (MySQL client), `acp.rs` (Agent Client Protocol), `pool.rs` (RepoPool connections), `thread.rs` (cross-repo external_ref sync), `vcs.rs` (jj state versioning via leyline-vcs)
- **Workspace crate**: `crates/crypto/` (`rosary-crypto` — ChaCha20-Poly1305 selective field encryption for Wasteland federation)

## Architecture Decisions

- Rosary is **Rust** (single binary, no runtime deps)
- Beads via native MySQL to Dolt (direct DB access, not CLI subprocess)
- Linear via `reqwest` + raw GraphQL queries
- MCP server via manual JSON-RPC (zero new deps)
- Claude Code spawning via `tokio::process::Command`
- Bidirectional flow: bottom-up (scan -> beads) AND top-down (Linear PRD -> beads)

## The Self-Management Thesis

Rosary's proof of concept is that it manages its own development. Specifically:

1. `rsry scan` should find issues in rosary's own code and create beads
2. `rsry dispatch` should fix those beads using Claude Code agents
3. `rsry sync` should track progress in Linear
4. The review layers (lint, idiom, cohomology, duplication, hygiene, docs) should run against rosary itself
5. `.beads/` in rosary tracks rosary's own work items
6. A `rosary.toml` config at the repo root lists rosary itself + all ART repos

This is the "eating your own dogfood" pattern -- if rosary can't manage its own backlog, it can't manage anyone else's.

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
