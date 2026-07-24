# Single rsry Launch Agent Design

## Problem

Codex connects to the local rsry HTTP MCP server at
`http://localhost:8383/mcp`. Two launch-agent definitions can currently exist
for that one endpoint:

- `dev.rsry.serve.plist`, the original local service definition.
- `com.rosary.serve.plist`, the repository-managed replacement installed by
  `task install-service`.

Both run `~/.local/bin/rsry serve --transport http --port 8383`. If launchd
loads both, they race for the same port and one enters an `Address already in
use` restart loop. After a user or configuration migration, the canonical
service may instead remain unloaded, leaving Codex without rsry tools for the
entire session.

## Design

`task install-service` will enforce one canonical local service before loading
or restarting it:

1. Unload the legacy `dev.rsry.serve` job if it is loaded.
2. Delete `~/Library/LaunchAgents/dev.rsry.serve.plist` if it exists.
3. Install or restart `com.rosary.serve` using the existing logic.

The operation is idempotent: an absent legacy plist is a no-op, and repeated
installer runs continue to manage only `com.rosary.serve`.

Codex remains configured for the long-running local HTTP MCP endpoint. This
change does not introduce a command-backed stdio server.

## Runtime Dependencies

The canonical plist executes rsry through the absolute path
`~/.local/bin/rsry`; it does not use a Homebrew rsry installation. Its `PATH`
retains `/opt/homebrew/bin` so runtime dependencies installed there, notably
`dolt`, remain discoverable.

## Testing

The existing installer contract test will gain assertions that:

- the legacy label and plist path are named explicitly;
- the legacy service is unloaded non-interactively;
- the obsolete plist is removed with `rm -f`;
- legacy cleanup occurs before canonical service loading/restarting;
- the canonical plist still points at `~/.local/bin/rsry`;
- the documented HTTP transport remains unchanged.

The focused installer test must fail before the Taskfile change and pass
afterward. The full `task check` gate must pass before the branch is pushed.

## Scope

Files in scope:

- `Taskfile.yml`
- `scripts/check-install-restart.sh`
- `docs/GETTING_STARTED.md`

Global Codex hooks and plugin configuration are intentionally outside this PR.
