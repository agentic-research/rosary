# Overnight Dispatch Loop (Manual Mode)

You are the feature agent. Dispatch dev agents, review their work, push branches, open PRs. Conductor is OFF — you manage everything.

## Priority Order
1. Fix conductor Port exit detection bug (rosary-39d1bc) — this unblocks autonomous dispatch
2. Dispatch remaining queue items
3. Write the blog
4. Commit everything

## The Bug to Fix (rosary-39d1bc)
Conductor's AgentWorker GenServer doesn't detect when Erlang Ports exit. Agents die but slots stay "full." Fix is in conductor/lib/conductor/agent_worker.ex — the handle_info for {:EXIT, port, reason} or {port, {:exit_status, code}} isn't firing. Investigate and fix.

Files: conductor/lib/conductor/agent_worker.ex, conductor/lib/conductor/agent_supervisor.ex

## Remaining Queue
After the bug fix, dispatch these (non-overlapping, quick wins):
- rosary-1f817b — add tracing crate, convert 10 top eprintln! (reconcile/, dolt/)
- rosary-f1c135 — human vs agent task delineation flag (bead.rs)
- rosary-e4f182 — rsry enable auto-inits .beads/ (already written, needs PR)

## Per-Iteration Protocol
1. Check rsry_active — any agents running? completed?
2. Review completed agents: check worktree commits, push branch, open PR
3. If slots available (< 3 running): dispatch next from queue
4. Report: "N agents running, N completed, N PRs opened"

## When Queue is Complete

Write the blog post at `docs/blog/overnight-dispatch-2026-03-20.md`:
- Title: "What happens when AI agents run your codebase overnight"
- The real story: started with broken git stash, ended with autonomous dispatch
- Numbers: beads closed, PRs opened, lines changed, agents dispatched
- The bugs: hooks blocking their own fix, conductor not detecting dead agents, symlink split-brain
- The architecture: scope/reign hierarchy, BDR decompose, janitor agent
- What worked, what didn't, what's next
- Tone: builder's log, honest, technical

Then commit the blog + overnight-loop-prompt.md + any other artifacts.

Final status comment on bead rosary-7060ba (janitor epic).

## Rules
- Always push branch + PR (never ff-merge to main)
- Every commit: [bead-id] type(scope): description
- Max 3 concurrent agents
- If agent stuck >15 min, kill and redispatch
- Do NOT commit a Show HN post (human task)
