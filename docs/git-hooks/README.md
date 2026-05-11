# Git hook templates

These files contain the **rsry-managed shell logic** that `rsry hooks install`
splices into `.git/hooks/post-push` and `.git/hooks/post-merge`.

They are deliberately **not standalone runnable scripts** — they contain just
the body content. `rsry hooks install` wraps each block in a `#!/bin/sh`
shebang and marker comments before writing it to the real hooks dir.

## Layout

- `post-push` — body shell logic that runs after `git push`. Pushes the
  local Dolt beads DB to the configured Dolt remote, best-effort.
- `post-merge` — body shell logic that runs after `git pull` / merge.
  Pulls the latest Dolt beads from the configured Dolt remote, best-effort.

## Why markers

The install command wraps these blocks with `# >>> rsry-managed >>>` /
`# <<< rsry-managed <<<` markers. On re-install, only the content between
markers is regenerated — any user-written hook content outside the markers
is preserved. This means rsry's hooks coexist with custom team hooks
without clobbering them.

To customize: edit `.git/hooks/post-push` (or `post-merge`) and put your
custom logic **outside** the marker block. Reinstalling will only touch
the marked section.

To preview what `rsry hooks install` would write, run `rsry hooks status`
after install — it tells you whether each hook exists, whether it's
rsry-managed, and where the canonical hooks directory lives (which differs
in git worktrees, where `.git` is a file pointer rather than a directory).
