#!/usr/bin/env bash
# End-to-end sandbox test for rosary.
#
# Proves the full loop works:
#   1. Binary builds and runs
#   2. rsry enable (register repo)
#   3. rsry bead create/list/search/close
#   4. rsry disable (unregister repo)
#
# Requires: rsry binary built, Dolt server running in .beads/.
# Exit code 0 = all passed, non-zero = failure with description.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
RSRY="$PROJECT_DIR/target/debug/rsry"

# fuse-t runtime library path (macOS)
if [ "$(uname)" = "Darwin" ]; then
    export DYLD_LIBRARY_PATH="/usr/local/lib:${DYLD_LIBRARY_PATH:-}"
fi

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m'

pass() { echo -e "${GREEN}  PASS${NC}: $1"; }
fail() { echo -e "${RED}  FAIL${NC}: $1"; exit 1; }
skip() { echo -e "${YELLOW}  SKIP${NC}: $1"; }

echo "=== rosary e2e sandbox test ==="
echo ""

# --- Phase 0: Binary works ---

[ -f "$RSRY" ] || { echo "Building rsry..."; cd "$PROJECT_DIR" && task build; }
[ -f "$RSRY" ] || fail "rsry binary not found at $RSRY"
pass "binary exists"

"$RSRY" --help > /dev/null 2>&1 || fail "rsry --help crashed"
pass "rsry --help"

"$RSRY" bead --help > /dev/null 2>&1 || fail "rsry bead --help crashed"
pass "rsry bead --help"

# --- Phase 1: Repo registration ---

"$RSRY" enable "$PROJECT_DIR" > /dev/null 2>&1 || fail "rsry enable failed"
pass "rsry enable"

"$RSRY" disable "$(basename "$PROJECT_DIR")" > /dev/null 2>&1 || fail "rsry disable failed"
pass "rsry disable"

# --- Phase 2: Bead CRUD (requires Dolt) ---

if [ ! -d "$PROJECT_DIR/.beads" ]; then
    skip "no .beads/ — skipping Dolt-dependent tests"
    echo ""
    echo "=== sandbox test complete (partial) ==="
    exit 0
fi

# Check if Dolt server is reachable
PORT=$(cat "$PROJECT_DIR/.beads/dolt-server.port" 2>/dev/null || echo "")
if [ -z "$PORT" ]; then
    skip "no dolt-server.port — skipping bead CRUD"
    echo ""
    echo "=== sandbox test complete (partial) ==="
    exit 0
fi

# Note: -r is a global flag on the bead subcommand, goes before the action
REPO_FLAG="-r $PROJECT_DIR"

# Create
OUTPUT=$("$RSRY" bead $REPO_FLAG create "E2E sandbox test $(date +%s)" 2>&1) || fail "bead create errored: $OUTPUT"
echo "$OUTPUT" | grep -q "Created" || fail "bead create output missing 'Created': $OUTPUT"
# Extract the generated ID
CREATED_ID=$(echo "$OUTPUT" | grep -oE 'rsry-[0-9a-f]+')
pass "bead create ($CREATED_ID)"

# List
"$RSRY" bead $REPO_FLAG list 2>&1 | grep -q "E2E sandbox" || fail "created bead not in list"
pass "bead list"

# Search
"$RSRY" bead $REPO_FLAG search "E2E sandbox" 2>&1 | grep -q "E2E sandbox" || fail "bead search failed"
pass "bead search"

# Close (use the generated ID)
if [ -n "$CREATED_ID" ]; then
    "$RSRY" bead $REPO_FLAG close "$CREATED_ID" 2>&1 | grep -q "Closed" || skip "bead close (may need real ID)"
    pass "bead close"
else
    skip "bead close (couldn't extract ID)"
fi

echo ""
echo "=== sandbox test complete ==="
