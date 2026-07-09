# Warm-Resume Phase A Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the pipeline carry a bounded, content-addressed context envelope so prompts don't bloat with phase depth and a resumed agent starts warm.

**Architecture:** A new `src/context/` module: `RefStore` (adapter over LLO `leyline_core::BlobStore`), a `PruningPolicy` trait (`tiers` default + `recency`), and a `ContextEnvelope` that renders recent handoffs hot and demotes older ones to CAS refs, enforcing a hard byte budget. `dispatch/prompt.rs` builds the envelope instead of inlining the whole chain; a `rsry_expand_ref` MCP tool fetches a demoted blob on demand.

**Tech Stack:** Rust, `leyline_core` (BlobStore, ContentAddressed hash — already a dep), `serde_json`, existing `src/handoff.rs`, `src/cas.rs`.

## Global Constraints

- Bead: `rosary-dd5828`. Spec: `docs/superpowers/specs/2026-07-08-warm-resume-design.md`.
- Content hashing goes through `crate::cas::content_hash(bytes: &[u8]) -> String` (hex) — never `blake3` directly.
- LLO API (already available, `leyline-core` `default-features = false`, no feature gate): `leyline_core::BlobStore` trait — `fn put(&mut self, bytes: &[u8]) -> anyhow::Result<Hash>`, `fn get(&self, h: Hash) -> anyhow::Result<Option<Vec<u8>>>`; impls `MemBlobStore` (tests), `FsBlobStore` (prod); `leyline_core::Hash` (`[u8;32]`, `Hash::as_bytes()`).
- Commit trailers on every commit:
  `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`
  `Claude-Session: https://claude.ai/code/session_01TzQGRJiJEZr3C4MbSySDkZ`
- Stage explicit paths (never `git add -A`). Run `task check` before the final commit of the last task.
- Deterministic policies only in committed gates. No silent truncation — over-budget → hard-prune + `log`.

---

### Task 1: `RefStore` — content-addressed blob adapter over LLO

**Files:**
- Create: `src/context/mod.rs` (module root — `pub mod ref_store; pub mod policy; pub mod envelope;` added as later tasks land; start with `pub mod ref_store;`)
- Create: `src/context/ref_store.rs`
- Modify: `src/main.rs` (add `mod context;` next to the other `mod` declarations)

**Interfaces:**
- Consumes: `leyline_core::{BlobStore, MemBlobStore, FsBlobStore, Hash}`, `crate::cas::content_hash`.
- Produces:
  - `pub struct RefStore<B: BlobStore> { store: B, puts: usize }`
  - `pub fn new(store: B) -> Self`
  - `pub fn put(&mut self, bytes: &[u8]) -> anyhow::Result<String>` (returns hex hash; increments `puts`)
  - `pub fn expand(&self, hash_hex: &str) -> anyhow::Result<Option<Vec<u8>>>`
  - `pub fn puts(&self) -> usize` (test instrumentation for the warmth gate)
  - `pub fn hash_from_hex(hex: &str) -> anyhow::Result<Hash>` (helper)

- [ ] **Step 1: Write the failing test**

```rust
// in src/context/ref_store.rs, #[cfg(test)] mod tests
use super::*;
use leyline_core::MemBlobStore;

#[test]
fn round_trip_and_tamper() {
    let mut rs = RefStore::new(MemBlobStore::default());
    let hash = rs.put(b"hello warm resume").unwrap();
    // hex hash equals cas::content_hash (single source of truth)
    assert_eq!(hash, crate::cas::content_hash(b"hello warm resume"));
    // expand round-trips byte-identical
    assert_eq!(rs.expand(&hash).unwrap().as_deref(), Some(&b"hello warm resume"[..]));
    // a wrong/unknown hash is a clean miss, not a panic
    let bogus = "0".repeat(64);
    assert_eq!(rs.expand(&bogus).unwrap(), None);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin rsry context::ref_store::tests::round_trip_and_tamper`
Expected: FAIL — `RefStore` not found.

- [ ] **Step 3: Write minimal implementation**

