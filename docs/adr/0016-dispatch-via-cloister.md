# ADR-0016: Dispatch agents as cloister bundles, not host subprocesses

**Status:** Proposed
**Date:** 2026-07-13
**Relates to:** [ADR-0013](0013-bead-substrate.md) / cloister slice-grants (capability model), [ADR-0015](0015-execution-lineage-capsules.md) (execution-lineage capsules — durable context), [ADR-0012](0012-personal-root-bead-substrate.md) (root substrate), `project_rsry_in_cloister_bundle` (rosary already runs as a cloister bundle with vault-proxied creds). Supersedes the band-aid in `rosary-5251a0` (`--disallowedTools`). Subsumes `rosary-b1495c` (dispatch "Not logged in"), `rosary-82caac` (dead-dispatch corpses).

## Context

rosary is, everywhere except one place, a principled capability system: it runs as a **cloister external bundle**, holds **no plaintext credentials** (the vault proxies outbound creds), and is moving its execution state onto **capsules** (ADR-0015) with a jj-backed durable history. Then it dispatches an agent — and throws all of that away by shelling out to a **raw `claude -p` subprocess** on the host, with the operator's own Keychain OAuth, no capability boundary, no isolation, no lifecycle handle, and no durable context.

Every dispatch failure we have hit traces to that one un-cloistered hole. Verified empirically this session:

| Symptom | Root cause (raw host subprocess) | Substrate that should own it |
| --- | --- | --- |
| reign not enforced — a ReadOnly agent ran `find /` (5251a0) | `--allowedTools` is *advisory* to a CLI; there is no authoritative deny | cloister **slice-grant** (manifest capability, hypervisor-enforced) |
| no filesystem sandbox | nothing confines a host subprocess | cloister **bundle isolate** (fs/service = exactly the granted slices) |
| "Not logged in" dispatch death (b1495c) | detached process is *outside* the vault-proxied context, so it has no reachable cred | **vault-proxied scoped token** (agent never holds the Keychain) |
| corpses — 14 "active" rows, 0 live workers; count only grows (82caac) | a detached subprocess has **no lifecycle handle**, so death is invisible | cloister **supervisor** (death is an event, not a `ps`-and-guess) |
| context dies with the worktree | worktree is both execution surface *and* durable store | **capsule** on the jj op-log (ADR-0015) |
| **can't unit-test any of it** | behaviour lives in an external binary — this ADR's author had to `printf` file-markers against the live `claude` CLI to prove reign, because there was nothing to assert against | a **programmatic agent loop** with a permission callback |

The band-aids we shipped (`--disallowedTools` for read-only profiles, 5251a0) close the *reported* incident but cannot bind the Implement profile's scoped Bash and do not touch creds, corpses, or context. This ADR names the real fix.

## Decision

### D1 — The dispatch primitive is a cloister bundle, not a host process

rosary dispatches an agent by asking cloister to spawn an **agent-execution bundle**, not by `Command::new("claude")` on the host. The bundle's manifest is the single place that declares the agent's *reign* (tools it may call), its *sandbox* (fs/service slices), and its *credential binding* (a vault handle, not a secret). The host process model — one object wearing "runtime," "sandbox," "credential holder," and "lifecycle" hats at once, none of them real — is retired.

### D2 — Reign is a slice-grant capability, hypervisor-enforced (supersedes `--allowedTools`)

The tool allowlist becomes a **slice-grant** in the bundle manifest (ADR-0013). The hypervisor enforces it; the agent cannot call a tool it was not granted, the way a workerd isolate cannot reach a binding it was not given. This is categorically different from `--allowedTools`, which we proved is an *auto-approve* list an out-of-band tool call simply ignores. `find /` is not "denied" — the syscall surface to reach `/` is **not bound into the bundle**.

### D3 — Sandbox is the bundle isolate, not an OS bolt-on

