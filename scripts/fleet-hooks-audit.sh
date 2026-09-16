#!/usr/bin/env bash
# Fleet hook rollout + audit (rosary-e5fd9a, thread trusted-kernel/projection-timing).
#
# The hooks are shell templates installed per repo, so a binary upgrade leaves
# every registered repo running the OLD contract until `rsry hooks install`
# is re-run there. This walks ~/.rsry/config.toml's [[repo]] entries and, per
# repo: (optionally) reinstalls the hooks, then runs `rsry hooks audit` —
# which since rosary-e5fc0e FAILS on a stale hook stamp — and prints a TSV:
#
#   repo  path  exists  hooks_dir  stamp_version  audit_rc  audit_first_problem
#
# Exit non-zero when any repo with a store runs a stale rsry hook or none at
# all (the fleet gate); other audit problems are reported, not gated here.
#
#   scripts/fleet-hooks-audit.sh            # audit only (read-only)
#   scripts/fleet-hooks-audit.sh --install  # rsry hooks install first, then audit
#   FLEET_TSV=/tmp/fleet.tsv scripts/fleet-hooks-audit.sh
#
# Hook files only: this never touches another repo's source, store or git
# history. Repos that do not exist on disk are reported, not failed.
set -u
CFG="${RSRY_CONFIG:-$HOME/.rsry/config.toml}"
OUT="${FLEET_TSV:-/dev/stdout}"
INSTALL=0
[ "${1:-}" = "--install" ] && INSTALL=1
RSRY="${RSRY_BIN:-$(command -v rsry || echo "$HOME/.local/bin/rsry")}"
RUNTIME=$("$RSRY" --version 2>/dev/null | awk '{print $2}')

paths=$(awk -F'"' '/^\[\[repo\]\]/{inrepo=1} inrepo && /^name *=/{n=$2} inrepo && /^path *=/{p=$2; print n "\t" p; inrepo=0}' "$CFG")
printf 'repo\tpath\texists\thooks_dir\tstamp_version\taudit_rc\taudit_first_problem\n' > "$OUT"
fail=0; total=0
while IFS=$'\t' read -r name rawpath; do
  [ -n "$name" ] || continue
  path="${rawpath/#\~/$HOME}"
  total=$((total+1))
  exists=n; hooks_dir=-; stamp=-; rc=-; msg=-
  if [ -d "$path" ] && git -C "$path" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    exists=y
    if [ "$INSTALL" = 1 ] && [ -d "$path/.beads" ]; then
      (cd "$path" && "$RSRY" hooks install >/dev/null 2>&1) || msg="hooks install failed"
    fi
    hooks_dir=$(git -C "$path" rev-parse --git-path hooks 2>/dev/null)
    case "$hooks_dir" in /*) ;; *) hooks_dir="$path/$hooks_dir" ;; esac
    stamp=$(grep -h -m1 -oE '^# rsry-hook pre-push v[0-9.]+' "$hooks_dir/pre-push" 2>/dev/null | awk '{print $4}' | sed 's/^v//')
    [ -n "$stamp" ] || stamp=none
    if [ -d "$path/.beads" ]; then
      out=$(cd "$path" && "$RSRY" hooks audit 2>&1); rc=$?
      first=$(printf '%s\n' "$out" | grep -m1 '✗' | sed 's/^[[:space:]]*//' | cut -c1-160)
      [ -n "$first" ] && msg="$first"
      # The FLEET gate is hook rollout: a stale stamp (audit says so since
      # rosary-e5fc0e) or no rsry hooks at all in a repo that has a store.
      # Other audit problems (cross-repo dep shape, rosary-d93ab7; store
      # drift, rosary-0bd2bf) are reported in the TSV but belong to their own
      # beads, so they do not fail this gate.
      if printf '%s\n' "$out" | grep -q 'HOOK STALE' || [ "$stamp" = none ]; then
        fail=$((fail+1))
      fi
    else
      rc=0; msg="no .beads"
    fi
  fi
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$name" "$path" "$exists" "$hooks_dir" "$stamp" "$rc" "$msg" >> "$OUT"
done <<< "$paths"
echo "fleet: $total repo(s), runtime v$RUNTIME, $fail failing audit(s)" >&2
[ "$fail" -eq 0 ]
