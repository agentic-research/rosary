# I don't write Rust. Last night my agents merged 10 PRs.

*March 20, 2026 — James Gardner*

---

I didn't write a line of Rust today. I've never written Rust before. But by the end of the session, autonomous AI agents had:

- Merged **10 PRs** to rosary's main branch
- Split a 2,716-line god file into 4 clean modules
- Fixed 5 dispatch-blocking bugs
- Organized 371 open beads into 4 decades and 13 threads
- Written an ADR and designed a 4-stage enrichment pipeline
- Codified 3 of 11 Golden Rules as automated pre-commit hooks
- Dispatched themselves, reviewed their own work, and opened their own PRs

And this was only 1/4 terminal windows.

This is the story of rosary's first real overnight dispatch — what worked, what broke, and what it feels like when the recursion starts to bite.

## The setup

Rosary is an autonomous work orchestrator. It tracks work as "beads" (think issues, but stored in Dolt databases per repo), dispatches AI agents to work on them in isolated worktrees, verifies the results, and syncs to Linear for human review.

The human manages work, not agents. The agents manage code.

I'm a platform engineer. I designed the architecture, wrote the ADRs, defined the product tiers. I've never written Rust — every line of rosary's 21,000 lines was written by AI agents, guided by me.

## The session

It started with a broken `git stash`. The jj+git colocated repo had a desync — a file Claude Code created got stuck in git's index, blocking pull and stash. Running `jj status` fixed it in 2 seconds (jj re-snapshots and reconciles the index).

From that bug, we unraveled a chain of infrastructure issues:
- **Symlink split-brain**: `~/github` → `~/remotes` symlink caused beads created via one path to be invisible from the other. Same Dolt DB, different path strings. Fixed by canonicalizing paths in the connection pool.
- **Stale Dolt PIDs**: Dead Dolt servers left PID/port files behind, causing 10-second timeouts on every startup. Fixed by checking if PIDs are alive before trusting port files.
- **Hooks blocking their own fix**: We added a pre-commit hook to enforce bead references in commit messages. Then we couldn't commit the hook update because the branch had the *old* hook that didn't understand the new format. The fix for the hook was blocked by the hook. (We ended up using `--no-verify` to bootstrap, then moved hooks to `~/.rsry/hooks/` — central, not per-branch.)

## The dispatch loop

Once the infrastructure was stable, we started dispatching:

**Level 1** — A trivial test: add a line to CLAUDE.md. Agent completed in 30 seconds, committed, closed its own bead, and we ff-merged to main. Proof of life.

**Level 3** — Split serve.rs (2,716 lines) into 4 modules. Agent ran for 10 minutes, created `serve/mod.rs`, `serve/tools.rs`, `serve/handlers.rs`, `serve/webhook.rs`. All 385 tests passed. Merged.

**Parallel dispatch** — Three agents simultaneously refactoring three god files (reconcile.rs, dolt.rs, dispatch.rs) in isolated worktrees. Non-overlapping file scopes, no collisions.

**Conductor** — The Elixir/OTP conductor manages agent lifecycles. We started it and it immediately dispatched 3 agents. No "no stdin data" warning (we fixed the PTY/stdin issue). But it picked P0 beads that were too ambitious for dev-agents — the triage needs work.

## What broke

**The conductor didn't detect dead agents.** We first tried wrapping the agent spawn in `script` for PTY allocation. `script` stayed alive after the child exited, masking the exit status from Erlang's Port. The fix: `exec "$@" < /dev/null` — `exec` replaces the shell so the Port tracks the real PID, and stdin comes from /dev/null (no warning, no PTY needed for CLI mode).

**Branch protection fought ff-merge.** Agents committed to isolated worktrees and we ff-merged to main. But GitHub's branch protection rules require PRs. The system was fighting itself. The fix: agents should *never* merge to main. They push branches, the feature agent (or human) opens a PR. This became ADR-0008 — the three-tier agent hierarchy.

**Beads piled up faster than dispatch could clear them.** Every conversation generated 10-20 beads. Dispatch handled 3-5. Without admission control, the backlog grows unbounded. The answer isn't "stop filing beads" — it's triage harder, cap per type, and let stale beads age out.

## The architecture that emerged

```
Human (release manager)
  └── Orchestrator (conductor)
        └── Feature agent (manages a thread, opens PRs)
              └── Dev agents (scoped to files, parallel)
```

Each tier has a **scope** (what it sees) and a **reign** (what it can do). As you go up: scope broadens, detail decreases, reign shifts from "edit code" to "dispatch agents" to "approve releases."

Dev agents don't know about each other. Feature agents compose their work. The orchestrator decides what to work on. The human reviews PRs.

## The numbers

| Metric | Count |
|--------|-------|
| PRs opened | 10 (#26-35) |
| PRs merged | 8 (#17-24) |
| Beads closed | ~15 |
| Beads created | ~25 |
| Agent dispatches | 20+ |
| God files split | 4 (serve, reconcile, workspace, dispatch) |
| Lines refactored | ~7,000 |
| Tests passing | 385 (Rust) + 110 (BDR) + 5 (conductor) |
| Golden Rules codified | 3 of 11 |
| ADRs written | 2 (0007, 0008) |
| Decades organized | 4 |
| Threads organized | 13 |
| Duplicates closed | 5 |
| Infrastructure bugs fixed | 5 (symlink, stale PID, hooks, search, enable) |
| Conductor bugs found | 2 (stdin, exit detection) |

## What it feels like

There's a moment around hour 6 where you stop thinking about the code and start thinking about the system. You're watching an agent split a 2,700-line file into modules — code you've never read, in a language you've never written — and the tests pass. You didn't tell it which functions go where. You told it "this file has too many responsibilities" and it figured out the rest.

Then you dispatch three more agents in parallel on three different files. They can't collide because the file scopes don't overlap. That's not magic — that's the system you designed working as intended. The beads have file scopes, the reconciler checks overlap, the worktrees isolate. You designed the rules. The agents follow them.

The recursive moment: the pre-commit hook that enforces bead references in commits was blocking its own commit because the branch had the old hook. The fix for the hook was blocked by the hook. You laugh, you `--no-verify`, you move hooks to a central directory so they're never per-branch again. The system found a bug in itself and you fixed the system, not the bug.

By midnight, you're not coding. You're reviewing PRs that agents wrote, filed, and tested. You're a release manager for a codebase you've never typed into. That's the feeling: you designed the factory, the factory runs, you inspect the output.

## What's next

The gap is the feature agent. Right now a human (me, or Claude in a chat session) plays that role — dispatching dev agents, reviewing their output, opening PRs. When the feature agent is built, the human reviews releases, not PRs. That's the path from "review 10 PRs a day" to "review 1 release a week."

The hosted endpoint at `mcp.rosary.bot` is next. One URL, any MCP client, structural code intelligence for free. The conductor runs on Fly. Agents work overnight. You review in the morning.

Designed by a human. Built by Claude. Managed by Claude. Reviewed by a human.

---

*Rosary is open source (AGPL-3.0): [github.com/agentic-research/rosary](https://github.com/agentic-research/rosary)*
