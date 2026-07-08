#!/usr/bin/env bash
# Rule QA for the semgrep gate: `semgrep --validate` (well-formed rules) +
# `semgrep --test` (the .semgrep/rules.rs fixtures' ruleid:/ok: annotations hold).
# Complements scripts/run-semgrep.sh (which runs the scan) — this checks the
# RULES themselves so they can't silently rot (rosary-4b18f0).
#
# Degrades cleanly, same as run-semgrep.sh: a sandbox X509 trust-init failure is
# harness friction, not a rule defect, so it skips+warns instead of failing.
# Real validation/test failures propagate.
set -uo pipefail

CONFIG="${1:-.semgrep/rules.yml}"
FIXTURES="${2:-.semgrep/rules.rs}"

if ! command -v semgrep >/dev/null 2>&1; then
  echo "SKIP: semgrep not installed (brew install semgrep)"
  exit 0
fi

unusable() {
  printf '%s' "$1" | grep -qiE "empty trust anchors|X509 authenticator|Failed to create system store"
}

# 1. Validate rule syntax/structure.
out=$(SEMGREP_SEND_METRICS=off SEMGREP_ENABLE_VERSION_CHECK=0 \
  semgrep --validate --config "$CONFIG" --metrics off --disable-version-check 2>&1)
if [ $? -ne 0 ]; then
  if unusable "$out"; then
    echo "SKIP: semgrep unusable in this environment (X509 trust init) — not a rule defect"
    exit 0
  fi
  printf '%s\n' "$out"
  echo "semgrep --validate failed"
  exit 1
fi
echo "semgrep --validate: rules well-formed"

# 2. Run the fixture annotations (ruleid: must match, ok: must not).
out=$(SEMGREP_SEND_METRICS=off SEMGREP_ENABLE_VERSION_CHECK=0 \
  semgrep --test --config "$CONFIG" "$FIXTURES" 2>&1)
code=$?
if [ "$code" -ne 0 ]; then
  if unusable "$out"; then
    echo "SKIP: semgrep unusable in this environment (X509 trust init) — not a rule defect"
    exit 0
  fi
  printf '%s\n' "$out"
  echo "semgrep --test failed — a rule no longer matches its fixtures"
  exit 1
fi
printf '%s\n' "$out" | tail -2
