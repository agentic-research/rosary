#!/usr/bin/env bash
# Golden Rule 11: every commit must reference a bead.
# Format: [bead-id] type(scope): description
# Example: [rosary-d3a3dd] fix(serve): rename domain

msg=$(cat "$1")

# Allow merge commits, initial commits
if echo "$msg" | grep -qiE "^Merge |^initial commit"; then
    exit 0
fi

# Require [bead-id] prefix
if echo "$msg" | grep -qE '^\[[-a-zA-Z0-9]+\] '; then
    exit 0
fi

# Also allow bead: in body (backward compat during transition)
if echo "$msg" | grep -qiE "bead:"; then
    exit 0
fi

echo "ERROR: commit message must start with [bead-id] (Golden Rule 11)"
echo "  Format: [rosary-abc123] type(scope): description"
echo "  Example: [rosary-d3a3dd] fix(serve): rename domain"
echo "  Got: $(echo "$msg" | head -1)"
exit 1