We do **not** wrap the subprocess in `sandbox-exec`/seccomp. The agent runs *inside* a cloister isolate whose filesystem is the mounted git worktree (D7) and whose outbound edges are its granted service-bindings. Unreachable beats denied.

### D4 — Credentials are vault-proxied scoped tokens; the agent never holds the Keychain

The agent authenticates with a **short-lived, scoped credential vended by the vault** through a service-binding — the same posture rosary itself already runs under (`project_rsry_in_cloister_bundle`). The operator's Keychain OAuth never enters the agent's environment. This is the *correct* fix for b1495c: the detached subprocess fails auth because it *should not have had* the ambient credential in the first place.

### D5 — Auth model is preserved; **cost is the binding constraint** (the Max-account tell)

`claude -p` was almost certainly chosen not because it is good design but because it uses the operator's **Claude Max subscription** (flat-rate OAuth via the installed CLI) — no `ANTHROPIC_API_KEY`, no per-token API billing. Any "just use the Agent SDK" answer that silently requires an API key is a **cost regression**, not an upgrade.

So the decision is explicit: **cloister the exec, do not abandon the subscription.** The agent inside the bundle still runs under Max OAuth — either `claude -p` or the Agent SDK driven by a `claude setup-token` OAuth token (the token shape rosary already routes, `rosary-1be3b8`). Isolation, capability, and credential-scoping are **orthogonal to how the agent bills**. We keep flat-rate auth *and* gain the bundle boundary. Requiring an API key is a rejected alternative (below).

### D6 — Lifecycle is a supervisor handle; reaping is an event (subsumes 82caac)

A bundle has a real lifecycle the supervisor observes. Death emits an event → the pipeline row is cleared and the capsule sealed *at death*, not "eventually, if a reconcile loop happens to be running." The corpse-accumulation of 82caac is structurally impossible when the runtime object is supervised rather than detached-and-forgotten. (82caac's terminal-filter + reap-on-read fix remains the correct stopgap for the *current* model until this lands.)

### D7 — jj + git + cloister compose into durable-yet-disposable execution

The three substrates already in the stack compose exactly into what durable dispatch needs:

```
git worktree   → disposable execution surface (mounted into the bundle; nuke-and-rebuild)
cloister       → the isolation + capability + credential boundary around the agent
jj (op-log)    → the orchestrator's durable, undoable operation history — the capsule's home
capsule        → the typed, hash-linked lineage (ADR-0015) recorded onto that history
```

Losing the worktree loses nothing: reign lives in the manifest, creds in the vault, context in the capsule on jj's op-log. Resume = re-spawn the bundle, re-mount a fresh worktree at the capsule's `workspace_base_commit`, rehydrate context from the capsule. This is ADR-0015's D4 with cloister as the execution boundary and jj as the durable substrate.

### D8 — In-bundle tool enforcement (SDK `canUseTool`) is an implementation detail — and the testability seam

*Inside* the bundle, how the agent loop enforces its granted slices is an implementation choice: the Claude Agent SDK's `canUseTool` callback is the natural fit (deny anything outside the manifest's grant), and — critically — it makes dispatch **unit-testable**: mock the loop, assert the grant decision, no live binary, no real creds. This is a second, softer boundary under cloister's hard one (defense in depth), not a replacement for it. It is also what turns "we test spawning by running the real thing" into real tests.

### D9 — A structural smell rule forbids un-sanctioned exec

Re-introducing a raw host spawn is the regression this ADR exists to prevent. Add a committed smell rule (mache `docs/smell-rules/` or semgrep) that flags `std::process::Command` / `tokio::process::Command` **outside the one sanctioned dispatch seam** (`src/dispatch/providers.rs` cloister path). Exec-out becomes a gate failure, not a habit.

## Architecture (sketch)

