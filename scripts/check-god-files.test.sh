#!/usr/bin/env bash
# Test the dynamic god-file gate (origin/main as the baseline — no committed list).
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SCRIPT="$ROOT/scripts/check-god-files.sh"
cd "$ROOT"
fails=0

# 1) Clean tree: every over-threshold file is already on the base branch →
#    grandfathered → gate passes.
if out="$(bash "$SCRIPT" 2>&1)"; then echo "ok   [clean-tree-passes]"; else
  echo "FAIL [clean-tree-passes]: $out"; fails=$((fails+1)); fi

# 2) A brand-new file over the threshold (absent on base → base lines 0) must
#    fail loud. Stage it so `git ls-files` sees it, then clean up.
big="src/zzz_god_file_test.rs"
python3 - "$big" <<'PY'
import sys
open(sys.argv[1],"w").write("// generated test line\n"*1600)
PY
git add -f "$big" >/dev/null 2>&1
out="$(bash "$SCRIPT" 2>&1)"; code=$?
git rm -f --quiet "$big" >/dev/null 2>&1 || true
rm -f "$big"
if [ "$code" -ne 0 ] && printf '%s' "$out" | grep -q "crossed"; then
  echo "ok   [new-godfile-fails]"; else
  echo "FAIL [new-godfile-fails]: exit $code"; echo "$out"; fails=$((fails+1)); fi

if [ "$fails" -eq 0 ]; then echo "PASS: dynamic god-file gate"; exit 0; fi
echo "FAILED: $fails case(s)"; exit 1
