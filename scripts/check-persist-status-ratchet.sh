#!/usr/bin/env bash
# Ratchet: imperative `persist_status(...)` call sites.
#
# Each call is a place that hand-decides a bead's status string and writes it —
# the scattered state-mutation the observation fold is meant to own ONCE
# (R4b, rosary-a66b3a). This is a duplicated-logic smell: the same "pick a status
# and persist it" scattered across reconcile/verify/vcs/orchestration, each a
# spot that can drift from what the fold would derive.
#
# The count may only DECREASE. New scattered writers are rejected; the existing
# ones get paid down until a single fold-driven writer remains — which IS R4b
# step 4 (the source-of-truth flip). "State is a fold over observations, not an
# imperative mutation" — enforced, not documented in an ADR.
#
# Ratchets against origin/main (fallback HEAD), like the god-file gate.

pattern='persist_status('

# Baseline count on a committed ref (git grep sees the tree at that ref).
count_ref() {
    git grep -h "$pattern" "$1" -- 'src/**/*.rs' 2>/dev/null \
        | grep -cv 'fn persist_status' || echo 0
}

# Working-tree count from the filesystem — catches new *untracked* files a
# git-grep-based count would miss (a new scattered writer in a fresh file).
count_worktree() {
    grep -rhE --include='*.rs' 'persist_status\(' src/ 2>/dev/null \
        | grep -cv 'fn persist_status' || echo 0
}

current=$(count_worktree)

base_ref=origin/main
git rev-parse --verify "$base_ref" >/dev/null 2>&1 || base_ref=HEAD
baseline=$(count_ref "$base_ref")

if [ "$current" -gt "$baseline" ]; then
    echo "FAIL: persist_status ratchet — imperative state-write sites rose ${baseline} → ${current}."
    echo "      State transitions must DERIVE from the observation fold, not scatter a new"
    echo "      persist_status() call (R4b / rosary-a66b3a). Route the new state through the"
    echo "      fold (src/observation/), or justify lowering the baseline first."
    exit 1
fi

if [ "$current" -lt "$baseline" ]; then
    echo "persist_status ratchet: ${baseline} → ${current} (DOWN — closer to the single fold-writer)."
else
    echo "persist_status ratchet: ${current} held at baseline."
fi
exit 0
