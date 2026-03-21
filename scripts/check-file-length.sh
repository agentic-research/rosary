#!/usr/bin/env bash
# Golden Rule 2: Keep files under 200 lines.
# Warns at 200. Fails at 500 — files beyond this hard limit must be
# refactored before committing. The janitor agent creates beads for
# files in the warning range that need splitting.

WARN_LIMIT=200
FAIL_LIMIT=500

failed=0

for file in "$@"; do
    # Skip non-text, generated, and lock files
    case "$file" in
        *.lock|*.sum|*.min.*|*/vendor/*|*/target/*) continue ;;
    esac

    if [ ! -f "$file" ]; then
        continue
    fi

    lines=$(wc -l < "$file")

    if [ "$lines" -gt "$FAIL_LIMIT" ]; then
        echo "FAIL: Golden Rule 2 — $file is $lines lines (hard limit: $FAIL_LIMIT)"
        failed=1
    elif [ "$lines" -gt "$WARN_LIMIT" ]; then
        echo "WARNING: Golden Rule 2 — $file is $lines lines (guideline: $WARN_LIMIT)"
    fi
done

exit $failed
