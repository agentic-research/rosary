#!/usr/bin/env bash
# God-file gate — block NEW files from crossing the long_file threshold, while
# grandfathering the known-legacy set we're actively breaking down.
#
# Uses mache's `long_file` structural rule when `mache` is on PATH (builds a
# throwaway db in ~2s); falls back to a native `wc -l` scan otherwise, so the
# gate blocks in any CI. Compares against a committed baseline that RATCHETS
# DOWN: as a god-file is decomposed below the threshold, drop it from
# scripts/god-files-baseline.txt and it can never cross back unnoticed.
#
# Exit: 0 = ok (no new god-file), 1 = a non-baseline file crossed the threshold.
set -uo pipefail

THRESHOLD=${GODFILE_THRESHOLD:-1500}
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BASELINE="$ROOT/scripts/god-files-baseline.txt"
cd "$ROOT"

# Current set of files over THRESHOLD (paths relative to repo root, `src/...`).
current_over() {
  if command -v mache >/dev/null 2>&1; then
    local db; db="$(mktemp -u).db"
    if mache build src "$db" >/dev/null 2>&1; then
      mache find-smells --db "$db" --rule long_file --format ci --min-metric "$THRESHOLD" 2>/dev/null \
        | sed -E 's#^([^:]+):.*#src/\1#' | sort -u
      rm -f "$db"
      return
    fi
  fi
  # Native fallback: line count over tracked Rust sources (excl. generated).
  git ls-files 'src/**/*.rs' 'src/*.rs' 2>/dev/null | while read -r f; do
    [ -f "$f" ] || continue
    [ "$(wc -l < "$f")" -gt "$THRESHOLD" ] && echo "$f"
  done | sort -u
}

baseline() { [ -f "$BASELINE" ] && sort -u "$BASELINE" || true; }

new_godfiles="$(comm -23 <(current_over) <(baseline))"

if [ -n "$new_godfiles" ]; then
  echo "FAIL: new god-file(s) crossed the ${THRESHOLD}-line threshold:" >&2
  echo "$new_godfiles" | sed 's/^/  /' >&2
  echo "Split the file, or (if intentional) add it to scripts/god-files-baseline.txt." >&2
  exit 1
fi

# Advisory: baseline entries that dropped below threshold should be removed
# (ratchet down) so the gate keeps tightening.
shrunk="$(comm -13 <(current_over) <(baseline))"
if [ -n "$shrunk" ]; then
  echo "note: these baseline god-files are now under ${THRESHOLD} lines — remove them from" >&2
  echo "      scripts/god-files-baseline.txt to ratchet the gate down:" >&2
  echo "$shrunk" | sed 's/^/  /' >&2
fi
echo "god-file gate: ok (no new god-file over ${THRESHOLD} lines)"
