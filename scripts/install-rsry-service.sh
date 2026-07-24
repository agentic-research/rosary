#!/usr/bin/env bash
# Install the canonical local rsry HTTP MCP launch agent.
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
plist="$HOME/Library/LaunchAgents/com.rosary.serve.plist"
legacy_plist="$HOME/Library/LaunchAgents/dev.rsry.serve.plist"
legacy_tunnel_plist="$HOME/Library/LaunchAgents/dev.rsry.tunnel.plist"
label="com.rosary.serve"
bin="$HOME/.local/bin/rsry"
log="$HOME/.rsry/http.log"
staged="${TMPDIR:-/tmp}/com.rosary.serve.plist.staged.$$"
service_user="${USER:-$(id -un)}"

trap 'rm -f "$staged"' EXIT
mkdir -p "$HOME/Library/LaunchAgents" "$HOME/.rsry"

if [ -f "$legacy_plist" ]; then
  launchctl unload "$legacy_plist" 2>/dev/null || true
  rm -f "$legacy_plist"
  echo "Removed obsolete dev.rsry.serve launch agent"
fi

if [ -f "$legacy_tunnel_plist" ]; then
  launchctl unload "$legacy_tunnel_plist" 2>/dev/null || true
  rm -f "$legacy_tunnel_plist"
  echo "Removed obsolete dev.rsry.tunnel launch agent"
fi

sed \
  -e "s|__RSRY_BIN__|$bin|g" \
  -e "s|__RSRY_LOG__|$log|g" \
  -e "s|__HOME__|$HOME|g" \
  -e "s|__USER__|$service_user|g" \
  "$here/scripts/com.rosary.serve.plist.template" > "$staged"

# Only reload launchd if the plist content has actually changed (or does not
# exist yet). A changed binary with an unchanged plist is restarted below.
if [ ! -f "$plist" ] || ! diff -q "$staged" "$plist" >/dev/null 2>&1; then
  cp -f "$staged" "$plist"
  launchctl unload "$plist" 2>/dev/null || true
  launchctl load "$plist"
  echo "HTTP MCP service (re)loaded (plist changed)"
elif ! launchctl list "$label" >/dev/null 2>&1; then
  # The plist is unchanged but the job is unloaded.
  launchctl load "$plist"
  echo "HTTP MCP service plist unchanged but was unloaded — loaded"
else
  # Restart deterministically onto the installed binary. WatchPaths was
  # unreliable after atomic binary replacement (rosary-de1237).
  launchctl kickstart -k "gui/$(id -u)/$label"
  echo "HTTP MCP service kickstarted onto the new binary"
fi

echo "  Port: 8383 | Log: $log"
