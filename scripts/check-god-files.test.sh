#!/usr/bin/env bash
# Test the god-file gate's ratchet logic (native path — no mache needed).
set -uo pipefail
HERE="$(cd "$(dirname "$0")/.." && pwd)"
SCRIPT="$HERE/scripts/check-god-files.sh"
fails=0

# Force the native (wc -l) path by hiding mache; run against the real repo.
run() { env PATH="/usr/bin:/bin" GODFILE_THRESHOLD="$1" bash "$SCRIPT" 2>&1; }

# 1) At the committed threshold (1500), the baseline covers every over-limit
#    file → gate passes.
out="$(run 1500)"; code=$?
if [ "$code" -eq 0 ]; then echo "ok   [pass-at-baseline]"; else
  echo "FAIL [pass-at-baseline]: exit $code"; echo "$out"; fails=$((fails+1)); fi

# 2) At a low threshold, non-baseline files cross it → gate fails loud.
out="$(run 1000)"; code=$?
if [ "$code" -ne 0 ] && printf '%s' "$out" | grep -q "new god-file"; then
  echo "ok   [fail-on-new]"; else
  echo "FAIL [fail-on-new]: exit $code"; echo "$out"; fails=$((fails+1)); fi

if [ "$fails" -eq 0 ]; then echo "PASS: god-file gate ratchet"; exit 0; fi
echo "FAILED: $fails case(s)"; exit 1
