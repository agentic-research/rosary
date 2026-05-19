---
name: note
description: >
  File a bead from conversation context. Use when the user mentions something
  that should be tracked — a bug, idea, task, or observation. Synthesizes a
  bead title and description from the user's input and surrounding conversation.
  Dedup-checks against existing beads before creating.
allowed-tools: "mcp__rsry__rsry_bead_search,mcp__rsry__rsry_bead_create,mcp__rsry__rsry_bead_comment"
argument-hint: "<what to file>"
version: "0.2.0"
author: "ART Ecosystem"
---

# /note — Conversational Bead Capture

File a bead from mid-conversation context. The user says something worth tracking and you turn it into a bead.

## Picking the target repo

**The bead lives in the repo whose code it concerns.** Derive `repo_path` from the file scopes / repo names in the user's input — don't default to rosary.

The old "always use rosary as the hub" rule relied on a reconciler hub-to-spoke sync that is currently broken (`rosary-403a1a`); beads filed under it get stuck in rosary's Dolt and never reach the actual target repo's `.beads/`. Route to the right repo at write time instead.

### Decision rubric

| User input mentions… | Target `repo_path` |
|---|---|
| A specific file path (`notme.bot/worker.ts`, `cloister/vault/...`, `signet/pkg/...`) | That file's repo (`~/remotes/art/notme.bot`, `~/remotes/art/cloister`, `~/github/art/signet`) |
| A specific repo by name (`crumb is crashing`, `mache LSP is wrong`) | That repo's checkout |
| Cross-repo coordination ("we need a way for both `cloister` and `signet` to…") | The primary repo + cross-link, OR rosary as a meta-bead |
| Tooling / orchestration / skill itself ("rsry MCP", "the /note skill", "agent dispatch") | `~/remotes/art/rosary` (rosary is the right home for **meta** concerns about the substrate) |

### Resolving the path

Known ART repos and their checkouts (use the local clone if it exists; fall back to GitHub clone path if not):

- `~/remotes/art/rosary` — agent orchestration + meta
- `~/remotes/art/cloister` — workerd hypervisor + MCP host
- `~/remotes/art/notme.bot` and `~/remotes/art/notme` — auth bridge
- `~/remotes/art/mache` — code intelligence FUSE filesystem
- `~/remotes/art/ley-line` and `~/remotes/art/ley-line-open` — content-addressed storage
- `~/remotes/art/crumb` — semantic knowledge capture
- `~/remotes/art/rig` — infrastructure (terraform / CF)
- `~/github/art/signet` — identity / key exchange

If the right repo isn't on disk, fall back to filing in rosary as a meta-bead with the target repo named in the description, and let the user redirect.

## What To Do

1. **Parse the user's input** (`$ARGUMENTS`) to understand what they want filed
2. **Pick `repo_path`** using the rubric above (file scopes win; repo names by mention win second; rosary for meta only)
3. **Dedup check**: Run `rsry_bead_search` with the chosen `repo_path` and keywords from the input. If the work clearly spans repos, also search rosary.
4. **If duplicate found**: Show it to the user, ask if they want to comment on it instead
5. **If new**: Create a bead with `rsry_bead_create` at the chosen `repo_path`:
   - **title**: Concise, actionable (under 80 chars)
   - **description**: Synthesize from the user's input AND relevant conversation context. If the work targets a different repo than where the bead lives (rare; only for meta-beads), name the target repo in the description.
   - **issue_type**: Infer from context — `bug`, `task`, `feature`, `review`, or `epic`
   - **priority**: Default 2 unless urgency is clear (0=P0 critical, 3=low)
6. **Confirm**: Show the user what was filed (ID + title + which repo)

## Examples

```
/note the CRUMB pydantic bug still crashes on evidence field
→ target repo: crumb (the user named it explicitly)
→ repo_path: ~/remotes/art/crumb
→ searches for "CRUMB pydantic evidence" in crumb
→ creates: bug "CRUMB crashes on evidence field: expects list, gets string"

/note we need to update the beads skill to mention rsry MCP tools
→ target repo: rosary (the /note skill itself is rosary-owned, meta concern)
→ repo_path: ~/remotes/art/rosary
→ searches for "beads skill rsry"
→ creates: task "Update beads skill to reference rsry MCP tools alongside bd CLI"

/note btw sprites API docs were never validated against real responses
→ target repo: rosary (sprites integration lives in rosary's orchestrator)
→ repo_path: ~/remotes/art/rosary
→ creates: task "Validate SpritesClient against real API responses"

/note bug in notme.bot/worker.ts — cookie isn't HttpOnly
→ target repo: notme.bot (the file path tells us where this lives)
→ repo_path: ~/remotes/art/notme.bot
→ searches for "notme.bot worker cookie HttpOnly" in notme.bot
→ creates: bug "notme.bot cookie missing HttpOnly flag"

/note we need a way for cloister bundles to subscribe to signet identity events
→ target repo: cloister (the work lands there; signet is the dependency)
→ repo_path: ~/remotes/art/cloister
→ creates: feature "Subscribe to signet identity events from cloister bundles"
→ note: also consider a cross-link from signet via rsry_bead_link
```

## Key Principles

- **File where the code lives**: derive `repo_path` from file scopes / repo names. Rosary is the meta home, not the universal home.
- **Cross-repo dependencies**: file in the primary repo and use `rsry_bead_link` with the cross-repo target (the partial-fix bead-id-prefix auto-routing also works).
- **Minimal friction**: Don't ask clarifying questions unless the target repo is genuinely ambiguous.
- **Context-aware**: Pull relevant details from the conversation, don't make the user repeat themselves.
- **Dedup first**: Always search before creating. Commenting on an existing bead > creating a duplicate.
- **Brief confirmation**: Just show the ID, title, and which repo, don't be verbose.
