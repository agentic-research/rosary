# ADR-0016: Route agent dispatch through cloister's harness plane (rosary-side coordination)

**Status:** Proposed
**Date:** 2026-07-13 (rewritten same day after reading cloister's authoritative design)
**Relates to (cloister is authoritative — this ADR is the rosary-side view):**
[cloister ADR-0040](../../../cloister/docs/adr/0040-harness-in-cloister.md) (harness-in-cloister: control + credential + audit plane — **L0 shipped, L1 building**), cloister ADR-0042 (`task harness:dev` — vault-proxied harness creds, **shipped**), cloister ADR-0043 (skills/agents as signed artifacts), cloister ADR-0044 (**libkrun microVM** compute sandbox — the `find /` fix), cloister ADR-0033 (**workerd has no process-spawn** — the harness runs host-side). Locally: [ADR-0015](0015-execution-lineage-capsules.md) (capsule lineage), `rosary-5251a0` / `rosary-b1495c` / `rosary-82caac` (the symptoms).

## Why this was rewritten (honesty)

The first draft of this ADR proposed running the agent "inside a cloister bundle (isolate)" with reign as a slice-grant and creds as a vault-scoped token. **Two of those claims were wrong**, and the design already exists more precisely on the cloister side:

- **You cannot run a harness inside a workerd isolate** — workerd has no `exec`/fork/host-fs (cloister ADR-0033). The harness (Claude Code / Codex) runs **host-side**. cloister is its **control + credential + audit plane, not its compute sandbox** (cloister ADR-0040).
- **The credential claim has a ceiling for the operator's subscription.** cloister L1 vaults the LLM key for **API-key / enterprise-gateway** shapes (custody). For **Max / Pro OAuth** — the shape that drove `claude -p` — the token is minted in the client's keychain, so cloister gives **audit (receipts), not custody** (cloister ADR-0040, "Scope of the credential claim"). b1495c has a real ceiling here.
- **The `find /` sandbox is a separate, in-progress substrate** — cloister ADR-0044 (libkrun microVM, host-mediated FUSE fs), not a workerd isolate.

So this ADR is not a parallel design. It is the **rosary-side coordination**: what rosary must do so that dispatch flows through cloister's already-designed (and partly shipped) harness plane, and what remains a rosary stopgap until the cloister substrate lands.

## Context

rosary spawns agents by `Command::new("claude") -p …` on the host — bypassing cloister entirely, even though rosary already runs as a cloister bundle (`[[bundles]] name = "rosary"`, wired `ROSARY_BUNDLE` over UDS; `cluster.toml`). Every dispatch failure this session (reign not bound / `find /` — 5251a0; "Not logged in" — b1495c; corpses — 82caac; context death — ADR-0015) is a consequence of the spawn skipping the plane that was built to mediate it.

cloister's ADR-0040 already frames the fix in three layers with a hard boundary; this ADR records how rosary participates.

## The through-line: dispatch is the *one inconsistent operation*

Every other rosary operation is an API call in MCP shape — request → handler does the work → response (`rsry_bead_create`, `rsry_status`, …). `rsry_dispatch` alone breaks that shape: request → **fork a detached host subprocess** → return "dispatched" → then poll a stream and reconcile out-of-band. There is no `mcp → hits api → done`; there is `mcp → fork → hope`.

**That discontinuity is the disease, not five separate bugs.** The corpses (82caac) are a fork nobody holds a handle to. The untestability is behaviour living in an external binary instead of a handler you can assert on. The auth surprise (b1495c) is the fork landing outside the credential context. The reign hole (5251a0) is the fork running with an authority the caller never granted. Restore the shape — dispatch is a **mediated call that runs the work and returns a receipt**, like everything else — and the special case that caused all of it is gone.

There is no "MCP vs. the API": MCP *is* an API call (JSON-RPC to a service). The credential proxy is another API call (HTTP to `/vault/proxy`). Consistency means the dispatch lifecycle is a **managed request/receipt**, not a forgotten pid — and that is *orthogonal* to who holds the LLM credential (Max stays audit-not-custody regardless of where the agent runs). The host fork is not a design; it is the temporary inconsistency this ADR removes.

The decision consistency exists **now** (D2, L0 shipped). The *execution* consistency is the part that isn't done — and cloister names exactly why (D3, `workerd` can't spawn) and exactly what fixes it (libkrun microVM, ADR-0044): the agent becomes a **supervised resource addressable through the plane**, so `done` means "a managed resource finished," not "a `ps` you have to guess about."

