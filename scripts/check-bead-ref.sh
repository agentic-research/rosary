#!/usr/bin/env bash
# Golden Rule 11: every commit must reference a bead.
# Used as a commit-msg pre-commit hook.

msg=$(cat "$1")

# Allow merge commits, initial commits
if echo "$msg" | grep -qiE "^Merge |^initial commit"; then
    exit 0
fi

# Require bead: reference
if echo "$msg" | grep -qiE "bead:"; then
    exit 0
fi

echo "ERROR: commit message must contain bead:ID reference (Golden Rule 11)"
echo "  Add e.g. bead:rosary-abc123 or closes bead:rosary-abc123"
echo "  Got: $msg"
exit 1