```rust
//! Content-addressed blob adapter over LLO's BlobStore. Demoted context lives
//! here keyed by its content hash; agents fetch on demand via `expand_ref`.
use anyhow::{Context, Result};
use leyline_core::{BlobStore, Hash};

pub struct RefStore<B: BlobStore> {
    store: B,
    puts: usize,
}

pub fn hash_from_hex(hex: &str) -> Result<Hash> {
    let bytes = hex::decode(hex).context("ref hash is not valid hex")?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("ref hash must be 32 bytes"))?;
    Ok(Hash::from(arr))
}

impl<B: BlobStore> RefStore<B> {
    pub fn new(store: B) -> Self {
        Self { store, puts: 0 }
    }

    /// Store bytes, return the hex content hash. The hex equals
    /// `cas::content_hash` so a bead's ref == the same CAS everywhere.
    pub fn put(&mut self, bytes: &[u8]) -> Result<String> {
        self.store.put(bytes).context("blobstore put")?;
        self.puts += 1;
        Ok(crate::cas::content_hash(bytes))
    }

    /// Fetch a blob by hex hash. `Ok(None)` on a clean miss; verify-on-read in
    /// the underlying store turns a tampered blob into an `Err`.
    pub fn expand(&self, hash_hex: &str) -> Result<Option<Vec<u8>>> {
        let h = hash_from_hex(hash_hex)?;
        self.store.get(h).context("blobstore get")
    }

    pub fn puts(&self) -> usize {
        self.puts
    }
}
```

Note: confirm `Hash: From<[u8;32]>`. If not, use the constructor `leyline_core::Hash::from_bytes(arr)` (check `substrate.rs` — the golden test in `cas.rs:86` shows `leyline_core::Hash` and `.as_bytes()`; mirror whatever constructor exists).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --bin rsry context::ref_store::tests::round_trip_and_tamper`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/context/mod.rs src/context/ref_store.rs src/main.rs
git commit -m "$(printf '%s\n\n%s\n\n%s\n%s' \
  '[rosary-dd5828] feat(context): RefStore — CAS adapter over LLO BlobStore' \
  'Round-trips bytes↔hex-hash via cas::content_hash; the proof-gate round-trip claim.' \
  'Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>' \
  'Claude-Session: https://claude.ai/code/session_01TzQGRJiJEZr3C4MbSySDkZ')"
```

---

### Task 2: `[context]` config section

**Files:**
- Modify: `src/config/mod.rs` (add struct + `Config` field near `pipelines`, line ~36)
- Test: same file, `#[cfg(test)] mod tests`

**Interfaces:**
- Produces:
  - `pub struct ContextConfig { pub policy: String, pub budget: usize, pub max_refs: usize }`
  - `Config.context: ContextConfig` (serde default)
  - defaults: `policy = "tiers"`, `budget = 8000`, `max_refs = 8`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn context_config_defaults_when_absent() {
    let cfg: Config = toml::from_str("").unwrap();
    assert_eq!(cfg.context.policy, "tiers");
    assert_eq!(cfg.context.budget, 8000);
    assert_eq!(cfg.context.max_refs, 8);
}

#[test]
fn context_config_overrides() {
    let cfg: Config = toml::from_str(
        "[context]\npolicy = \"recency\"\nbudget = 4000\nmax_refs = 4\n",
    ).unwrap();
    assert_eq!(cfg.context.policy, "recency");
    assert_eq!(cfg.context.budget, 4000);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --bin rsry config::tests::context_config_defaults_when_absent`
Expected: FAIL — no `context` field.

- [ ] **Step 3: Implement**

```rust
// near the top-level Config struct fields:
#[serde(default)]
pub context: ContextConfig,

// with the other config structs:
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ContextConfig {
    #[serde(default = "default_context_policy")]
    pub policy: String,
    #[serde(default = "default_context_budget")]
    pub budget: usize,
    #[serde(default = "default_context_max_refs")]
    pub max_refs: usize,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            policy: default_context_policy(),
            budget: default_context_budget(),
            max_refs: default_context_max_refs(),
        }
    }
}

fn default_context_policy() -> String { "tiers".to_string() }
fn default_context_budget() -> usize { 8000 }
fn default_context_max_refs() -> usize { 8 }
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --bin rsry config::tests::context_config`
Expected: PASS (both tests)

- [ ] **Step 5: Commit**

```bash
git add src/config/mod.rs
git commit -m "$(printf '%s\n\n%s\n%s' \
  '[rosary-dd5828] feat(config): [context] section — policy/budget/max_refs' \
  'Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>' \
  'Claude-Session: https://claude.ai/code/session_01TzQGRJiJEZr3C4MbSySDkZ')"
