#!/usr/bin/env bash
set -euo pipefail

ROOT="${1:-.}"
TASKFILE="$ROOT/Taskfile.yml"
WORKFLOW="$ROOT/.github/workflows/ci.yml"

task_block() {
  local task="$1"
  awk -v task="$task" '
    $0 ~ "^  " task ":" { in_task=1; print; next }
    in_task && /^  [a-zA-Z0-9:_-]+:/ { exit }
    in_task { print }
  ' "$TASKFILE"
}

require_contains() {
  local haystack="$1"
  local needle="$2"
  local label="$3"
  if ! grep -Fq -- "$needle" <<<"$haystack"; then
    echo "FAIL: $label must contain: $needle" >&2
    exit 1
  fi
}

reject_contains() {
  local haystack="$1"
  local needle="$2"
  local label="$3"
  if grep -Fq -- "$needle" <<<"$haystack"; then
    echo "FAIL: $label must not contain: $needle" >&2
    exit 1
  fi
}

check_block="$(task_block check)"
ci_block="$(task_block ci)"

require_contains "$check_block" "task: taskfile-contract" "task check"
require_contains "$check_block" "task: compile" "task check"
require_contains "$check_block" "task: lint" "task check"
require_contains "$check_block" "task: test" "task check"

require_contains "$ci_block" "task: check" "task ci"
reject_contains "$ci_block" "cargo " "task ci"
reject_contains "$ci_block" "run-semgrep" "task ci"

require_contains "$(cat "$WORKFLOW")" "run: task check" "GitHub Actions CI"
if grep -Eq "run: task (test|lint|ci|all)$" "$WORKFLOW"; then
  echo "FAIL: GitHub Actions CI must invoke the canonical task check gate only" >&2
  exit 1
fi

echo "Taskfile verification contract OK"
