#!/usr/bin/env bash
# Commit contract — deterministically enforced at commit-msg time (no human, no
# CI, no Actions). The workflow is agentic: agents write every commit, so the
# format is a GATE the substrate enforces, not a convention anyone remembers.
# See docs/design/codex-rosary-determinism-friction.md (rosary-d88cfb): "if
# correctness depends on remembering a rule, encode it as a gate."
#
# Required subject shape:
#
#     [bead-id] <type>(<scope>): <subject>
#
#   - [bead-id]  Golden Rule 11 — every commit references a bead.
#   - <type>     Conventional Commits — feat|fix|docs|chore|refactor|test|perf|
#                build|ci|style|revert (optional `!` for breaking).
#   - (<scope>)  optional.
#
# Example: [rosary-d3a3dd] fix(serve): rename domain

set -uo pipefail

subject=$(head -n1 "$1")

# Escapes: merge/revert/initial commits and git's autosquash markers, which are
# generated (not authored) and don't carry the contract.
if echo "$subject" | grep -qiE '^(Merge |Revert "|Revert:|initial commit|fixup!|squash!|amend!)'; then
    exit 0
fi

types='feat|fix|docs|chore|refactor|test|perf|build|ci|style|revert'

# Full contract in one match: bead ref + conventional type(scope)!?: subject.
if echo "$subject" | grep -qE "^\[[-a-zA-Z0-9._]+\] (${types})(\([a-zA-Z0-9._/-]+\))?!?: .+"; then
    exit 0
fi

# Diagnose which half failed so the fix is obvious.
if ! echo "$subject" | grep -qE '^\[[-a-zA-Z0-9._]+\] '; then
    echo "ERROR: commit must start with a bead reference (Golden Rule 11)."
    echo "  Missing the [bead-id] prefix."
else
    echo "ERROR: commit must use Conventional Commits after the bead reference."
    echo "  Expected a <type> from: ${types}"
fi
echo "  Required: [bead-id] <type>(<scope>): <subject>"
echo "  Example:  [rosary-d3a3dd] fix(serve): rename domain"
echo "  Got:      ${subject}"
exit 1