```

---

### Task 3: `PruningPolicy` trait + `tiers` and `recency`

**Files:**
- Create: `src/context/policy.rs`
- Modify: `src/context/mod.rs` (add `pub mod policy;`)

**Interfaces:**
- Consumes: `crate::handoff::Handoff`.
- Produces:
  - `pub fn estimate_tokens(s: &str) -> usize` (deterministic: `s.len().div_ceil(4)`)
  - `pub trait PruningPolicy { fn hot_count(&self, phase_sizes: &[usize], budget: usize) -> usize; }`
    (returns how many of the MOST RECENT phases stay hot; the rest are demoted — deterministic, index-based)
  - `pub struct TiersPolicy;` — always `1` (current phase only), or `0` if empty.
  - `pub struct RecencyPolicy;` — greedily counts most-recent phases whose cumulative size fits `budget`.
  - `pub fn policy_from_name(name: &str) -> Box<dyn PruningPolicy>` (unknown → `TiersPolicy` + `log::warn!`)

- [ ] **Step 1: Write the failing test**

```rust
use super::*;

#[test]
fn tiers_keeps_only_current_hot() {
    let sizes = vec![100, 100, 100];
    assert_eq!(TiersPolicy.hot_count(&sizes, 10_000), 1);
    assert_eq!(TiersPolicy.hot_count(&[], 10_000), 0);
}

#[test]
fn recency_keeps_most_recent_that_fit() {
    // budget 250 fits the last two (100+100) but not the third (would be 300)
    let sizes = vec![100, 100, 100];
    assert_eq!(RecencyPolicy.hot_count(&sizes, 250), 2);
    // huge budget keeps all
    assert_eq!(RecencyPolicy.hot_count(&sizes, 10_000), 3);
    // tiny budget still keeps at least the current phase
    assert_eq!(RecencyPolicy.hot_count(&sizes, 1), 1);
}

