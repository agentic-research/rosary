#!/usr/bin/env bash
# Golden Rule 2: Keep files under 200 lines.
# Warns at 200. Does NOT fail — existing large files need refactoring,
# but blocking commits on them would halt all work. The janitor agent
# creates beads for files that need splitting.

WARN_LIMIT=200

for file in "$@"; do
    # Skip non-text, generated, and lock files
    case "$file" in
        *.lock|*.sum|*.min.*|*/vendor/*|*/target/*) continue ;;
    esac

    if [ ! -f "$file" ]; then
        continue
    fi

    lines=$(wc -l < "$file")

    if [ "$lines" -gt "$WARN_LIMIT" ]; then
        echo "WARNING: Golden Rule 2 — $file is $lines lines (guideline: $WARN_LIMIT)"
    fi
done
# Always pass — this is advisory, not blocking.
# Enforcement is via janitor agent creating refactor beads.
exit 0
