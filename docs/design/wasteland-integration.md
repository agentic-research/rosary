# Wasteland Integration: Next Steps

**Date**: 2026-03-25
**Beads**: rosary-4158a0 (repo cache wiring), rosary-41980b (auth alignment)
**Status**: Foundation built, wiring needed

## What's Done

### Rosary side
- `src/repo_cache.rs` — On-demand repo cloning (PR #119)
- `src/serve/handlers.rs` — workspace_create, workspace_merge, workspace_checkpoint MCP tools
- `src/workspace/` — Full lifecycle: create worktree → agent works → checkpoint → push → PR
- `src/github.rs` — GitHub App auth, PR creation via REST API
- `src/store.rs` — UserRepoStore trait (register_repo, list_user_repos)
- `src/dispatch/providers.rs` — resolve_auth_token from env/.envrc (PR #115)

### Rig side (done in rig session)
- `POST /api/workspace/create` — thin proxy to rsry_workspace_create MCP
- `POST /api/workspace/pr` — thin proxy to rsry_workspace_merge MCP
- Encrypted GitHub token storage in KV per user
- CF Worker auth (OAuth + session cookies)

## What's Next (can run in parallel)

### Track 1: Wire RepoCache (rosary-4158a0)

**Goal**: `workspace_create` auto-clones registered remote repos.

1. Add `RepoCache` as shared state in MCP server (alongside pool/backend)
2. In `tool_workspace_create`: if `repo_path` doesn't exist locally →
   a. Look up in UserRepoStore by user_id
   b. Get GitHub token ref → decrypt via rig KV
   c. `RepoCache::ensure_local(repo_url, token)` → local clone path
   d. `Workspace::create(bead_id, repo_name, &clone_path, true)`
3. Address Copilot findings from PR #119:
   - Use GIT_ASKPASS instead of URL token injection
   - Per-URL Mutex for clone serialization
   - Add host prefix to cache paths (github.com_org_repo)
   - Validate .git dir before trusting cached clone
   - HTTPS only (drop http except in tests)

**Reference**: mache `serve_hosted.go` getOrCreateRepoClone

### Track 2: Auth Alignment (rosary-41980b)

**Goal**: Rosary uses the same auth pattern as `claude-code-action`.

1. Read `claude-code-action` setup.md + ci-failure-auto-fix.yml
2. `resolve_auth_token()` precedence should match:
   - `ANTHROPIC_API_KEY` (Console key)
   - `CLAUDE_CODE_OAUTH_TOKEN` (setup-token)
   - `.envrc` fallback
3. Add unit tests for auth cascade
4. Document alignment in CONFIGURATION.md

**Reference**: https://code.claude.com/docs/en/github-actions

### Track 3: Ley-line Sheaf Cache (research)

**Goal**: Understand if ley-line's content-addressed caching can improve repo cache.

1. Read ley-line sheaf-cache architecture
2. Assess: is content-addressed storage better than path-based for repo clones?
3. Could shared ley-line arena store git objects across repos?
4. Decision: use ley-line for cache backend or keep simple git clone

### Track 4: End-to-end Test

**Goal**: Prove the full wasteland flow works.

```
User (browser) → rig.rosary.bot/api/workspace/create
  → rsry MCP (HTTP) → workspace_create handler
    → UserRepoStore lookup → RepoCache clone
      → Workspace::create (git worktree)
        → rsry_dispatch → agent runs
          → rsry_workspace_merge → push branch + PR
            → User reviews PR on GitHub
```

Test this with a real repo (e.g., a test repo on agentic-research org).

## Gas Town / Wasteland Protocol Alignment

From Steve Yegge's docs:
- **Rigs** = rosary instances (each user's orchestrator)
- **Beads** = work items (same as rosary beads — Dolt-backed)
- **Convoys** = batched work (rosary threads/decades)
- **Mail protocol** = inter-rig messaging (8 message types: POLECAT_DONE, MERGE_READY, etc.)
- **Federation** = Dolt-based cross-rig sync (design spec, not yet implemented in Gas Town either)

Rosary's advantage: we have the pipeline engine (scoping → dev → staging → prod), verification tiers, and APAS provenance. Gas Town has the federation protocol spec and community. Convergence point: both use Dolt for bead persistence, both use git worktrees for isolation, both need cross-rig bead visibility.

The realistic path: rosary joins the wasteland as a "rig" that speaks the mail protocol. The mail messages map directly to rosary's MCP tools:
- `POLECAT_DONE` → `rsry_workspace_checkpoint`
- `MERGE_READY` → `rsry_workspace_merge`
- `HANDOFF` → `rsry_dispatch` (next pipeline phase)

This is the `rig-1bae29` bead: GitHub-backed wasteland with signet-signed Dolt commits.
