#!/usr/bin/env bash
# End-to-end "fresh setup" test.
#
# Walks the path a brand-new user would take per docs/GETTING_STARTED.md:
#   1. `rsry enable` is rejected with an actionable error when dolt has no
#      global identity (regression test for the silent half-init bug)
#   2. After `dolt config --global` is set, `rsry enable` succeeds end-to-end
#   3. `rsry bead create` + `rsry bead list` + `rsry status` work
#   4. No `[migrate] ... migration 001_add_user_id failed` warning appears
#      (regression test for the dolt 2.x duplicate-column heuristic)
#
# Designed to run inside e2e.Dockerfile against a built rsry binary, but can
# also be executed locally as long as RSRY points at the binary and the
# environment has `dolt` and `git` on PATH.
#
# Exit codes: 0 = all asserts passed; non-zero = first failed assertion.

set -euo pipefail

RSRY="${RSRY:-/usr/local/bin/rsry}"
WORK="${WORK:-/tmp/rsry-e2e}"

RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m'

pass() { echo -e "${GREEN}PASS${NC}: $1"; }
fail() { echo -e "${RED}FAIL${NC}: $1"; exit 1; }

[ -x "$RSRY" ] || fail "rsry binary not executable at $RSRY"
command -v dolt >/dev/null 2>&1 || fail "dolt not on PATH"
command -v git  >/dev/null 2>&1 || fail "git not on PATH"

# Isolate the run from the host's real rosary registry and dolt config by
# pinning HOME to a scratch dir. `rsry` resolves ~/.rsry via dirs_next, and
# `dolt config --global` writes under $HOME — so this single override makes
# the test safe to run on a developer machine without trampling state.
rm -rf "$WORK"
mkdir -p "$WORK/home" "$WORK/sample"
export HOME="$WORK/home"

cd "$WORK/sample"
git init -q
git -c user.email=t@t.t -c user.name=t -c commit.gpgsign=false commit -q --allow-empty -m init

# --- Phase 1: dolt has no identity → enable must fail with actionable hint ---

# HOME is a clean scratch dir, so dolt has no global identity here by
# construction. The explicit --unset calls are defensive in case the script
# is ever sourced from a context where HOME is reused.
dolt config --global --unset user.email >/dev/null 2>&1 || true
dolt config --global --unset user.name  >/dev/null 2>&1 || true

set +e
OUT=$("$RSRY" enable "$WORK/sample" 2>&1)
RC=$?
set -e

[ "$RC" -ne 0 ] || fail "expected enable to fail without dolt identity (got rc=0)"
echo "$OUT" | grep -q 'dolt config --global --add user' \
    || fail "missing actionable hint in error:\n$OUT"
[ ! -d "$WORK/sample/.beads" ] \
    || fail "enable left a half-initialized .beads/ behind"
pass "rsry enable refuses without dolt identity, no half-init"

# --- Phase 2: configure identity, enable must now succeed ---

dolt config --global --add user.email "e2e@example.com" >/dev/null
dolt config --global --add user.name  "e2e"             >/dev/null

"$RSRY" enable "$WORK/sample" >/dev/null 2>&1 \
    || fail "rsry enable failed after dolt identity was set"
[ -f "$WORK/sample/.beads/metadata.json" ] \
    || fail "enable did not produce .beads/metadata.json"
pass "rsry enable end-to-end after dolt identity is configured"

# --- Phase 3: bead create + list + status, capture stderr for noise check ---

CREATE=$("$RSRY" bead -r "$WORK/sample" create "fresh-setup smoke" \
            --priority 2 --files README.md 2>&1)
echo "$CREATE" | grep -q 'created' \
    || fail "bead create output unexpected:\n$CREATE"
pass "rsry bead create"

LIST=$("$RSRY" bead -r "$WORK/sample" list 2>&1)
echo "$LIST" | grep -q 'fresh-setup smoke' \
    || fail "created bead missing from list:\n$LIST"
pass "rsry bead list"

STATUS=$("$RSRY" status 2>&1)
echo "$STATUS" | grep -q '1 bead' \
    || fail "rsry status did not report the new bead:\n$STATUS"
pass "rsry status reports the new bead"

# --- Phase 4: regression check — the dolt 2.x duplicate-column noise is gone ---

# Combine output of two more invocations and assert the specific failing
# migration string never appears. Use both list and status because the
# pre-fix bug surfaced on every read path.
NOISE=$( ("$RSRY" bead -r "$WORK/sample" list; "$RSRY" status) 2>&1 || true )
if echo "$NOISE" | grep -q 'migration 001_add_user_id failed'; then
    fail "migration 001 still warns on every invocation:\n$NOISE"
fi
pass "no '[migrate] migration 001_add_user_id failed' noise on read paths"

echo ""
echo "e2e fresh-setup: all assertions passed"