#[test]
fn policy_from_name_is_total() {
    assert_eq!(policy_from_name("tiers").hot_count(&[9,9], 10_000), 1);
    assert_eq!(policy_from_name("recency").hot_count(&[9,9], 10_000), 2);
    assert_eq!(policy_from_name("nonsense").hot_count(&[9,9], 10_000), 1); // falls back to tiers
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --bin rsry context::policy::tests`
Expected: FAIL — module/types not found.

- [ ] **Step 3: Implement**

```rust
//! Deterministic pruning policies: how many of the most-recent pipeline phases
//! stay "hot" (rendered in full) vs are demoted to CAS refs.

pub fn estimate_tokens(s: &str) -> usize {
    s.len().div_ceil(4)
}

pub trait PruningPolicy {
    /// Given per-phase sizes (oldest→newest) and a budget, return how many of
    /// the MOST RECENT phases stay hot. Deterministic.
    fn hot_count(&self, phase_sizes: &[usize], budget: usize) -> usize;
}

pub struct TiersPolicy;
impl PruningPolicy for TiersPolicy {
    fn hot_count(&self, phase_sizes: &[usize], _budget: usize) -> usize {
        if phase_sizes.is_empty() { 0 } else { 1 }
    }
}

pub struct RecencyPolicy;
impl PruningPolicy for RecencyPolicy {
    fn hot_count(&self, phase_sizes: &[usize], budget: usize) -> usize {
        let mut acc = 0usize;
        let mut kept = 0usize;
        for &sz in phase_sizes.iter().rev() {
            acc = acc.saturating_add(sz);
            if kept > 0 && acc > budget {
                break;
            }
            kept += 1; // always keep at least the current phase
        }
        kept
    }
}

pub fn policy_from_name(name: &str) -> Box<dyn PruningPolicy> {
    match name {
        "recency" => Box::new(RecencyPolicy),
        "tiers" => Box::new(TiersPolicy),
        other => {
            log::warn!("unknown context.policy '{other}', defaulting to tiers");
            Box::new(TiersPolicy)
        }
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --bin rsry context::policy::tests`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/context/policy.rs src/context/mod.rs
git commit -m "$(printf '%s\n\n%s\n%s' \
  '[rosary-dd5828] feat(context): PruningPolicy trait — deterministic tiers + recency' \
  'Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>' \
  'Claude-Session: https://claude.ai/code/session_01TzQGRJiJEZr3C4MbSySDkZ')"
```

---

### Task 4: `ContextEnvelope` — bounded render + the BOUND gate

**Files:**
- Create: `src/context/envelope.rs`
- Modify: `src/context/mod.rs` (add `pub mod envelope;`)

**Interfaces:**
- Consumes: `crate::handoff::Handoff`, `super::policy::{PruningPolicy, estimate_tokens}`, `super::ref_store::RefStore`, `leyline_core::BlobStore`.
- Produces:
  - `pub struct ContextEnvelope { hot: Vec<String>, refs: Vec<RefLine>, rollup: Option<String> }`
  - `pub struct RefLine { pub hash: String, pub digest: String }`
  - `pub fn build<B: BlobStore>(chain: &[Handoff], policy: &dyn PruningPolicy, budget: usize, max_refs: usize, store: &mut RefStore<B>) -> anyhow::Result<ContextEnvelope>`
  - `pub fn render(&self) -> String`
- Behavior: render one full block per hot handoff (reuse `Handoff::format_for_prompt` on the hot slice), a one-line `RefLine` per demoted handoff up to `max_refs`, older-than-`max_refs` collapsed into one `rollup` ref (a CAS blob holding the JSON list of the remaining `RefLine`s). Hard invariant: `render().len() <= budget` — if hot alone exceeds budget, demote the oldest hot phases until it fits, and `log::warn!` the drop.

- [ ] **Step 1: Write the failing test**

```rust
use super::*;
use crate::context::policy::TiersPolicy;
use crate::context::ref_store::RefStore;
use crate::handoff::Handoff;
use leyline_core::MemBlobStore;

fn phase(n: u32) -> Handoff {
    // minimal Handoff for phase n; use the existing constructor / builder.
    Handoff::new_test(n, &format!("phase {n} did work")) // see note below
}

#[test]
fn bound_holds_across_depth() {
    let budget = 2000;
    let mk = |depth: usize| {
        let chain: Vec<Handoff> = (0..depth as u32).map(phase).collect();
        let mut rs = RefStore::new(MemBlobStore::default());
        let env = build(&chain, &TiersPolicy, budget, 8, &mut rs).unwrap();
        env.render().len()
    };
    let shallow = mk(2);
    let deep = mk(20);
    assert!(shallow <= budget, "depth 2 render {shallow} exceeds budget");
    assert!(deep <= budget, "depth 20 render {deep} exceeds budget");
    // "does not grow with history": deep is not materially larger than shallow
    assert!(deep <= shallow + 400, "render grew with depth: {shallow} -> {deep}");
}

#[test]
fn older_than_max_refs_roll_up() {
    let chain: Vec<Handoff> = (0..12u32).map(phase).collect();
    let mut rs = RefStore::new(MemBlobStore::default());
    let env = build(&chain, &TiersPolicy, 8000, 4, &mut rs).unwrap();
    assert_eq!(env.refs.len(), 4);          // capped
    assert!(env.rollup.is_some());          // remainder collapsed to one blob
}
```

Note: if `Handoff` has no lightweight test constructor, add `#[cfg(test)] pub fn new_test(phase: u32, summary: &str) -> Self` to `src/handoff.rs` populating required fields with defaults — fold that into this task's Step 3.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --bin rsry context::envelope::tests`
Expected: FAIL — `build`/`ContextEnvelope` not found.

- [ ] **Step 3: Implement**

```rust
//! Bounded, content-addressed view of the handoff chain. Recent phases render
//! hot; older ones demote to CAS refs; the whole thing stays under `budget`.
use anyhow::Result;
use leyline_core::BlobStore;

use crate::handoff::Handoff;
use super::policy::{estimate_tokens, PruningPolicy};
use super::ref_store::RefStore;

pub struct RefLine {
    pub hash: String,
    pub digest: String,
}

pub struct ContextEnvelope {
    hot: Vec<String>,
    pub refs: Vec<RefLine>,
    pub rollup: Option<String>,
}

fn digest_of(h: &Handoff) -> String {
    format!("phase {} ({}): {}", h.phase, h.from_agent, h.summary)
}

pub fn build<B: BlobStore>(
    chain: &[Handoff],
    policy: &dyn PruningPolicy,
    budget: usize,
    max_refs: usize,
    store: &mut RefStore<B>,
) -> Result<ContextEnvelope> {
    if chain.is_empty() {
        return Ok(ContextEnvelope { hot: vec![], refs: vec![], rollup: None });
    }
    // sizes oldest→newest
    let sizes: Vec<usize> = chain
        .iter()
        .map(|h| estimate_tokens(&Handoff::format_for_prompt(std::slice::from_ref(h))))
        .collect();
    let mut hot_n = policy.hot_count(&sizes, budget).min(chain.len());

    // Render hot; if over budget, demote oldest hot until it fits (never silent).
    let split = chain.len() - hot_n;
    let mut hot: Vec<String> =
        chain[split..].iter().map(|h| Handoff::format_for_prompt(std::slice::from_ref(h))).collect();
    while hot_n > 1 && hot.iter().map(|s| s.len()).sum::<usize>() > budget {
        log::warn!("context envelope over budget; demoting an extra hot phase");
        hot.remove(0);
        hot_n -= 1;
    }

    // Demoted = everything before the hot slice, newest-first.
    let demoted: Vec<&Handoff> = chain[..chain.len() - hot_n].iter().rev().collect();
    let mut refs = Vec::new();
    for h in demoted.iter().take(max_refs) {
        let blob = serde_json::to_vec(h)?;
        let hash = store.put(&blob)?;
        refs.push(RefLine { hash, digest: digest_of(h) });
    }
    let rollup = if demoted.len() > max_refs {
        let rest: Vec<String> = demoted[max_refs..].iter().map(|h| digest_of(h)).collect();
        let blob = serde_json::to_vec(&rest)?;
        Some(store.put(&blob)?)
    } else {
        None
    };

    Ok(ContextEnvelope { hot, refs, rollup })
}

impl ContextEnvelope {
    pub fn render(&self) -> String {
        let mut out = String::new();
        for block in &self.hot {
            out.push_str(block);
        }
        if !self.refs.is_empty() || self.rollup.is_some() {
            out.push_str("\n## Earlier context (fetch with rsry_expand_ref)\n");
            for r in &self.refs {
                out.push_str(&format!("- {} [{}]\n", r.digest, r.hash));
            }
            if let Some(ref h) = self.rollup {
                out.push_str(&format!("- (+ older phases rolled up) [{h}]\n"));
            }
        }
        out
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --bin rsry context::envelope::tests`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/context/envelope.rs src/context/mod.rs src/handoff.rs
git commit -m "$(printf '%s\n\n%s\n\n%s\n%s' \
  '[rosary-dd5828] feat(context): ContextEnvelope — bounded render + BOUND proof-gate' \
  'render() stays <= budget across arbitrary depth (tested at 2 and 20); older refs roll up.' \
  'Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>' \
  'Claude-Session: https://claude.ai/code/session_01TzQGRJiJEZr3C4MbSySDkZ')"
```

---

### Task 5: WARMTH gate — resume re-fetches nothing already held

**Files:**
- Modify: `src/context/envelope.rs` (`#[cfg(test)] mod tests` — add the warmth test only; no prod change)

**Interfaces:**
- Consumes: `RefStore::puts()`.
- Produces: no new API — this is the proof-gate's warmth claim as a committed test.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn warmth_resume_refetches_nothing_already_held() {
    let chain: Vec<Handoff> = (0..10u32).map(phase).collect();
    let mut rs = RefStore::new(MemBlobStore::default());

    // First build demotes older phases → some puts.
    let _ = build(&chain, &TiersPolicy, 2000, 8, &mut rs).unwrap();
    let after_first = rs.puts();
    assert!(after_first > 0, "first build should demote something");

    // "Resume": rebuild the SAME chain against the SAME store. Content-addressed
    // puts are idempotent by hash — the warmth claim is that resume adds nothing
    // the store already holds.
    let _ = build(&chain, &TiersPolicy, 2000, 8, &mut rs).unwrap();
    assert_eq!(rs.puts() - after_first, 0, "resume re-put already-held blobs");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --bin rsry context::envelope::tests::warmth_resume_refetches_nothing_already_held`
Expected: FAIL — `RefStore::put` currently increments `puts` unconditionally, so the second build re-counts.

- [ ] **Step 3: Make `put` warmth-honest**

In `src/context/ref_store.rs`, only count a put as new work when the store didn't already hold it:

```rust
pub fn put(&mut self, bytes: &[u8]) -> Result<String> {
    let hex = crate::cas::content_hash(bytes);
    let h = hash_from_hex(&hex)?;
    if self.store.get(h).context("blobstore get (warmth check)")?.is_none() {
        self.store.put(bytes).context("blobstore put")?;
        self.puts += 1;
    }
    Ok(hex)
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --bin rsry context::` (Task 1 round-trip + warmth both green)
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/context/ref_store.rs src/context/envelope.rs
git commit -m "$(printf '%s\n\n%s\n%s' \
  '[rosary-dd5828] test(context): WARMTH gate — resume re-puts zero already-held blobs' \
  'Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>' \
  'Claude-Session: https://claude.ai/code/session_01TzQGRJiJEZr3C4MbSySDkZ')"
```

---

### Task 6: Wire the envelope into prompt building

**Files:**
- Modify: `src/dispatch/prompt.rs:47-48` (the `read_chain` → `format_for_prompt` site)
- Modify: `src/context/mod.rs` (add `pub fn build_bounded_prompt(...)` convenience that opens an `FsBlobStore` under `~/.rsry/cas`, reads config policy/budget, and returns the rendered string — keeps `prompt.rs` a one-liner)

**Interfaces:**
- Consumes: `crate::config::Config`, `crate::handoff::Handoff`, `FsBlobStore`, `ContextEnvelope::build/render`.
- Produces: `pub fn build_bounded_prompt(chain: &[Handoff], cfg: &crate::config::ContextConfig, cas_dir: &std::path::Path) -> anyhow::Result<String>`

- [ ] **Step 1: Write the failing test**

```rust
// src/context/mod.rs #[cfg(test)] mod tests
#[test]
fn build_bounded_prompt_is_under_budget() {
    let tmp = tempfile::TempDir::new().unwrap();
    let chain: Vec<crate::handoff::Handoff> =
        (0..20u32).map(|n| crate::handoff::Handoff::new_test(n, "did work")).collect();
    let cfg = crate::config::ContextConfig { policy: "tiers".into(), budget: 2000, max_refs: 8 };
    let out = build_bounded_prompt(&chain, &cfg, tmp.path()).unwrap();
    assert!(out.len() <= cfg.budget);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --bin rsry context::tests::build_bounded_prompt_is_under_budget`
Expected: FAIL — `build_bounded_prompt` not found.

- [ ] **Step 3: Implement the convenience + rewire**

```rust
// src/context/mod.rs
use std::path::Path;
pub fn build_bounded_prompt(
    chain: &[crate::handoff::Handoff],
    cfg: &crate::config::ContextConfig,
    cas_dir: &Path,
) -> anyhow::Result<String> {
    let store = ref_store::RefStore::new(leyline_core::FsBlobStore::open(cas_dir)?);
    let mut store = store;
    let policy = policy::policy_from_name(&cfg.policy);
    let env = envelope::build(chain, policy.as_ref(), cfg.budget, cfg.max_refs, &mut store)?;
    Ok(env.render())
}
```

```rust
// src/dispatch/prompt.rs — replace the format_for_prompt call site (~47-48)
let chain = crate::handoff::Handoff::read_chain(ws);
let cas_dir = /* ~/.rsry/cas */ crate::vcs::state_dir()?.join("cas");
crate::context::build_bounded_prompt(&chain, &config.context, &cas_dir)
    .unwrap_or_else(|e| {
        log::warn!("bounded prompt failed ({e}); falling back to full chain");
        crate::handoff::Handoff::format_for_prompt(&chain)
    })
```

Note: confirm `FsBlobStore::open`'s exact constructor name in `blob_store.rs:90+`; adjust if it's `new`/`with_root`. Confirm `prompt.rs` has `config` in scope (it builds from config) — if not, thread `&Config` into that function signature as part of this task.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --bin rsry context::tests::build_bounded_prompt_is_under_budget`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/context/mod.rs src/dispatch/prompt.rs
git commit -m "$(printf '%s\n\n%s\n%s' \
  '[rosary-dd5828] feat(dispatch): build the bounded envelope instead of inlining the chain' \
  'Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>' \
  'Claude-Session: https://claude.ai/code/session_01TzQGRJiJEZr3C4MbSySDkZ')"
```

---

### Task 7: `rsry_expand_ref` MCP tool

**Files:**
- Modify: `src/serve/handlers/mod.rs` (add dispatch arm near line 185 + implement `tool_expand_ref`)
- Modify: the MCP tool-schema list (the `tools()`/list function that declares `rsry_scan`'s schema — grep `"rsry_scan"` for the schema site; add `rsry_expand_ref` with one required string arg `hash`)

**Interfaces:**
- Consumes: `crate::context::ref_store::RefStore`, `FsBlobStore`, `crate::vcs::state_dir`.
- Produces: `async fn tool_expand_ref(args: &Value) -> anyhow::Result<Value>`

- [ ] **Step 1: Write the failing test**

```rust
// src/serve/handlers/mod.rs #[cfg(test)] mod tests (or a focused unit test)
#[tokio::test]
async fn expand_ref_returns_stored_blob() {
    let tmp = tempfile::TempDir::new().unwrap();
    // seed a blob through a RefStore rooted at the same cas dir the tool reads
    let mut rs = crate::context::ref_store::RefStore::new(
        leyline_core::FsBlobStore::open(tmp.path()).unwrap());
    let hash = rs.put(b"demoted phase body").unwrap();

    let args = serde_json::json!({ "hash": hash, "cas_dir": tmp.path().to_str().unwrap() });
    let out = tool_expand_ref(&args).await.unwrap();
    assert_eq!(out["content"].as_str().unwrap(), "demoted phase body");

    let miss = serde_json::json!({ "hash": "0".repeat(64), "cas_dir": tmp.path().to_str().unwrap() });
    assert!(tool_expand_ref(&miss).await.unwrap()["content"].is_null());
}
```

(`cas_dir` is a test-only override arg; in production the tool defaults to `state_dir()?.join("cas")`.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --bin rsry serve::handlers::tests::expand_ref_returns_stored_blob`
Expected: FAIL — `tool_expand_ref` not found.

- [ ] **Step 3: Implement + register**

```rust
async fn tool_expand_ref(args: &Value) -> anyhow::Result<Value> {
    let hash = args.get("hash").and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("expand_ref requires `hash`"))?;
    let cas_dir = match args.get("cas_dir").and_then(|v| v.as_str()) {
        Some(d) => std::path::PathBuf::from(d),
        None => crate::vcs::state_dir()?.join("cas"),
    };
    let rs = crate::context::ref_store::RefStore::new(
        leyline_core::FsBlobStore::open(&cas_dir)?);
    let body = rs.expand(hash)?;
    Ok(serde_json::json!({
        "content": body.map(|b| String::from_utf8_lossy(&b).to_string()),
    }))
}
```

```rust
// dispatch arm, near line 185:
"rsry_expand_ref" => tool_expand_ref(args).await,
```

Register the schema next to `rsry_scan`'s in the tool-list function: name `rsry_expand_ref`, description "Fetch a demoted context blob by its content hash", inputSchema `{ "type":"object", "properties": { "hash": {"type":"string"} }, "required":["hash"] }`.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --bin rsry serve::handlers::tests::expand_ref_returns_stored_blob`
Expected: PASS

- [ ] **Step 5: Full gate + commit**

Run: `task check`
Expected: PASS (all tests green, clippy clean, smells within ratchet)

```bash
git add src/serve/handlers/mod.rs
git commit -m "$(printf '%s\n\n%s\n%s' \
  '[rosary-dd5828] feat(mcp): rsry_expand_ref — fetch a demoted context blob by hash' \
  'Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>' \
  'Claude-Session: https://claude.ai/code/session_01TzQGRJiJEZr3C4MbSySDkZ')"
```

---

## Self-Review

**Spec coverage:**
- ContextEnvelope → Task 4. PruningPolicy trait (tiers+recency, configurable) → Tasks 2+3. RefStore over LLO BlobStore → Task 1. handoff format change (digest+refs) → Task 4 render + Task 6 wiring. expand_ref tool → Task 7. `[context]` config → Task 2. Proof-gate: round-trip → Task 1; bound → Task 4; warmth → Task 5. Errors never silent → Task 1 (verify-on-read), Task 4 (`log::warn!` on demote). Mache decoupling → structural (no mache import anywhere). ✅ all spec sections mapped.
- Continuation (Phase B) → intentionally out of scope (spec §Sequencing); no task, correct.

**Placeholder scan:** No TBD/TODO. Two "confirm exact constructor" notes (Task 1 `Hash::from`, Task 6 `FsBlobStore::open`, Task 7 schema site) are verification instructions with a named fallback, not deferred work — acceptable.

**Type consistency:** `RefStore::{put→String, expand→Option<Vec<u8>>, puts→usize}` consistent across Tasks 1/5/6/7. `PruningPolicy::hot_count(&[usize], usize)->usize` consistent Tasks 3/4. `ContextConfig{policy,budget,max_refs}` consistent Tasks 2/6. `build(chain, policy, budget, max_refs, store)` consistent Tasks 4/5/6. ✅
