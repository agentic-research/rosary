#!/usr/bin/env bash
# Golden Rule 1: No versioned files.
# Rejects files with version numbers, _final, _old, _backup in their names.

failed=0
for file in "$@"; do
    basename=$(basename "$file")
    if echo "$basename" | grep -qE '_v[0-9]+\.|_final\.|_old\.|_backup\.|_copy\.'; then
        echo "ERROR: Golden Rule 1 violation — versioned filename: $file"
        echo "  Use configuration to manage variants. Git provides version control."
        failed=1
    fi
done
exit $failed
