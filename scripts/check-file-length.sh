#!/usr/bin/env bash
# Golden Rule 2: Keep files under 200 lines.
# Warns at 200. Fails at 500 — but only if the file CROSSED the limit
# in this commit (was under, now over). Files already over get a warning
# on growth but don't block. Like ruff: strict on new, lenient on legacy.

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

    # Get baseline from HEAD (0 if file is new)
    baseline=$(git show HEAD:"$file" 2>/dev/null | wc -l || echo 0)
    baseline=${baseline:-0}

    if [ "$lines" -gt "$FAIL_LIMIT" ]; then
        if [ "$baseline" -ge "$FAIL_LIMIT" ] && [ "$lines" -le "$baseline" ]; then
            # Already over limit and didn't grow — pass (pre-existing)
            :
        elif [ "$baseline" -ge "$FAIL_LIMIT" ] && [ "$lines" -gt "$baseline" ]; then
            # Already over limit and GREW — warn (legacy file, don't block)
            echo "WARNING: Golden Rule 2 — $file grew from $baseline to $lines lines (over $FAIL_LIMIT limit)"
        else
            # Crossed the limit in this commit
            echo "FAIL: Golden Rule 2 — $file is $lines lines (crossed $FAIL_LIMIT limit, was $baseline)"
            failed=1
        fi
    elif [ "$lines" -gt "$WARN_LIMIT" ]; then
        echo "WARNING: Golden Rule 2 — $file is $lines lines (guideline: $WARN_LIMIT)"
    fi
done

exit $failed
