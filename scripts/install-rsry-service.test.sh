#!/usr/bin/env bash
# Behavioral regression for rosary-080934: only the canonical rsry HTTP MCP
# launch agent may survive installation.
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT

test_home="$test_root/home"
fake_bin="$test_root/bin"
launchctl_log="$test_root/launchctl.log"
legacy_plist="$test_home/Library/LaunchAgents/dev.rsry.serve.plist"
canonical_plist="$test_home/Library/LaunchAgents/com.rosary.serve.plist"

mkdir -p "$test_home/Library/LaunchAgents" "$fake_bin"
touch "$legacy_plist"

printf '%s\n' \
  '#!/usr/bin/env bash' \
  'printf "%s\n" "$*" >> "$LAUNCHCTL_LOG"' \
  'if [ "${1:-}" = "list" ]; then exit 1; fi' \
  'exit 0' > "$fake_bin/launchctl"
chmod +x "$fake_bin/launchctl"

run_rsry_service_installer_fixture() {
  HOME="$test_home" \
  USER="tester" \
  PATH="$fake_bin:$PATH" \
  LAUNCHCTL_LOG="$launchctl_log" \
    bash "$here/scripts/install-rsry-service.sh"
}

fail_rsry_service_installer_test() {
  echo "FAIL: $*" >&2
  exit 1
}

run_rsry_service_installer_fixture

test ! -e "$legacy_plist" ||
  fail_rsry_service_installer_test "legacy dev.rsry.serve plist survived installation"
test -e "$canonical_plist" ||
  fail_rsry_service_installer_test "canonical com.rosary.serve plist was not installed"
grep -q "$test_home/.local/bin/rsry" "$canonical_plist" ||
  fail_rsry_service_installer_test "canonical plist does not use the expected local rsry binary"

legacy_unload_line="$(
  grep -nF "unload $legacy_plist" "$launchctl_log" | head -n1 | cut -d: -f1
)"
canonical_load_line="$(
  grep -nF "load $canonical_plist" "$launchctl_log" | head -n1 | cut -d: -f1
)"
test -n "$legacy_unload_line" ||
  fail_rsry_service_installer_test "legacy service was not unloaded"
test -n "$canonical_load_line" ||
  fail_rsry_service_installer_test "canonical service was not loaded"
test "$legacy_unload_line" -lt "$canonical_load_line" ||
  fail_rsry_service_installer_test "canonical service loaded before legacy cleanup"

# A second run must succeed after the legacy plist is already absent.
run_rsry_service_installer_fixture
test ! -e "$legacy_plist" ||
  fail_rsry_service_installer_test "second installation recreated the legacy plist"

echo "PASS: installer leaves exactly one canonical rsry launch agent"
