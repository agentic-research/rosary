#!/usr/bin/env bash
# End-to-end "fresh setup" test.
#
# Walks the path a brand-new user would take per docs/GETTING_STARTED.md:
#   1. `rsry enable` defaults to SQLite and succeeds with NO dolt identity,
#      writing the .beads/.gitignore guard (rosary-75af4d / rosary-05fbe0)
#   1b. `rsry enable --dolt` still preflights dolt identity — rejected with an
#      actionable error + no half-init when absent (the half-init regression
#      guard, now on the Dolt path)
#   2. After `dolt config --global` is set, `rsry enable --dolt` succeeds
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
mkdir -p "$WORK/home" "$WORK/sample" "$WORK/sample-sqlite"
export HOME="$WORK/home"

# Two sample repos: `sample` exercises the `--dolt` path, `sample-sqlite` the
# SQLite-default path (rosary-75af4d).
for s in sample sample-sqlite; do
  git -C "$WORK/$s" init -q
  git -C "$WORK/$s" -c user.email=t@t.t -c user.name=t -c commit.gpgsign=false commit -q --allow-empty -m init
done
cd "$WORK/sample"

# --- Phase 1: `rsry enable` defaults to SQLite — succeeds with NO dolt identity ---

# HOME is a clean scratch dir, so dolt has no global identity here by
# construction. The explicit --unset calls are defensive in case the script
# is ever sourced from a context where HOME is reused.
dolt config --global --unset user.email >/dev/null 2>&1 || true
dolt config --global --unset user.name  >/dev/null 2>&1 || true

# rosary-75af4d: `enable` used to hardcode a Dolt store (needing dolt identity);
# it now defaults to a single-file SQLite store, so a fresh enable must SUCCEED
# even with no dolt identity — and must NOT create a Dolt store.
set +e
OUT=$("$RSRY" enable "$WORK/sample-sqlite" 2>&1)
RC=$?
set -e

[ "$RC" -eq 0 ] \
    || fail "expected enable (SQLite default) to succeed without dolt identity (rc=$RC):\n$OUT"
[ -f "$WORK/sample-sqlite/.beads/beads.db" ] \
    || fail "enable (SQLite) did not create .beads/beads.db"
[ ! -d "$WORK/sample-sqlite/.beads/dolt" ] \
    || fail "enable (SQLite default) unexpectedly created a Dolt store"
[ -f "$WORK/sample-sqlite/.beads/.gitignore" ] \
    || fail "enable (SQLite) did not write the .beads/.gitignore guard (rosary-05fbe0)"
pass "rsry enable defaults to SQLite, no dolt identity required"

# --- Phase 1b: `rsry enable --dolt` still preflights dolt identity ---

# The silent half-init regression guard now lives on the Dolt path: `--dolt`
# with no global identity must fail with an actionable hint and leave no
# half-init behind.
set +e
OUT=$("$RSRY" enable --dolt "$WORK/sample" 2>&1)
RC=$?
set -e

[ "$RC" -ne 0 ] || fail "expected enable --dolt to fail without dolt identity (got rc=0)"
echo "$OUT" | grep -q 'dolt config --global --add user' \
    || fail "missing actionable hint in --dolt error:\n$OUT"
[ ! -d "$WORK/sample/.beads" ] \
    || fail "enable --dolt left a half-initialized .beads/ behind"
pass "rsry enable --dolt refuses without dolt identity, no half-init"

# --- Phase 2: configure identity, `enable --dolt` must now succeed ---

dolt config --global --add user.email "e2e@example.com" >/dev/null
dolt config --global --add user.name  "e2e"             >/dev/null

"$RSRY" enable --dolt "$WORK/sample" >/dev/null 2>&1 \
    || fail "rsry enable --dolt failed after dolt identity was set"
[ -f "$WORK/sample/.beads/metadata.json" ] \
    || fail "enable --dolt did not produce .beads/metadata.json"
[ -d "$WORK/sample/.beads/dolt" ] \
    || fail "enable --dolt did not create a Dolt store"
pass "rsry enable --dolt end-to-end after dolt identity is configured"

