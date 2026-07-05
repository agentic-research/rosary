#!/usr/bin/env bash
# Tests for the commit-contract gate (scripts/check-bead-ref.sh).
# Run: bash scripts/check-bead-ref.test.sh
set -uo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
SCRIPT="$DIR/check-bead-ref.sh"
tmp="$(mktemp)"
fail=0

check() { # <expected-exit> <label> <message>
    printf '%s\n' "$3" >"$tmp"
    bash "$SCRIPT" "$tmp" >/dev/null 2>&1
    got=$?
    if [ "$got" != "$1" ]; then
        echo "FAIL: $2 — expected exit $1, got $got  (msg: '$3')"
        fail=1
    fi
}

# --- valid: full contract + escapes ---
check 0 "conventional + bead + scope"      "[rosary-3c19e6] feat(vcs): scan squash body"
check 0 "no scope"                         "[rosary-abc123] docs: update readme"
check 0 "breaking marker"                  "[rosary-abc123] feat(api)!: drop v1"
check 0 "chore type"                       "[rosary-abc123] chore(deps): bump serde"
check 0 "merge escape"                     "Merge branch 'main' into feature"
check 0 "revert escape"                    'Revert "[rosary-x] feat(y): z"'
check 0 "fixup escape"                     "fixup! [rosary-x] feat(y): z"

# --- invalid: each half of the contract, independently ---
check 1 "missing bead ref"                 "feat(vcs): no bead prefix"
check 1 "bead but no conventional type"    "[rosary-abc123] just a plain message"
check 1 "bead but unknown type"            "[rosary-abc123] update(vcs): not a real type"
check 1 "bead + type but no colon"         "[rosary-abc123] feat scan without colon"
check 1 "empty subject after colon"        "[rosary-abc123] feat(vcs): "

rm -f "$tmp"
if [ "$fail" = 0 ]; then
    echo "check-bead-ref: all cases passed"
else
    exit 1
fi
