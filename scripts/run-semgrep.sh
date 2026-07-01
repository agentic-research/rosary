#!/usr/bin/env bash
# Run Semgrep for `task lint`, degrading cleanly when Semgrep is present but
# UNUSABLE in this environment (rosary-9e5138).
#
# Semgrep can exit before scanning with:
#   "Failed to create system store X509 authenticator: ca-certs: empty trust anchors"
# even with metrics/version-check disabled. That is harness friction (missing
# system trust anchors in a sandbox), NOT a code finding — so it must not fail
# `task lint`. We still fail on REAL findings and on other genuine errors.
#
# Exit contract:
#   0  → no findings, OR semgrep unusable (trust-init failure → skip+warn)
#   N  → real findings / other semgrep error (propagated)
set -uo pipefail

CONFIG="${1:-.semgrep/rules.yml}"
TARGET="${2:-.}"

if ! command -v semgrep >/dev/null 2>&1; then
  echo "SKIP: semgrep not installed (brew install semgrep)"
  exit 0
fi

# Minimize the network/trust surface that triggers the X509 init.
out=$(SEMGREP_SEND_METRICS=off SEMGREP_ENABLE_VERSION_CHECK=0 \
  semgrep --config "$CONFIG" --error --metrics off --disable-version-check "$TARGET" 2>&1)
code=$?

if [ "$code" -eq 0 ]; then
  printf '%s\n' "$out"
  exit 0
fi

# Signature of the environment trust-init failure — degrade, don't fail.
if printf '%s' "$out" | grep -qiE "empty trust anchors|X509 authenticator|Failed to create system store"; then
  echo "SKIP: semgrep unusable in this environment (system X509 trust init failed) — not a code finding"
  printf '%s\n' "$out" | tail -2
  exit 0
fi

# A real finding (exit 1) or a genuine error — surface it and fail.
printf '%s\n' "$out"
exit "$code"
