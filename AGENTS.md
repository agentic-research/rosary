# Agent Instructions

This project tracks all work as **beads**, stored in `.beads/` (a SQLite
`beads.db`, or a Dolt server when `.beads/dolt/` exists) and accessed **through
`rsry`** — the CLI or the `rsry_*` MCP tools. Rosary owns the store and reads/
writes it in-process; it **never invokes the `bd` CLI** (see
[ADR-0014](docs/adr/0014-decouple-rosary-from-bd.md)). Do not run `bd`.

## Quick Reference

```bash
rsry bead list --dispatchable      # work that's actually safe to pick up
rsry bead list --ready             # open + unblocked (superset of dispatchable)
rsry bead review <id>              # full context: summary + comments + change-set
rsry bead close <id>               # complete work (requires a close condition)
rsry status --repo <name>          # counts for one repo (omit --repo = all repos)
```

The `rsry_*` MCP tools mirror these (`rsry_list_beads`, `rsry_bead_create`,
`rsry_bead_close`, `rsry_bead_comment`, …) and are the preferred surface from
inside an MCP client.

## Non-Interactive Shell Commands

**ALWAYS use non-interactive flags** with file operations to avoid hanging on
confirmation prompts. Shell commands like `cp`, `mv`, and `rm` may be aliased to
include `-i` (interactive) mode on some systems, causing the agent to hang
indefinitely waiting for y/n input.

**Use these forms instead:**

```bash
cp -f source dest           # NOT: cp source dest
mv -f source dest           # NOT: mv source dest
rm -f file                  # NOT: rm file
rm -rf directory            # NOT: rm -r directory
cp -rf source dest          # NOT: cp -r source dest
```

**Other commands that may prompt:**

- `scp` — use `-o BatchMode=yes` for non-interactive
- `ssh` — use `-o BatchMode=yes` to fail instead of prompting
- `apt-get` — use `-y`
- `brew` — use `HOMEBREW_NO_AUTO_UPDATE=1`

## Issue Tracking with beads (via `rsry`)

**IMPORTANT**: All issue tracking goes through beads. Do NOT use markdown TODOs,
task lists, or external trackers. Do NOT use the `bd` CLI — use `rsry`.

### Create work

```bash
rsry bead create "Issue title" --description "Detailed context" \
  --issue-type bug --priority 1 --files src/foo.rs
```

Implementation beads (`bug`/`feature`/`task`/`chore`) require a **file scope**
(`--files`) and a **close condition** (an `--acceptance-criteria`, a runnable
test command in the description, or the default PR-merge signal). Planning types
(`epic`/`design`/`research`) and `review` are exempt from the file-scope rule.

### Claim, update, complete

```bash
rsry bead comment add <id> "progress note"
rsry bead close <id>               # gated on the close condition
```

### Issue types

`bug`, `feature`, `task`, `chore` (implementation) · `epic`, `design`,
`research` (planning) · `review` (read-only adversarial). A secondary
`work_mode` axis (investigation / synthesis / adversarial / procedural / …)
maps back to a canonical issue type when a bead is authored.

### Priorities

- `0` — Critical (security, data loss, broken builds)
- `1` — High (major features, important bugs)
- `2` — Medium (default)
- `3` — Low (polish, optimization)

### Ready vs dispatchable

`rsry bead list --ready` = open + unblocked. `--dispatchable` is the strict
subset that is actually safe to hand to an agent: it also has a close condition,
a bounded file scope, and a refined description (`Bead::is_dispatchable`). Prefer
`--dispatchable` when choosing what to work on.

### State sync (automatic, rsry-native)

There is **no** `bd dolt push` / `issues.jsonl` export step. Rosary owns the
store. When a PR merges, the git `post-merge` hook (installed by
`rsry hooks install`) runs `rsry close-merged --local`, which reads the
squash-merge commit (`[bead-id] … (#N)`) from local `git log` and closes the
bead — no webhook, no `gh`, no manual export. Dolt-backed repos also `dolt pull`
in the same hook.

## Landing the Plane (Session Completion)

**When ending a work session**, complete ALL steps. Work is NOT complete until
`git push` succeeds.

1. **File beads for remaining work** — `rsry bead create …` for anything that
   needs follow-up.
1. **Run the gate** (if code changed) — `task check` (the canonical verification
   gate: contract + rules + compile + lint + test + smells).
1. **Update bead status** — close finished work; comment on in-progress items.
1. **PUSH TO REMOTE** — mandatory:
   ```bash
   git pull --rebase
   git push
   git status   # MUST show "up to date with origin"
   ```
1. **Clean up** — clear stashes, prune remote branches.
1. **Hand off** — provide context for the next session.

**CRITICAL RULES:**

- Work is NOT complete until `git push` succeeds.
- NEVER stop before pushing — that strands work locally.
- If push fails, resolve and retry until it succeeds.
