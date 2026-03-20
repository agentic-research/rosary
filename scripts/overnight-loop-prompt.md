# Overnight Dispatch Loop (Safe Mode)

No conductor. You dispatch specific small beads via rsry_dispatch, review results, open PRs.

## Per-Iteration Protocol

1. **Cleanup check**: `ps aux | grep "claude -p" | grep -v grep` — kill any agents running >15 min
2. **Check rsry_active**: any completed agents with commits?
3. **Review completed**: check worktree commits, push branch, open PR
4. **Dispatch next** (if < 2 running): pick from the queue below
5. **Report**: "N running, N completed, N PRs opened"

## Queue (small, agent-safe beads only)

1. rosary-1f817b — add tracing crate, convert 10 top eprintln! (src/reconcile/, src/dolt/)
2. rosary-f1c135 — human vs agent task delineation flag (src/bead.rs, src/reconcile.rs)

If both complete, look for more small beads via rsry_bead_search — bugs and tasks with clear file scopes, not epics or Ship beads.

## Safety

- Max 2 concurrent dispatches (not 3 — leave headroom)
- Only dispatch beads with explicit file scopes (not `./`)
- Skip beads with issue_type epic/design/research
- If an agent is stuck (>15 min, no new commits in worktree), kill it:
  `kill <pid>` then comment on bead "agent timed out, needs retry"
- Clean stale worktrees each iteration: `git worktree prune`

## When Queue is Empty

Write blog at `docs/blog/overnight-dispatch-2026-03-20.md` and commit.

## Rules
- Push branch + PR (never ff-merge)
- [bead-id] type(scope): description
- Do NOT start conductor
- Do NOT dispatch Ship/P0 epic beads
