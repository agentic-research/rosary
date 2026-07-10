# Warm-Resume Phase B — work-state resume + a staleness-invalidated context cache

- **Thread:** warm-resume (under `rosary-41e98b` — content-addressed agent-context substrate)
- **Bead:** `rosary-a9f5dc`
- **Builds on:** Phase A (`rosary-dd5828`) — `ContextEnvelope` + `PruningPolicy` + `RefStore` + CAS (`src/context/`), already shipped and dogfooded.
- **Status:** design, approved 2026-07-10.

## Problem

Phase A made the *derived/handoff* context bounded and warm-resumable: a re-dispatched agent restores the envelope (hot working set + content-addressed refs) instead of re-inlining the whole chain. Two gaps remain:

1. **Work-state resume.** A resumed agent still redoes completed steps — it has the *context* but not its own *plan + progress + reads*. It re-reads files it already read, re-derives conclusions it already reached.
2. **No reuse across dispatches, and no protection against reusing the wrong thing.** Derived context is recomputed every dispatch even when nothing it depends on changed (cold when it could be warm); and there is no mechanism that guarantees we never serve context whose underlying state has moved ("bad context").

Phase B closes both on the *same* content-addressed envelope, and does so under a proof discipline that makes it impossible to serve stale context before a committed gate proves the cache equivalent to re-derivation.

## The model: staleness-via-cascade (not failure-triggered)

"Bad context" is **stale context** — context whose underlying state has changed — resolved by change-propagation, exactly as leyline's sheaf cache does it (`ll-open/sheaf/src/cache.rs`), which mache consumes:

- `CacheEntry { value, generation, valid }` — an entry stamped with the generation it was cached at.
- `get(key)` returns the value **only while `valid`**.
- `on_change(changed_regions)` bumps the generation, marks those regions invalid, and BFS-propagates invalidation through the coupling graph, **returning the full cascade** — because, in leyline's words, *"UDS/MCP consumers own their own caches and need the full cascade list to evict on their side."*

**Rosary is one of those cascade consumers.** There is **no separate quality gate**: a failed attempt only makes its context bad insofar as it *changed state* (new commits) that invalidates the prior derivation — which staleness already subsumes. Fix-forward's `<previous_attempt>` diagnostic (`completion.rs` → `.rsry-retry.md` → `prompt.rs`) is orthogonal and untouched.

**Warmth and safety are the same coin:** on resume, an entry whose generation still matches (nothing it depends on changed) is served warm; anything in a since-then cascade is re-derived.

## Components

### 1. `ContextCache` — generation-stamped, content-addressed (`src/context/cache.rs`)

Aligns with LLO's primitive rather than forking it:

```
struct CacheEntry<V> { value: V, generation: u64, valid: bool, provenance: Provenance }
struct ContextCache { generation: u64, entries: Map<CacheKey, CacheEntry<Ref>> }
```

- **`CacheKey`** = the blake3 content-hash the CAS already produces for a derived-context blob (`RefStore`). Keys are content-addressed, so identical derivations collide (dedup) and round-trip is byte-exact.
- **`Provenance`** = the set of source regions an entry was derived from (`{bead, commit_sha, source_refs}`). This is what an invalidation cascade is intersected against.
- `get(key)` → the ref **only if `valid`** (LLO semantics — `get` never recomputes freshness at read time). `put(key, ref, provenance)` stamps the current generation and `valid = true`.
- **`valid` is flipped solely by `on_change`.** `generation` is the monotone clock `on_change` advances and stamps entries with; it is *not* re-checked at `get` time. This keeps reads O(1) and the invalidation logic in exactly one place.

### 2. Invalidation — self-contained floor + optional leyline refiner

