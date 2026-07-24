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

The launchd transaction will move from the inline Taskfile recipe to
`scripts/install-rsry-service.sh`, called by `task install-service`. The script
will enforce one canonical local service before loading or restarting it:

1. Unload the legacy `dev.rsry.serve` job if it is loaded.
2. Delete `~/Library/LaunchAgents/dev.rsry.serve.plist` if it exists.
3. Unload and delete the obsolete Homebrew-backed `dev.rsry.tunnel` job if it
   exists.
4. Install or restart `com.rosary.serve` using the existing logic.

The operation is idempotent: an absent legacy plist is a no-op, and repeated
installer runs continue to manage only `com.rosary.serve`.

Codex remains configured for the long-running local HTTP MCP endpoint. This
change does not introduce a command-backed stdio server.

## Runtime Dependencies

The canonical plist executes rsry through the absolute path
`~/.local/bin/rsry`; it does not use a Homebrew rsry installation. Its `PATH`
is restricted to `~/.local/bin:/usr/bin:/bin`, so the local service cannot
resolve tooling from either Apple Silicon or Intel Homebrew prefixes.

## Testing

`scripts/install-rsry-service.test.sh` will execute the real installer in a
temporary `HOME`, with a fake `launchctl` at the external process boundary. It
will assert observable filesystem effects and the recorded command order:

- the obsolete plist no longer exists;
- the obsolete Homebrew-backed tunnel plist no longer exists;
- the canonical plist is installed and points at the temporary
  `~/.local/bin/rsry`;
- the canonical plist does not expose Homebrew paths to the service;
- the legacy unload occurs before the canonical load;
- a second run succeeds with the legacy plist already absent.

The focused installer test must fail before the Taskfile change and pass
afterward. It will be wired into the canonical `task rules`/`task check` gate.
The full `task check` gate must pass before the branch is pushed.

## Scope

Files in scope:

- `Taskfile.yml`
- `scripts/install-rsry-service.sh`
- `scripts/install-rsry-service.test.sh`
- `scripts/check-install-restart.sh`
- `docs/GETTING_STARTED.md`

Global Codex hooks and plugin configuration are intentionally outside this PR.
