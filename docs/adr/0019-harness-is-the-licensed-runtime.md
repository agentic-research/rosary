# ADR-0019: The harness is the licensed runtime — RuntimeProvider drives it, ModelProvider replaces it

**Status:** Proposed
**Date:** 2026-07-17

**Relates to:**
- Local: [ADR-0016](0016-dispatch-via-cloister.md) (rosary-side coordination with cloister's harness plane — the custody ceiling this ADR generalizes), `rosary-c79331` (ModelProvider three-layer design), `rosary-470270` (vault-custody RCA — the empirical evidence), `rosary-aeb573` (Anthropic direct adapter, API-key lane), `rosary-f43f64` (local-model provider, the escape).
- cloister (authoritative for the credential plane): [ADR-0040](../../../cloister/docs/adr/0040-harness-in-cloister.md) (harness = control + credential + audit plane; **"Scope of the credential claim"** = custody for API-key shapes, audit-not-custody for subscription OAuth), [ADR-0024](../../../cloister/docs/adr/0024-credential-isolation-capability.md) (`credential-isolation/v1` — the vault pattern the custody lane reuses).

## Context

Rosary talks to models three ways: it drives a harness (`claude -p`, `codex exec`, ACP), and — per `rosary-c79331` — it will also make direct model calls. When we tried to collapse these into one "a provider is a thing that emits tokens" abstraction, an empirical result stopped us (`rosary-470270`):

> A Max/Pro **subscription** OAuth token (`sk-ant-oat…`) sent to `POST /v1/messages` **authenticates** but returns **HTTP 400 "credit balance too low"** — not 401/403. The token is valid; the *Console/Platform* org it maps to simply has no API credits.

Two ledgers, not one. The seat and the pay-as-you-go API are different wallets, and the seat's money isn't in the API one. The deeper reason (confirmed against Anthropic's support docs and cloister ADR-0040): **the seat licenses the *harness's* use of the model on your behalf — not your use of the token.** `claude -p` is still the harness, headless. The instant you extract the token and call `/v1/messages` yourself, you haven't front-ended the harness — you've *replaced* it, and the seat was never a license to do that.

This is the same ceiling cloister ADR-0040 already records from the custody side: cloister can **custody** an API-key-shaped credential, but for a subscription OAuth token minted in the client's keychain it can only offer **audit (receipts), not custody**. Same principle, observed from two directions.

## Decision

**Model the provider seam in two layers, because the harness is the licensed runtime, not a swappable front-end.** (This is the joint `rosary-c79331` already carves; this ADR records *why* the joint is there.)

- **`RuntimeProvider` — drive an existing harness.** `claude -p`, `codex exec`, ACP. The credential is the *harness's* (keychain/OAuth); rosary never holds it. Seat-powered, headless, job-shaped — this **is** the "terminal, not chat" execution model, and it is sanctioned for the operator's individual use. Custody lives with the harness; cloister gives audit-not-custody for subscription shapes (ADR-0040, ADR-0016).
- **`ModelProvider` — become the runtime yourself.** Direct `generate`/`stream`. This detaches from the harness, so it **requires a first-class `Credential` rosary can custody** — an API key (`sk-ant-api…`) or a vault-proxied OAuth grant — *never* the subscription seat token. This is the only lane cloister can fully custody (ADR-0024).

**Corollary — local models collapse the wall.** `ANTHROPIC_BASE_URL` swaps the *model backend*, not the harness; a `localhost` base_url swaps the *model* in the ModelProvider lane. Either way the model is no longer Anthropic's, so **no seat, no token, no policy line applies** — the wall exists only when the *model* is Anthropic's, not when the *harness* is Claude Code (`rosary-f43f64`).

## Consequences

| Lane | "Who is the runtime?" | Credential | Seat/policy wall? | Cloister |
|---|---|---|---|---|
| RuntimeProvider (`claude -p`, codex, ACP) | the harness | harness-held (keychain/OAuth) | applies (individual-use OK) | audit-not-custody for OAuth |
| ModelProvider (direct call) | **rosary** | must be custody-able — **API key**, not seat token | n/a (own metered key) | full custody (ADR-0024) |
| Local (either lane) | harness or rosary | none / throwaway | **none** — not an Anthropic model | nothing to custody |

Concretely:
- **`bdr_enrich` stays on `claude -p`** (RuntimeProvider) for the Max seat. Do **not** migrate it to a direct ModelProvider call on the subscription token — that would cross both the billing ledger and the policy line.
- **The Anthropic direct adapter (`rosary-aeb573`) is specced for an API key** *precisely because* ModelProvider = replace-the-runtime = needs its own custody-able credential. This ADR is the "why" behind that bead's design note.
- **The local provider (`rosary-f43f64`) is the vendor-neutral escape** and the cleanest thing to custody (no credential at all). It's also the honest hedge: full CC harness, your metal, lower model quality — a *tier*, not a Claude replacement.

## Non-goals / deferrals

- Agent-process *sandboxing* (the `find /` problem) is cloister ADR-0044 (libkrun), not here.
- The custody mechanics (vault DO, KEK source, receipts) are authoritative on the cloister side (ADR-0040/0024); this ADR only records the rosary-facing consequence of *which lane can be custodied at all*.
