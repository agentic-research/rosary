# Single rsry Launch Agent Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `task install-service` remove the obsolete rsry launch agent so exactly one local HTTP MCP service can bind port 8383.

**Architecture:** Extend the existing macOS installer transaction with a legacy-service cleanup phase before rendering and loading the canonical plist. Keep the long-running HTTP transport and absolute `~/.local/bin/rsry` executable path unchanged.

**Tech Stack:** Taskfile shell, Bash contract tests, launchd plist, Markdown documentation.

## Global Constraints

- `com.rosary.serve` is the sole canonical launchd label.
- `dev.rsry.serve.plist` is unloaded and deleted when found.
- Cleanup is idempotent and non-interactive.
- Codex remains URL-backed at `http://localhost:8383/mcp`; no stdio migration.
- `/opt/homebrew/bin` remains in the service `PATH` for dependencies such as `dolt`, not for rsry itself.

---

### Task 1: Enforce the Single-Service Installer Contract

**Files:**
- Create: `scripts/install-rsry-service.sh`
- Create: `scripts/install-rsry-service.test.sh`
- Modify: `scripts/check-install-restart.sh`
- Modify: `Taskfile.yml`
- Modify: `docs/GETTING_STARTED.md`

**Interfaces:**
- Consumes: `HOME`, `PATH`, and `scripts/com.rosary.serve.plist.template`
- Produces: `scripts/install-rsry-service.sh`, an idempotent launchd transaction
  called by the `install-service` Taskfile target

- [x] **Step 1: Write the failing behavioral test**

Create `scripts/install-rsry-service.test.sh`. The test must:

```bash
test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT
test_home="$test_root/home"
fake_bin="$test_root/bin"
mkdir -p "$test_home/Library/LaunchAgents" "$fake_bin"
touch "$test_home/Library/LaunchAgents/dev.rsry.serve.plist"
```

Install a fake `launchctl` in `fake_bin` that appends its arguments to
`$LAUNCHCTL_LOG` and returns nonzero for `list`, forcing the load path. Run
`scripts/install-rsry-service.sh` with the temporary `HOME`, fake-first `PATH`,
and `USER=tester`. Assert:

```bash
test ! -e "$test_home/Library/LaunchAgents/dev.rsry.serve.plist"
test -e "$test_home/Library/LaunchAgents/com.rosary.serve.plist"
grep -q "$test_home/.local/bin/rsry" \
  "$test_home/Library/LaunchAgents/com.rosary.serve.plist"
```

Check the fake log positions to prove the legacy unload precedes the canonical
load. Run the installer a second time to prove absent-legacy idempotence.

- [x] **Step 2: Run the focused test and verify RED**

Run:

```bash
bash scripts/install-rsry-service.test.sh
```

Expected: FAIL because `scripts/install-rsry-service.sh` does not exist.

- [x] **Step 3: Implement the minimal installer migration**

Create `scripts/install-rsry-service.sh` by moving the existing inline
`install-service` transaction out of `Taskfile.yml`. Add this before the
canonical render/load logic:

```bash
LEGACY_PLIST="$HOME/Library/LaunchAgents/dev.rsry.serve.plist"
if [ -f "$LEGACY_PLIST" ]; then
  launchctl unload "$LEGACY_PLIST" 2>/dev/null || true
  rm -f "$LEGACY_PLIST"
  echo "Removed obsolete dev.rsry.serve launch agent"
fi
```

Replace the Taskfile recipe body with:

```yaml
cmds:
  - bash scripts/install-rsry-service.sh
```

Add an `install-service-contract` target that runs both installer scripts and
call it from `task rules` so CI executes the behavioral regression:

```yaml
install-service-contract:
  cmds:
    - bash scripts/check-install-restart.sh
    - bash scripts/install-rsry-service.test.sh
```

- [x] **Step 4: Document the canonical service and Homebrew boundary**

Update `docs/GETTING_STARTED.md` to state that installation removes the obsolete
`dev.rsry.serve` definition, `com.rosary.serve` is the only supported label,
rsry runs from `~/.local/bin/rsry`, and Homebrew remains on `PATH` only for
dependencies such as `dolt`.

- [x] **Step 5: Run focused verification and verify GREEN**

Run:

```bash
bash scripts/check-install-restart.sh
bash scripts/install-rsry-service.test.sh
```

Expected: all installer invariants print `ok` and the script exits zero.

- [x] **Step 6: Run the repository gate**

Run:

```bash
task check
```

Expected: contract, rules, compile, lint, tests, and smell gates all pass.

- [ ] **Step 7: Commit the implementation**

```bash
git add Taskfile.yml scripts/check-install-restart.sh \
  scripts/install-rsry-service.sh scripts/install-rsry-service.test.sh \
  docs/GETTING_STARTED.md \
  docs/superpowers/plans/2026-07-24-single-rsry-launch-agent.md .beads/beads.jsonl
git commit -m "[rosary-080934] fix(install): remove legacy rsry launch agent"
```

- [ ] **Step 8: Push and open the PR**

```bash
git pull --rebase
git push -u origin fix/rosary-080934-single-launch-agent
gh pr create --base main --head fix/rosary-080934-single-launch-agent
```

The PR description must include the RED/GREEN focused test evidence, full
`task check` result, local MCP health evidence, and the note that Codex needs a
fresh session to hydrate a URL-backed MCP server.
