---
name: note
description: >
  File a bead from conversation context. Use when the user mentions something
  that should be tracked — a bug, idea, task, or observation. Synthesizes a
  bead title and description from the user's input and surrounding conversation.
  Dedup-checks against existing beads before creating.
allowed-tools: "mcp__rsry__rsry_bead_search,mcp__rsry__rsry_bead_create,mcp__rsry__rsry_bead_comment"
argument-hint: "<what to file>"
version: "0.1.0"
author: "ART Ecosystem"
---

# /note — Conversational Bead Capture

File a bead from mid-conversation context. The user says something worth tracking and you turn it into a bead.

## What To Do

**Always use `repo_path: ~/remotes/art/rosary`** for all rsry calls. Rosary is the central hub — it syncs beads out to per-repo Dolt databases via its reconciliation loop.

1. **Parse the user's input** (`$ARGUMENTS`) to understand what they want filed
2. **Dedup check**: Run `rsry_bead_search` on rosary with keywords from the input
3. **If duplicate found**: Show it to the user, ask if they want to comment on it instead
4. **If new**: Create a bead with `rsry_bead_create` on rosary:
   - **title**: Concise, actionable (under 80 chars)
   - **description**: Synthesize from the user's input AND relevant conversation context. Mention the relevant repo if not rosary itself.
   - **issue_type**: Infer from context — `bug`, `task`, `feature`, `review`, or `epic`
   - **priority**: Default 2 unless urgency is clear (0=P0 critical, 3=low)
5. **Confirm**: Show the user what was filed (ID + title)

## Examples

```
/note the CRUMB pydantic bug still crashes on evidence field
→ searches for "CRUMB pydantic evidence"
→ creates: bug "CRUMB crashes on evidence field: expects list, gets string" in crumb repo

/note we need to update the beads skill to mention rsry MCP tools
→ searches for "beads skill rsry"
→ creates: task "Update beads skill to reference rsry MCP tools alongside bd CLI"

/note btw sprites API docs were never validated against real responses
→ searches for "sprites API docs"
→ creates: task "Validate SpritesClient against real API responses"
```

## Key Principles

- **Minimal friction**: Don't ask clarifying questions unless truly ambiguous
- **Context-aware**: Pull relevant details from the conversation, don't make the user repeat themselves
- **Dedup first**: Always search before creating. Commenting on an existing bead > creating a duplicate
- **Brief confirmation**: Just show the ID and title, don't be verbose