## Decision

### D1 — Dispatch is *mediated* by cloister, *executed* host-side

The authoritative model is cloister ADR-0040: cloister mediates the **decision** to dispatch and the **credential** the harness uses and **records** both; the harness process itself runs host-side (workerd can't contain it). rosary does not spawn a cloister bundle per agent; rosary's dispatch **requests flow through cloister's mediated surface**, and the spawned host process is pointed at cloister's credential plane.

### D2 — Orchestration mediation is L0, and it is **shipped** — rosary must not bypass it

`rsry_dispatch` reachable through cloister `/mcp` is lease-gated (who may dispatch) and attested (every dispatch on the §13.4 receipt chain) — cloister ADR-0040 L0, shipped via `cloister-cf7a3b`. rosary's obligation: **the dispatch path that actually spawns must sit behind that mediated surface**, not a raw host entrypoint that skips the lease gate. Today `rsry_dispatch` (MCP) is mediated, but the CLI/detached path spawns directly — that gap is the rosary-side work.

### D3 — The compute sandbox is host-side libkrun (ADR-0044), and `--disallowedTools` is the **stopgap until it lands** — NOT superseded

The `find /` class of failure is *local-tool* reign (Bash/fs), which cloister's control plane does not contain — that is cloister ADR-0044 (libkrun microVM, per-op FUSE mediation, `~/.ssh` never exposed), in progress. Until it lands, `rosary-5251a0`'s `--disallowedTools` denylist for read-only profiles is the correct **stopgap**, not a superseded band-aid. MCP-tool reign (the `rsry_*` surface) *is* mediated once dispatch goes through cloister's lease gate (D2); local-tool reign waits on libkrun.

### D4 — Credentials via cloister L1, with the honest Max/OAuth ceiling

The spawned harness points `ANTHROPIC_BASE_URL` (and `OPENAI_BASE_URL` for codex) at cloister's `/vault/proxy/<name>` (both `anthropic` `x-api-key` and `openai` `authorizationBearer` services are **shipped**, cloister ADR-0040); cloister injects the vaulted key, streams the response, writes a receipt per call. Stock Claude Code doesn't mint Interlace lease headers, so this needs the lease-aware local shim from cloister ADR-0042 (`task harness:dev`, shipped for dev).

**The b1495c ceiling, stated plainly:** for an **API-key** shape this is full custody (harness never sees the key). For the operator's **Max/Pro OAuth** it is **audit, not custody** — cloister receipts every call but cannot hold a token the keychain minted. So "the agent shouldn't hold the Keychain" is achievable for API-key dispatch; for Max-subscription dispatch, the realistic win is *receipts + a lease-scoped channel*, and the OAuth token stays client-side. This is why `claude -p`/Max is a genuine constraint, not just legacy.

### D5 — Durability/lineage = ADR-0015 capsule + the cloister receipt chain

Context death (ADR-0015) is unchanged by this ADR: the worktree stays the disposable surface, the capsule (on jj) stays the durable lineage. cloister's per-dispatch + per-LLM-call receipts are a **second, authenticated** lineage source that the capsule's proof projection can fold in (ADR-0015 D5 already anticipates APAS). rosary's job: emit dispatch attestation so the capsule and the cloister receipt chain reconcile.

### D6 — Corpses (82caac): supervisor, not raw detach

Because the harness runs host-side, the lifecycle handle is rosary's (not cloister's) responsibility — which is why `82caac` (terminal-filtered `list_active_pipelines` + reap-on-read) remains the correct fix, and dispatch should hold a real child handle rather than detaching and forgetting. cloister's mediation records that a dispatch *happened*; rosary must record when it *ended*.

