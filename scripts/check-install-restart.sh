#!/usr/bin/env bash
# Regression guard for rosary-de1237: the launchd MCP service must restart onto
# the new binary via an explicit `launchctl kickstart -k`, NOT the flaky
# WatchPaths/touch mechanism that left it running a 5-day-stale binary.
#
# launchd restart behavior can't be unit-tested in CI, so this locks the
# structural invariants of the fix instead.
set -uo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"
TASKFILE="$HERE/Taskfile.yml"
PLIST="$HERE/scripts/com.rosary.serve.plist.template"
INSTALLER="$HERE/scripts/install-rsry-service.sh"
fails=0

check() { # desc, test-expr already evaluated to 0/1 via caller
  if [ "$1" -eq 0 ]; then echo "ok   $2"; else echo "FAIL $2"; fails=$((fails+1)); fi
}

grep -q "launchctl kickstart" "$INSTALLER"; check $? "install-service kickstarts the service"
! grep -q "touch ~/.local/bin/rsry" "$TASKFILE"; check $? "install no longer relies on unreliable touch"
! grep -q "<key>WatchPaths</key>" "$PLIST"; check $? "plist template drops the WatchPaths key"

# KeepAlive must be UNCONDITIONAL. It was <dict><SuccessfulExit=false></dict>
# — "restart only on a FAILED exit" — and rsry exits 0 on SIGTERM, so a clean
# stop left the service dead with launchd considering the matter settled. It
# went down 2026-08-28 23:46 and stayed down until bead writes failed in a
# session the next day. Guarded here because install-rsry-service.sh renders
# this template over the installed plist whenever the two differ, so a
# hand-edit of ~/Library/LaunchAgents survives exactly until the next
# `task install` — the fix only holds if it lives in the template.
grep -qz "<key>KeepAlive</key>[[:space:]]*<true/>" "$PLIST"; check $? "plist template keeps the service alive unconditionally"
! grep -q "<key>SuccessfulExit</key>" "$PLIST"; check $? "plist template drops the conditional-restart policy"

# plist must still be valid (plutil is macOS-only; skip elsewhere).
if command -v plutil >/dev/null 2>&1; then
  plutil -lint "$PLIST" >/dev/null 2>&1; check $? "plist template is valid plist XML"
fi

if [ "$fails" -eq 0 ]; then echo "PASS: install-restart invariants"; exit 0; fi
echo "FAILED: $fails invariant(s)"; exit 1
