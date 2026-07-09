# Warm-Resume — bounded, content-addressed pipeline context

- **Thread:** warm-resume (under `rosary-41e98b` — content-addressed agent-context substrate)
- **Status:** Design, awaiting approval
- **Date:** 2026-07-08
- **Depends on:** hashing centralization on LLO (`bf6c74`/`bf8121`/`bf9cbf`/`30374f`, merged) — `cas::content_hash` == `leyline_core::ContentAddressed::hash`; LLO `BlobStore` (verify-on-read).

## Problem

Agents re-derive repo knowledge every dispatch: re-read 2k-line files, recompute
impact, re-read prior phases. Two costs, one root:

1. **Context bloat.** `handoff.rs::format_for_prompt` inlines the *entire* handoff
   chain into each phase's prompt. Context grows linearly with phase count and
   retries — the deeper the pipeline, the fatter every prompt.
2. **Cold resume.** A retried / re-dispatched / interrupted agent restarts from
   nothing — it re-reads and re-derives what a prior run already had.

## Invariant (the premise we couple to)

> The pipeline carries a **bounded, content-addressed context envelope.**

"No context bloat" and "warm resume" are the **same mechanism**. Content-addressing
lets the pipeline hold *references* (hashes) plus a bounded working set instead of
inlined copies. That simultaneously:

- **bounds size** — inline a compact digest + refs, not the full history → no bloat;
- **makes resume warm** — refs are already computed; "resume" restores the working
  set + refs, and an agent pulls a full blob by hash *only if it needs it*.

We couple to this invariant, **not to mache.** Mache (or any deriver) becomes *one
optional producer* of a reference-able blob — never a dependency.

## Non-goals

- Not a mache cache. Mache is optional; warm-resume works with zero mache present.
- Not a summarizer/relevance engine (v1). The default pruning policy is mechanical
  and deterministic; a relevance scorer is a future, opt-in policy (see Open Questions).
- Not agent work-state resume *yet* — that is Phase B, layered on the same envelope.

## Architecture — units (each testable in isolation)

### `ContextEnvelope`
The bounded representation that replaces raw chain inlining.
- **hot working set** — full material kept inline, under budget.
- **refs** — `(hash, one-line digest)` pointers to demoted material in the CAS.
- Content-addressed as a whole; a restore of the same envelope is byte-identical.

### `PruningPolicy` (trait, configurable)
Decides, given the running envelope + a new item, what stays hot vs is demoted to a ref.
- **Default `tiers` (deterministic):** the bead spec + the current-phase handoff are
  always hot; older phases are always refs. Dead simple, deterministic.
- **`recency` (deterministic):** keep the most-recent items that fit under budget;
  demote the rest oldest-first.
- Trait seam so a custom (incl. nondeterministic) policy can drop in later.

### `RefStore`
Thin adapter over LLO `BlobStore`.
- `put(bytes) -> hash` (content-addressed; `cas::content_hash`).
- `expand(hash) -> bytes` (verify-on-read; hash mismatch = tamper = hard error).

### handoff.rs change
`format_for_prompt` stops inlining the whole chain. It renders the **`ContextEnvelope`**:
compact digest of the hot set + the ref list. `Handoff::write_to`/`read_chain` are
unchanged — handoffs are still written per phase; the envelope is the *view* the
prompt is built from.

## Config

```toml
[pipeline.context]
policy = "tiers"   # "tiers" (default) | "recency" | <custom>
budget = 8000      # ceiling (tokens) the envelope must stay under
```

## Data flow

1. Phase completes → `Handoff` written to the workspace (as today).
2. Envelope updates: new handoff considered by `PruningPolicy`; current stays hot,
   older material demoted to refs (`RefStore::put`), all kept under `budget`.
3. Next phase's prompt is built from the **bounded** envelope (digest + refs).
4. Agent calls **`expand_ref <hash>`** (new MCP tool → `RefStore::expand`) only if it
   needs a demoted blob.
5. **Resume** = restore the envelope (working set + refs). Refs are already computed
   ⇒ warm. Nothing already-held is re-fetched.

## Proof-gate (committed, runs in `task check`)

The design is only as good as its falsifiable claims. Pinned on the deterministic
default policy:

1. **Bound** — envelope size stays under `budget` across 2 / 20 / retry / resume
   depth. Assert context does **not** grow with history. Falsifiable.
2. **Warmth** — on resume, count refs re-fetched that were already held → **must be 0.**
3. **Round-trip** — `put(bytes)` → ref → `expand(hash)` → **byte-equal** (golden-style,
   same discipline as `bf6c74`).

Custom/nondeterministic policies are allowed but run under a looser assertion (bound
only), so *configurable* never rots *provable*.

*Continuation* (Phase B work-state: a resumed agent doesn't redo completed steps) is
proven **once as a demo**, not a permanent gate — agent-behavior tests are the flaky
kind we `#[ignore]` (rosary-59ff84).

## Error handling (never silent)

- `expand` miss / hash mismatch → **fail-loud with the hash** (verify-on-read catches
  tamper; a missing blob is a deterministic error, not a silent empty context).
- Budget still exceeded after pruning → **hard-prune oldest + log** the drop; never
  silently exceed the ceiling (no-silent-caps, per feedback_no_moving_gates).

## Sequencing

- **Phase A (this spec):** `ContextEnvelope` + `PruningPolicy` + `RefStore` + the
  handoff-format change + `expand_ref` tool + the three-part proof-gate. Delivers
  no-bloat **and** warm resume for derived/handoff context.
- **Phase B (follow-up):** agent work-state resume (plan + progress + reads) layered
  on the *same* envelope, gated on continuation. Separate spec.

## Open questions

- **Relevance policy** — a scoring `PruningPolicy` that ranks what the current phase
  needs. Powerful, but reintroduces nondeterminism + a dependency; deferred, opt-in,
  looser gate.
- **Budget unit** — tokens vs bytes vs item-count. Start with tokens (matches the real
  constraint); revisit if the tokenizer coupling is annoying.