**Baseline (always on, zero external dependency — preserves Phase A's "works with zero mache/leyline present" invariant):** on state rosary already observes — a bead's `commit_sha` change, a pipeline phase advance, a new handoff appended — it calls `on_change(changed_regions)`. With no coupling graph, the floor's cascade is a scan: mark `valid = false` on every entry whose `provenance` intersects the changed region set (e.g. all entries derived at the old sha). Coarse (over-evicts) but **correct and deterministic** — and it uses the *same* `on_change`/`valid` contract as the leyline refiner, so `get` is identical either way.

**Refiner (only when leyline is present):** rosary consumes leyline's `on_change` cascade to evict the *true blast radius* instead of everything-at-old-sha — warmer, **never required for correctness**. This is exactly how mache treats leyline: auto-provisioned, not mandatory.

### 3. Work-state resume — plan + progress + reads on the envelope

The envelope gains a `work_state` region alongside the hot working set: the agent's plan, per-step progress, and the set of files/refs it has already read (by content-hash). On resume, `build_bounded_prompt` restores `work_state` so the agent continues from its last completed step instead of restarting. Reads already in the cache (fresh generation) are served warm; stale ones re-derive. Continuation is proven **once as a demo**, not a permanent gate — agent-behavior tests are the flaky kind we `#[ignore]` (`rosary-59ff84`).

### 4. Provider cache — designed-for, built later

The render boundary (`ContextRenderer` / `envelope.render()`) emits a `cache_control` breakpoint at the stable content-addressed prefix so an external provider (Anthropic/OpenAI) prompt-caches the ctx-derived prefix. **Designed now** (the breakpoint is a render-time hook keyed off the same content boundary), **built later** — this phase ships only the rosary-owned local cache.

## Verification & safety (the "certain early" pillar)

The cache is a **warmth layer over a proven-correct cold path** (Phase A re-derivation). It never changes correctness — only *skips* re-derivation when provably fresh. The whole safety argument reduces to one falsifiable claim: **warm-served content is byte-identical to what the cold path would derive.**

### Shadow mode first (ADR-0010 / R4b discipline)

Exactly as the observation lattice ships derived-status in shadow before flipping the source of truth (`observation/shadow.rs`, `lattice audit`), the cache ships in **shadow**: every dispatch renders *both* cold (authoritative — fed to the agent) and warm (computed, **not** fed), and asserts `warm ≡ cold`. Divergence is logged + quarantined (`observation/quarantine.rs`), never served. Zero prod risk because cold stays authoritative.

### Falsifiable gates (committed, in `task check`) — extending Phase A's bound/warmth/round-trip

1. **Freshness-soundness** — `valid` never lies: for any entry with `valid = true`, no `on_change` since its `put` touched a region in its `provenance`. (The "never serve stale" invariant — asserts `on_change` evicts everything it must.)
2. **Eviction-completeness** — after `on_change(R)`, `get` returns `None` for R and its whole cascade.
3. **Shadow-equivalence** — warm-served ≡ cold-derived, byte-equal, over a committed fixture corpus.
4. **Warmth** — a no-change resume ⟹ re-derivations = 0 (the payoff; Phase A's warmth gate extended).

Custom/nondeterministic pruning policies (Phase A) still run under the looser bound-only assertion, so *configurable* never rots *provable*.

### Rollback as a config axis — `[pipeline.context] cache = "off" | "shadow" | "on"`

- `off` — always cold (exact Phase A behavior; the escape hatch).
- `shadow` — compute + compare, serve cold. **Ships here.**
- `on` — serve warm — flipped only after shadow-equivalence is green over a corpus (the same evidence-gated promotion as the lattice source-of-truth flip; corpus evidence via a `context cache audit` subcommand mirroring `lattice audit`).
- Any shadow divergence **auto-demotes to `off`** (kill-switch).

Rollback is a one-line config flip, not a revert.

## Data flow

```
dispatch → build_bounded_prompt(chain, cfg, cas_dir):
  cold = render(chain)                         # authoritative (Phase A)
  if cfg.cache != off:
    warm = cache.get_or_derive(chain):
       key = blake3(region)
       hit  = cache.get(key)                     # Some(ref) iff valid
       miss = derive + cache.put(key, ref, provenance)
    if cfg.cache == shadow:  assert warm ≡ cold; serve cold
    if cfg.cache == on:      serve warm
  on state change (sha/phase/handoff, or leyline on_change):
    cache.on_change(regions) → bump generation, evict cascade
```

## Error handling (never silent)

- `expand`/`get` hash mismatch → **fail-loud with the hash** (verify-on-read catches tamper; a missing blob is a deterministic error, not silent empty context — inherited from Phase A).
- Shadow divergence → log the diverging key + both digests, quarantine the entry, **auto-demote to `off`**; never silently serve.
- Budget still exceeded after pruning → **hard-prune oldest + log** the drop (no-silent-caps, `feedback_no_moving_gates`).

## Sequencing

- **B1 — cache core:** `ContextCache` + `CacheEntry`/generation + content-addressed keys + self-contained invalidation floor + the four falsifiable gates. Ships in `shadow`.
- **B2 — work-state resume:** `work_state` region on the envelope + restore in `build_bounded_prompt` + the one-shot continuation demo.
- **B3 — leyline cascade refiner:** consume `on_change` to narrow eviction when leyline is present. Optional; correctness-neutral.
- **B4 — flip to `on`:** after shadow-equivalence is green over a corpus (`context cache audit`). Separate, evidence-gated step.
- **Later (not this phase):** provider `cache_control` feed.

## Success criteria (falsifiable)

- The four gates above are committed and run in `task check`.
- `cache = shadow` runs a real dispatch chain rendering both paths with **zero** shadow divergence over the fixture corpus.
- With `cache = on` (post-flip), a no-change resume re-derives **0** regions (warmth), and a one-region `on_change` evicts exactly that region's cascade (eviction-completeness).
- `cache = off` reproduces Phase A byte-for-byte (rollback correctness).

## Open questions

- **Cascade granularity of the self-contained floor** — is `(bead, sha)` the right coarse region, or do we want `(bead, sha, source_file)` from the start? Coarser is safer/simpler; finer reduces over-eviction before the leyline refiner lands. Leaning `(bead, sha, source_refs)` since the envelope already tracks source refs.
- **Provenance for non-file-derived context** (e.g. an LLM-summarized handoff) — its provenance is the *inputs* to the summary (their content-hashes), so a change to any input invalidates the summary. Confirm this composes cleanly through multi-hop derivations.
