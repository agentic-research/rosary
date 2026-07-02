#!/usr/bin/env bash
# God-file gate — block a file from crossing the line threshold *relative to the
# base branch*. No hand-maintained list: `origin/main` IS the baseline. A file
# already over the threshold on main is grandfathered automatically; only a file
# that crosses (or grows further past) the threshold on this branch is flagged.
#
# Metric is `wc -l` — deterministic and directly comparable to the base version
# via `git show`. (mache's `long_file` AST metric is used for the richer
# `task god-files` report; mixing the two metrics is what let a borderline file
# slip through the earlier baseline approach.)
#
# Exit: 0 = ok, 1 = a file crossed the threshold on this branch.
set -uo pipefail

THRESHOLD=${GODFILE_THRESHOLD:-1500}
cd "$(cd "$(dirname "$0")/.." && pwd)"

# Base to compare against — merge-base with origin/main. On main this resolves
# to HEAD, so nothing is ever flagged there (main's god-files are accepted).
BASE="$(git merge-base HEAD origin/main 2>/dev/null || echo HEAD)"

lines_at() { # <ref-or-worktree> <file> ; worktree when ref is empty
  local ref="$1" f="$2"
  if [ -z "$ref" ]; then [ -f "$f" ] && wc -l < "$f" || echo 0
  else git show "$ref:$f" 2>/dev/null | wc -l; fi
}

new_godfiles=""; grown=""
while read -r f; do
  [ -f "$f" ] || continue
  cur=$(wc -l < "$f")
  [ "$cur" -gt "$THRESHOLD" ] || continue
  base=$(lines_at "$BASE" "$f")
  if [ "$base" -le "$THRESHOLD" ]; then
    new_godfiles+="  $f (${base} → ${cur} lines, crossed ${THRESHOLD})"$'\n'
  elif [ "$cur" -gt "$base" ]; then
    grown+="  $f (${base} → ${cur} lines)"$'\n'
  fi
done < <(git ls-files 'src/**/*.rs' 'src/*.rs' 'crates/**/*.rs')

if [ -n "$new_godfiles" ]; then
  echo "FAIL: file(s) crossed the ${THRESHOLD}-line god-file threshold on this branch:" >&2
  printf '%s' "$new_godfiles" >&2
  echo "Split the file into focused modules before merging." >&2
  exit 1
fi
if [ -n "$grown" ]; then
  echo "warning: legacy god-file(s) grew (allowed, but prefer shrinking):" >&2
  printf '%s' "$grown" >&2
fi
echo "god-file gate: ok (no file crossed ${THRESHOLD} lines vs ${BASE:0:12})"