# --- Phase 3: bead create + list + status, capture stderr for noise check ---

# Bare create (no explicit close condition) must succeed — it gets the honest
# PR-merge default rather than being rejected (the quickstart path).
CREATE=$("$RSRY" bead -r "$WORK/sample" create "fresh-setup smoke" \
            --priority 2 --files README.md 2>&1)
echo "$CREATE" | grep -q 'created' \
    || fail "bare bead create should default a close condition, not fail:\n$CREATE"
pass "rsry bead create (bare — defaults close condition)"

LIST=$("$RSRY" bead -r "$WORK/sample" list 2>&1)
echo "$LIST" | grep -q 'fresh-setup smoke' \
    || fail "created bead missing from list:\n$LIST"
pass "rsry bead list"

STATUS=$("$RSRY" status 2>&1)
echo "$STATUS" | grep -q '1 bead' \
    || fail "rsry status did not report the new bead:\n$STATUS"
pass "rsry status reports the new bead"

# Recommended path: declare an explicit close condition up front (run after the
# '1 bead' status assertion above so the count stays deterministic).
CREATE2=$("$RSRY" bead -r "$WORK/sample" create "fresh-setup explicit" \
            --priority 2 --files README.md \
            --acceptance "cargo test fresh_setup" 2>&1)
echo "$CREATE2" | grep -q 'created' \
    || fail "bead create --acceptance output unexpected:\n$CREATE2"
pass "rsry bead create --acceptance (explicit close condition)"

# --- Phase 4: regression check — the dolt 2.x duplicate-column noise is gone ---

# Combine output of two more invocations and assert the specific failing
# migration string never appears. Use both list and status because the
# pre-fix bug surfaced on every read path.
NOISE=$( ("$RSRY" bead -r "$WORK/sample" list; "$RSRY" status) 2>&1 || true )
if echo "$NOISE" | grep -q 'migration 001_add_user_id failed'; then
    fail "migration 001 still warns on every invocation:\n$NOISE"
fi
pass "no '[migrate] migration 001_add_user_id failed' noise on read paths"

# --- Phase 5: orchestrator-backend init path also preflights identity ---

# Independent of the per-repo `.beads/` init: commands like `rsry backup`,
# `rsry sync`, `rsry run`, and `rsry serve` go through
# config::BackendConfig::connect → store_dolt::DoltBackend::connect, which
# is a *second* dolt-init call site. Before the shared dolt_init_dir
# helper, this site swallowed dolt's stderr and left a half-init backend
# directory behind on failure — the same anti-pattern init_beads_db used
# to have. Exercise it by configuring a `[backend]` section, nuking the
# data dir, unsetting identity, then asserting backup fails with the
# same actionable hint and leaves no half-init.

BACKEND_DIR="$HOME/.rsry/dolt/rosary"
cat >> "$HOME/.rsry/config.toml" <<'EOF'

[backend]
provider = "dolt"
path = "~/.rsry/dolt/rosary"
EOF
rm -rf "$BACKEND_DIR"
dolt config --global --unset user.email >/dev/null 2>&1 || true
dolt config --global --unset user.name  >/dev/null 2>&1 || true

set +e
BACKUP_OUT=$("$RSRY" backup --output "$WORK/backup.json" 2>&1)
BACKUP_RC=$?
set -e

[ "$BACKUP_RC" -ne 0 ] \
    || fail "expected backup to fail without dolt identity (got rc=0)"
echo "$BACKUP_OUT" | grep -q 'dolt config --global --add user' \
    || fail "orchestrator backend init missed actionable hint:\n$BACKUP_OUT"
[ ! -d "$BACKEND_DIR" ] \
    || fail "orchestrator init left a half-initialized $BACKEND_DIR behind"
pass "orchestrator backend init refuses without dolt identity, no half-init"

# Restore identity for any future phases / interactive debugging.
dolt config --global --add user.email "e2e@example.com" >/dev/null
dolt config --global --add user.name  "e2e"             >/dev/null

echo ""
echo "e2e fresh-setup: all assertions passed"