```
reconciler.dispatch(bead)
  └─ cloister.spawn_bundle(manifest {
        slices:   tool grants from PermissionProfile  (D2 — reign)
        mounts:   git worktree @ workspace_base_commit (D7 — surface)
        bindings: vault:agent-cred(scope=bead)         (D4 — creds)
     })
       └─ agent loop (claude -p | SDK, Max OAuth)      (D5 — cost preserved)
            ├─ tool call → canUseTool ∈ slices?         (D8 — in-bundle gate)
            ├─ emits capsule_events → jj op-log         (D7 — durable lineage, ADR-0015)
            └─ exit/death → supervisor event            (D6 — reap + seal, no corpse)
```

## Migration

`claude -p`-on-host stays the default until the cloister path is proven. Add a `cloister` variant to the `AgentProvider` axis (alongside `claude`/`codex`/`gemini`/`acp`); dispatch routes through it when configured. The 5251a0 `--disallowedTools` denylist remains the fallback for any non-cloister dispatch. No flag-day.

## Phasing

| Phase | Scope |
| --- | --- |
| **V0** | D9 smell rule — cheap guard, lands first, stops the bleeding from spreading. |
| **V1** | Cloister-bundle dispatch for **claude** only: slice-grant reign (D2) + isolate (D3) + vault cred (D4) + supervisor handle (D6), under Max OAuth (D5). Proves the boundary. |
| **V1.5** | Capsule lineage on jj (D7) — ties ADR-0015 V1; resume-from-capsule via re-spawn. |
| **V2** | codex via its app-server (`codex_native.rs` already speaks it) hosted in-bundle; unify the provider seam. In-bundle `canUseTool` (D8) for SDK-driven loops. |

## Consequences

- reign, sandbox, creds, corpses, and context stop being five separate bugs and become **properties of the bundle + capsule** — one designed seam.
- Dispatch becomes unit-testable (D8) instead of requiring a live binary + real credentials.
- The Max-subscription cost model is preserved (D5) — no API-key regression.
- **New dependency:** cloister must be able to host an *agent-execution* bundle (see Open questions). If it cannot yet, V1 is a cloister feature first.
- One more integration surface (rosary↔cloister spawn protocol) and one more manifest schema to maintain.

## Alternatives considered

- **Agent SDK with `canUseTool`, no cloister.** Binds reign in-process and is testable, but an in-process callback is not a security boundary (a compromised loop bypasses it), gives no fs isolation or credential-scoping, and — if it forces `ANTHROPIC_API_KEY` — is a cost regression (D5). Kept only as the *in-bundle* enforcement detail (D8), under cloister.
- **OS sandbox (`sandbox-exec`/seccomp) around the subprocess.** Adds fs confinement but nothing else — no capability model, no vault creds, no lifecycle, no durable context. A bolt-on where a substrate exists.
- **Status quo + more `--disallowedTools` denylists.** Whack-a-mole; provably cannot bind Implement's scoped Bash (a broad `Bash` deny kills `cargo`); touches none of creds/corpses/context.
- **Require an API key and drop the subscription.** Rejected: a direct, ongoing cost regression for the operator (D5).

## What we are explicitly **not** claiming

- Not claiming cloister hosts agent-execution bundles *today* — that may be net-new (Open questions).
- Not replacing the Max subscription with per-token API billing.
- Not signing capsule lineage in V1 (that is ADR-0015 V2).
- Not that the 5251a0/82caac stopgaps were wrong — they are the right patches for the *current* model until this lands.

## Open questions

1. **Does cloister host agent-execution bundles today, or is that net-new?** If net-new, V1 begins as a cloister capability (a bundle kind that runs a long-lived agent loop with a git-worktree mount + vault binding), and rosary integrates against it.
2. **Worktree mount into an isolate** — does the bundle get a real filesystem mount of the git worktree, or does the agent's file I/O go through a service-binding (VFS)? The former is simpler; the latter is a truer capability boundary.
3. **jj as the capsule home** — does the capsule live *in* the jj op-log (operation = capsule event) or alongside it (backend store referencing jj change-ids)? ADR-0015 D3 currently says the orchestrator store; this ADR proposes jj as the durable substrate — reconcile the two.