The detached fork exists partly for a real reason: the intended **supervisor is the Elixir/OTP orchestrator layer** (the paid orchestrator tier; `project_tier_architecture` — Rust = pipeline, Elixir/Gleam = orchestrator). OTP supervision trees *are* the "manage / restart / reap an external process" primitive this ADR keeps invoking — a dispatched agent is a natural OTP-supervised port/process, and death is a supervisor event, not a `ps`-guess. So the durable "supervisor handle" likely lands **in OTP, not in the Rust pipeline** — the Rust side spawns/holds, OTP supervises. This is the honest long-term home for D6, but it is **not the immediate concern**: the interim is the Rust-side child handle + 82caac reaping, and the OTP supervision is layered on when the orchestrator tier is in play. (It also composes cleanly with libkrun, D3: OTP supervises the microVM as the resource, not a raw pid.)

### D7 — The exec-out smell rule (V0), narrowed by the census

A census this session found **126 non-test `Command::new` sites across 28 files** — but almost all are legitimate git/dolt/jj/`task` invocations, not agent spawns. So the V0 smell rule (`rosary-9be65d`) must be scoped to **agent-harness spawns** (the `claude`/`gemini`/`codex` exec in `src/dispatch/providers.rs`), flagging any *new* agent-spawn site outside the sanctioned, cloister-mediated seam — not a blanket `Command::new` ban.

## What rosary actually does (the coordination effort)

1. **Route the spawning path behind cloister's mediated `/mcp`** (D2) so every real dispatch inherits the lease gate + attestation — close the CLI/detached bypass.
2. **Point the spawned host harness at cloister's credential plane** (`ANTHROPIC_BASE_URL`/`OPENAI_BASE_URL` → `/vault/proxy`, via the ADR-0042 shim) (D4).
3. **Keep the local-tool stopgaps** (`--disallowedTools`, 82caac reaping) until the cloister compute sandbox (ADR-0044) and a real supervisor land (D3, D6).
4. **Emit dispatch attestation** so the capsule (ADR-0015) and cloister's receipt chain reconcile (D5).
5. **Ship the narrowed exec-out smell rule** (D7) so no new *un-mediated* agent spawn is introduced.

None of this is net-new spawning technology — the plane exists (L0 shipped, L1 building, ADR-0042 runnable). It is wiring rosary's dispatch to stop skipping it.

## Consequences

- Dispatch inherits cloister's lease gate + receipts (audit for all shapes; credential custody for API-key shapes) instead of running unaudited with the operator's ambient keychain.
- Reign splits honestly: **MCP-tool reign** is mediated now; **local-tool reign** (the `find /` class) waits on libkrun (ADR-0044), with `--disallowedTools` as the interim.
- The Max/OAuth ceiling is documented, not papered over: for the operator's subscription, expect **receipts, not custody**.
- No API-key regression is *forced*: API-key/enterprise deployments get custody; Max stays subscription-billed with audit.

## Alternatives considered

- **Run the harness inside a workerd isolate** — impossible (no process-spawn, ADR-0033). This was the original draft's error.
- **A rosary-only OS sandbox around `claude -p`** — duplicates cloister ADR-0044 (libkrun) with less: no per-op FUSE mediation, no shared credential plane, no receipts.
- **Keep raw host spawn + more `--disallowedTools`** — the current stopgap; fine as a stopgap, but it never gets the credential plane, the audit chain, or the compute sandbox, and it can't bind Implement's scoped Bash.

## What we are explicitly **not** claiming

- Not claiming credential *custody* for Max/Pro OAuth — that shape gets **audit only** (cloister ADR-0040).
- Not claiming this contains the *compute* (Bash/fs) — that is cloister ADR-0044, separate and in progress.
- Not superseding `5251a0` or `82caac` — they are the correct stopgaps until the cloister substrate lands.
- Not inventing new spawn technology — this is coordination onto an existing, partly-shipped cloister plane.
