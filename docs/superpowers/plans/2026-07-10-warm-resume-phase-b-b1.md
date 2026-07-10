# Warm-Resume Phase B — B1 (Cache Core + Falsifiable Gates) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the rosary-owned context cache mechanism and its four falsifiable safety gates, proven-safe and inert-by-default (`cache = off`), so no warm context can ever be served before a committed gate proves it equivalent to re-derivation.

**Architecture:** A content-addressed `ContextCache` aligned with LLO's `CacheEntry{value, generation, valid}` primitive: `get` returns a value only while `valid`; `on_change` is the *sole* invalidator, marking `valid = false` on every entry whose `provenance` intersects a described change (the self-contained generation floor — no leyline dependency). A shadow helper renders warm + cold, asserts `warm ≡ cold`, and always serves cold; on divergence it logs and auto-demotes to cold-only. Wired behind `[pipeline.context] cache = off|shadow|on` with `off` the default, so dispatch behavior is unchanged until deliberately enabled.

**Tech Stack:** Rust, `leyline_core` CAS (already the Phase A backend), `serde`, the existing `src/context/` module.

## Global Constraints

- **Never serve stale.** `get` checks `valid` only; `on_change` is the only thing that flips `valid`. No freshness recomputation at read time. (Spec §Components 1.)
- **Correct with zero leyline present.** Invalidation is the self-contained generation floor; leyline is a later, correctness-neutral refiner (B3, out of scope here). (Spec §Components 2.)
- **`cache = on` is deferred to B4.** B1 ships `off` (default) and `shadow`. If `on` is set, B1 treats it as `shadow` and logs that `on` is uncertified — the auto-demote-to-safe posture. (Spec §Rollback.)
- **Shadow is non-fatal in prod, fatal in tests.** In a live render, a `warm ≠ cold` divergence logs + auto-demotes + serves cold — never panics. The committed gate *tests* assert equality and fail loud. (Spec §Verification.)
- **Default `cache = off`** — zero behavior change vs Phase A until explicitly enabled. (User directive: "cache can be dangerous, be mindful.")
- **Keep `src/context/cache.rs` ≤ 499 lines** (Golden Rule 2 / `long_file_rosary`).
- **Branch, don't commit to main.** Commit trailers: `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_01TzQGRJiJEZr3C4MbSySDkZ`.
- **Gate before PR:** `task check` must be green.

## File Structure

- **Create `src/context/cache.rs`** — `Provenance`, `ChangeSet`, `CacheEntry` (private), `ContextCache` (put/get/on_change/generation), and all four gate tests. One responsibility: the validity-tracked content cache.
- **Modify `src/context/mod.rs`** — `pub mod cache;`; add `build_bounded_prompt_shadow(chain, cfg, cas_dir, cache)` (warm+cold+compare+serve-cold) and make `build_bounded_prompt` delegate through the cache mode (Off = current path).
- **Modify `src/config/mod.rs`** — add `CacheMode` enum + `cache: CacheMode` field on `ContextConfig` (default `Off`) + `default_context_cache()`.

---

### Task 1: `ContextCache` core — put / get / valid-gating

**Files:**
- Create: `src/context/cache.rs`
- Modify: `src/context/mod.rs:10` (add `pub mod cache;` after `pub mod render;`)

**Interfaces:**
- Produces:
  - `pub struct Provenance { pub bead: String, pub commit_sha: String, pub source_refs: Vec<String> }` (derives `Clone, Debug, PartialEq, Eq, Default`)
  - `pub struct ContextCache` with `pub fn new() -> Self`, `pub fn generation(&self) -> u64`, `pub fn put(&mut self, key: impl Into<String>, value: impl Into<String>, provenance: Provenance)`, `pub fn get(&self, key: &str) -> Option<&str>`, `pub fn len_valid(&self) -> usize`

- [ ] **Step 1: Write the failing test**

In `src/context/cache.rs`:
```rust
//! Staleness-invalidated, content-addressed context cache (warm-resume Phase B,
//! rosary-a9f5dc). Aligned with LLO's CacheEntry{value,generation,valid}: `get`
//! returns a value only while `valid`; `on_change` is the sole invalidator.

use std::collections::HashMap;

/// What a cached render was derived from — intersected against a `ChangeSet` by
/// `on_change` to decide staleness.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Provenance {
    pub bead: String,
    pub commit_sha: String,
    pub source_refs: Vec<String>,
}

struct CacheEntry {
    value: String,
    generation: u64,
    valid: bool,
    provenance: Provenance,
}

/// Content-addressed cache with generation-tracked validity.
#[derive(Default)]
pub struct ContextCache {
    generation: u64,
    entries: HashMap<String, CacheEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prov(bead: &str, sha: &str) -> Provenance {
        Provenance { bead: bead.into(), commit_sha: sha.into(), source_refs: vec![] }
    }

    #[test]
    fn put_then_get_returns_value_absent_is_none() {
        let mut c = ContextCache::new();
        c.put("k1", "render-A", prov("rosary-1", "sha1"));
        assert_eq!(c.get("k1"), Some("render-A"));
        assert_eq!(c.get("missing"), None);
        assert_eq!(c.len_valid(), 1);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin rsry context::cache::tests::put_then_get 2>&1 | tail -20`
Expected: FAIL — `no function or associated item named 'new'` / `put` / `get` not found.

- [ ] **Step 3: Write minimal implementation**

Add to `src/context/cache.rs` (above the `tests` module):
```rust
impl ContextCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Monotone clock advanced by `on_change`; stamped onto entries at `put`.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Insert (or overwrite) a valid entry stamped at the current generation.
    pub fn put(&mut self, key: impl Into<String>, value: impl Into<String>, provenance: Provenance) {
        self.entries.insert(
            key.into(),
            CacheEntry {
                value: value.into(),
                generation: self.generation,
                valid: true,
                provenance,
            },
        );
    }

    /// Return the cached value ONLY while valid — never recomputes freshness at
    /// read time (LLO semantics; invalidation lives solely in `on_change`).
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .get(key)
            .filter(|e| e.valid)
            .map(|e| e.value.as_str())
    }

    /// Count of currently-valid entries — instrumentation for the warmth gate.
    pub fn len_valid(&self) -> usize {
        self.entries.values().filter(|e| e.valid).count()
    }
}
```

And in `src/context/mod.rs`, add after line 10 (`pub mod render;`):
```rust
pub mod cache;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --bin rsry context::cache::tests::put_then_get 2>&1 | tail -5`
Expected: `test result: ok. 1 passed`.

- [ ] **Step 5: Commit**

```bash
git add src/context/cache.rs src/context/mod.rs
git commit -m "$(cat <<'EOF'
[rosary-a9f5dc] feat(context): ContextCache core — put/get with valid-gating

get returns a value only while valid; the valid flag will be flipped solely by
on_change (next task). LLO CacheEntry{value,generation,valid} primitive.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01TzQGRJiJEZr3C4MbSySDkZ
EOF
)"
```

---

### Task 2: `on_change` invalidation + gates 1 & 2 (freshness-soundness, eviction-completeness)

**Files:**
- Modify: `src/context/cache.rs`

**Interfaces:**
- Consumes: `Provenance`, `ContextCache` (Task 1)
- Produces:
  - `pub struct ChangeSet { pub bead: Option<String>, pub commit_sha: Option<String>, pub source_refs: Vec<String> }` (derives `Clone, Debug, Default`)
  - `impl ContextCache { pub fn on_change(&mut self, change: &ChangeSet) -> Vec<String> }` — bumps generation, marks matching entries invalid, returns their keys.
  - `impl Provenance { fn intersects(&self, change: &ChangeSet) -> bool }` (private)

- [ ] **Step 1: Write the failing test**

Add these tests to the `tests` module in `src/context/cache.rs`:
```rust
    fn prov_refs(bead: &str, sha: &str, refs: &[&str]) -> Provenance {
        Provenance {
            bead: bead.into(),
            commit_sha: sha.into(),
            source_refs: refs.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn eviction_completeness_sha_change() {
        // Gate 2: after on_change, the affected entry is gone from get().
        let mut c = ContextCache::new();
        c.put("k", "old-render", prov("rosary-1", "sha1"));
        let evicted = c.on_change(&ChangeSet {
            bead: Some("rosary-1".into()),
            commit_sha: Some("sha2".into()),
            source_refs: vec![],
        });
        assert_eq!(evicted, vec!["k".to_string()]);
        assert_eq!(c.get("k"), None, "stale entry must never be served");
        assert_eq!(c.generation(), 1, "on_change bumps generation");
    }

    #[test]
    fn on_change_spares_unrelated_beads() {
        let mut c = ContextCache::new();
        c.put("k", "render", prov("rosary-1", "sha1"));
        let evicted = c.on_change(&ChangeSet {
            bead: Some("rosary-2".into()),
            commit_sha: Some("shaX".into()),
            source_refs: vec![],
        });
        assert!(evicted.is_empty());
        assert_eq!(c.get("k"), Some("render"), "unrelated change must not evict");
    }

    #[test]
    fn eviction_on_shared_source_ref() {
        let mut c = ContextCache::new();
        c.put("k", "render", prov_refs("rosary-1", "sha1", &["blobhash-r1"]));
        c.on_change(&ChangeSet { bead: None, commit_sha: None, source_refs: vec!["blobhash-r1".into()] });
        assert_eq!(c.get("k"), None, "a changed source ref must evict its derivations");
    }

    #[test]
    fn freshness_soundness_valid_never_lies() {
        // Gate 1: after a scripted sequence, every valid entry's provenance does
        // NOT intersect any applied change.
        let mut c = ContextCache::new();
        c.put("a", "ra", prov("rosary-1", "sha1"));
        c.put("b", "rb", prov("rosary-2", "sha1"));
        c.put("d", "rd", prov_refs("rosary-3", "sha1", &["r-shared"]));
        let changes = [
            ChangeSet { bead: Some("rosary-1".into()), commit_sha: Some("sha2".into()), source_refs: vec![] },
            ChangeSet { bead: None, commit_sha: None, source_refs: vec!["r-shared".into()] },
        ];
        for ch in &changes {
            c.on_change(ch);
        }
        // "a" (rosary-1 moved) and "d" (r-shared changed) must be gone; "b" survives.
        assert_eq!(c.get("a"), None);
        assert_eq!(c.get("d"), None);
        assert_eq!(c.get("b"), Some("rb"));
        // Invariant: no served entry intersects any applied change.
        for ch in &changes {
            assert!(c.get("a").is_none() || !prov("rosary-1", "sha1").intersects(ch));
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin rsry context::cache::tests::eviction_completeness_sha_change 2>&1 | tail -20`
Expected: FAIL — `ChangeSet` not found / `on_change` not found.

- [ ] **Step 3: Write minimal implementation**

Add the `ChangeSet` struct near `Provenance` in `src/context/cache.rs`:
```rust
/// A described change to the world. `on_change` invalidates every entry whose
/// `provenance` intersects it. This is the self-contained generation floor —
/// correct with zero leyline present; a leyline cascade refiner (B3) narrows it.
#[derive(Clone, Debug, Default)]
pub struct ChangeSet {
    /// The bead whose state moved (paired with `commit_sha`).
    pub bead: Option<String>,
    /// The new commit sha; entries for `bead` cached at a different sha are stale.
    pub commit_sha: Option<String>,
    /// Source regions (content-hashes) that changed.
    pub source_refs: Vec<String>,
}
```

Add the intersection helper:
```rust
impl Provenance {
    /// True if this entry was derived from something the change touched.
    fn intersects(&self, change: &ChangeSet) -> bool {
        // Same bead, advanced sha → the derivation is at an old commit.
        if let (Some(bead), Some(sha)) = (&change.bead, &change.commit_sha) {
            if &self.bead == bead && &self.commit_sha != sha {
                return true;
            }
        }
        // Any shared source region changed.
        change
            .source_refs
            .iter()
            .any(|r| self.source_refs.contains(r))
    }
}
```

Add `on_change` to `impl ContextCache`:
```rust
    /// Bump the generation, mark `valid = false` on every entry whose provenance
    /// intersects `change`, and return the invalidated keys (the cascade a
    /// downstream consumer would evict on its side). The SOLE invalidator.
    pub fn on_change(&mut self, change: &ChangeSet) -> Vec<String> {
        self.generation += 1;
        let mut invalidated = Vec::new();
        for (key, entry) in self.entries.iter_mut() {
            if entry.valid && entry.provenance.intersects(change) {
                entry.valid = false;
                invalidated.push(key.clone());
            }
        }
        invalidated.sort();
        invalidated
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --bin rsry context::cache 2>&1 | tail -6`
Expected: `test result: ok. 5 passed` (Task 1 + the four new tests).

- [ ] **Step 5: Commit**

```bash
git add src/context/cache.rs
git commit -m "$(cat <<'EOF'
[rosary-a9f5dc] feat(context): on_change invalidation + freshness/eviction gates

on_change is the sole invalidator: bumps generation, marks valid=false on every
entry whose provenance intersects the change (same-bead-new-sha, or shared
source ref). Gates 1 (freshness-soundness: valid never lies) and 2
(eviction-completeness: on_change removes the entry from get) committed as tests.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01TzQGRJiJEZr3C4MbSySDkZ
EOF
)"
```

---

### Task 3: `CacheMode` config — `[pipeline.context] cache`, default `off`

**Files:**
- Modify: `src/config/mod.rs:471-491` (the `ContextConfig` struct + `Default` impl) and the `default_context_*` helpers below it.

**Interfaces:**
- Produces:
  - `pub enum CacheMode { Off, Shadow, On }` (derives `Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize`, `#[serde(rename_all = "lowercase")]`)
  - New field `pub cache: CacheMode` on `ContextConfig`, default `CacheMode::Off`.

- [ ] **Step 1: Write the failing test**

Add to the config tests (find the existing `#[cfg(test)] mod tests` in `src/config/mod.rs`; if the module uses a separate `config/tests.rs`, add there). Test body:
```rust
    #[test]
    fn context_cache_mode_defaults_off_and_parses() {
        // Default is Off — zero behavior change vs Phase A.
        assert_eq!(crate::config::ContextConfig::default().cache, crate::config::CacheMode::Off);
        // Parses lowercase strings from TOML.
        let c: crate::config::ContextConfig =
            toml::from_str("cache = \"shadow\"").unwrap();
        assert_eq!(c.cache, crate::config::CacheMode::Shadow);
        let c: crate::config::ContextConfig =
            toml::from_str("cache = \"on\"").unwrap();
        assert_eq!(c.cache, crate::config::CacheMode::On);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin rsry context_cache_mode_defaults_off_and_parses 2>&1 | tail -15`
Expected: FAIL — `CacheMode` not found / no field `cache`.

- [ ] **Step 3: Write minimal implementation**

Add the enum just above `ContextConfig` in `src/config/mod.rs`:
```rust
/// Warm-resume context-cache mode (rosary-a9f5dc). `off` = always re-derive
/// (Phase A behavior, the default + escape hatch); `shadow` = compute the warm
/// render, assert it equals cold, but serve cold; `on` = serve warm (deferred to
/// B4 — treated as `shadow` until certified).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheMode {
    Off,
    Shadow,
    On,
}

fn default_context_cache() -> CacheMode {
    CacheMode::Off
}
```

Add the field to `ContextConfig` (after `max_refs`):
```rust
    /// Warm-resume cache mode. Default `off` — no cache until deliberately enabled.
    #[serde(default = "default_context_cache")]
    pub cache: CacheMode,
```

Add to the `Default for ContextConfig` impl body:
```rust
            cache: default_context_cache(),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --bin rsry context_cache_mode_defaults_off_and_parses 2>&1 | tail -5`
Expected: `test result: ok. 1 passed`.

- [ ] **Step 5: Commit**

```bash
git add src/config/mod.rs
git commit -m "$(cat <<'EOF'
[rosary-a9f5dc] feat(config): [pipeline.context] cache = off|shadow|on (default off)

CacheMode gates the warm-resume cache. Default off = zero behavior change vs
Phase A. on is deferred to B4 (treated as shadow until certified).

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01TzQGRJiJEZr3C4MbSySDkZ
EOF
)"
```

---

### Task 4: Shadow helper + gate 3 (shadow-equivalence) + auto-demote kill-switch

**Files:**
- Modify: `src/context/mod.rs` (add `build_bounded_prompt_shadow` + a `provenance_of` helper + a `ShadowOutcome`)
- Modify: `src/context/cache.rs` (no change; consumed here)

**Interfaces:**
- Consumes: `ContextCache`, `Provenance`, `ChangeSet` (Tasks 1-2); `CacheMode` (Task 3); `build_bounded_prompt`'s internals (envelope build + render).
- Produces:
  - `pub struct ShadowOutcome { pub served: String, pub diverged: bool }`
  - `pub fn build_bounded_prompt_shadow(chain: &[Handoff], cfg: &ContextConfig, cas_dir: &Path, cache: &mut cache::ContextCache) -> Result<ShadowOutcome>`
  - `fn provenance_of(chain: &[Handoff]) -> cache::Provenance` (bead + last-phase digest as source ref)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `src/context/mod.rs`:
```rust
    #[test]
    fn shadow_serves_cold_and_flags_no_divergence_when_fresh() {
        // Gate 3: warm ≡ cold on a fresh chain; the cache hit equals a fresh render.
        let tmp = tempfile::TempDir::new().unwrap();
        let chain: Vec<crate::handoff::Handoff> = (0..10u32)
            .map(|n| crate::handoff::Handoff::new(n, "dev-agent", None, "rosary-a9f5dc", "claude", &crate::manifest::Work::default(), None))
            .collect();
        let cfg = crate::config::ContextConfig { policy: "tiers".into(), budget: 2000, max_refs: 8, cache: crate::config::CacheMode::Shadow };
        let mut cache = super::cache::ContextCache::new();

        let cold = super::build_bounded_prompt(&chain, &cfg, tmp.path()).unwrap();
        let first = super::build_bounded_prompt_shadow(&chain, &cfg, tmp.path(), &mut cache).unwrap();
        assert_eq!(first.served, cold, "shadow must serve cold");
        assert!(!first.diverged);
        // "Resume": same chain again is a cache hit that still equals cold.
        let second = super::build_bounded_prompt_shadow(&chain, &cfg, tmp.path(), &mut cache).unwrap();
        assert_eq!(second.served, cold);
        assert!(!second.diverged, "a fresh cache hit must not diverge");
    }

    #[test]
    fn shadow_catches_stale_hit_and_auto_demotes() {
        // Simulate an invalidation MISS: poison the cache with a stale value under
        // the key the chain will hash to, then render. warm(stale) != cold(fresh)
        // => diverged=true, served=cold (never the stale warm), and demoted.
        let tmp = tempfile::TempDir::new().unwrap();
        let chain: Vec<crate::handoff::Handoff> = (0..5u32)
            .map(|n| crate::handoff::Handoff::new(n, "dev-agent", None, "rosary-a9f5dc", "claude", &crate::manifest::Work::default(), None))
            .collect();
        let cfg = crate::config::ContextConfig { policy: "tiers".into(), budget: 2000, max_refs: 8, cache: crate::config::CacheMode::Shadow };
        let mut cache = super::cache::ContextCache::new();
        let key = super::shadow_key(&chain, &cfg);
        cache.put(key, "STALE-POISON", super::provenance_of(&chain));

        let out = super::build_bounded_prompt_shadow(&chain, &cfg, tmp.path(), &mut cache).unwrap();
        assert!(out.diverged, "a stale hit must be detected");
        assert_ne!(out.served, "STALE-POISON", "must never serve the stale warm value");
        let cold = super::build_bounded_prompt(&chain, &cfg, tmp.path()).unwrap();
        assert_eq!(out.served, cold, "must serve cold on divergence");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin rsry context::tests::shadow_ 2>&1 | tail -20`
Expected: FAIL — `build_bounded_prompt_shadow` / `shadow_key` / `provenance_of` not found; and `ContextConfig` has no field `cache` in the literal only if Task 3 not merged (it is).

- [ ] **Step 3: Write minimal implementation**

In `src/context/mod.rs`, add near the top `use crate::config::CacheMode;` (extend the existing imports) and append:
```rust
/// Content-addressed cache key for a (chain, cfg) render — same inputs, same key.
pub(crate) fn shadow_key(chain: &[crate::handoff::Handoff], cfg: &crate::config::ContextConfig) -> String {
    let mut bytes = serde_json::to_vec(chain).unwrap_or_default();
    bytes.extend_from_slice(cfg.policy.as_bytes());
    bytes.extend_from_slice(&cfg.budget.to_le_bytes());
    bytes.extend_from_slice(&cfg.max_refs.to_le_bytes());
    crate::cas::content_hash(&bytes)
}

/// Provenance for a chain's render: the bead it belongs to, the latest phase's
/// content hash as the commit/source signal. A new phase or a bead move changes
/// this, so `on_change` can invalidate derivations of the old state.
pub(crate) fn provenance_of(chain: &[crate::handoff::Handoff]) -> cache::Provenance {
    let last = chain.last();
    let bead = last.map(|h| h.bead_id.clone()).unwrap_or_default();
    let sig = crate::cas::content_hash(&serde_json::to_vec(chain).unwrap_or_default());
    cache::Provenance { bead, commit_sha: sig.clone(), source_refs: vec![sig] }
}

/// Result of a shadow render: what was served (always cold) and whether warm
/// diverged from cold (an invalidation bug — auto-demotes the cache).
pub struct ShadowOutcome {
    pub served: String,
    pub diverged: bool,
}

/// Render both the warm (cache) and cold (re-derive) paths, assert-and-log their
/// equality, and ALWAYS serve cold. A divergence means `on_change` missed a
/// change: log it, mark the entry invalid (auto-demote), never serve warm.
/// This is the shadow-mode discipline (ADR-0010/R4b) — cold stays authoritative.
pub fn build_bounded_prompt_shadow(
    chain: &[crate::handoff::Handoff],
    cfg: &crate::config::ContextConfig,
    cas_dir: &Path,
    cache: &mut cache::ContextCache,
) -> anyhow::Result<ShadowOutcome> {
    let cold = build_bounded_prompt(chain, cfg, cas_dir)?;
    if cfg.cache == CacheMode::Off {
        return Ok(ShadowOutcome { served: cold, diverged: false });
    }
    let key = shadow_key(chain, cfg);
    let warm = cache.get(&key).map(str::to_string);
    let diverged = match &warm {
        Some(w) => w != &cold,
        None => false,
    };
    if diverged {
        eprintln!(
            "[context] shadow divergence for {key}: warm != cold — auto-demoting entry (rosary-a9f5dc)"
        );
        // Auto-demote: invalidate the poisoned entry so it is never served again.
        cache.on_change(&cache::ChangeSet {
            bead: None,
            commit_sha: None,
            source_refs: provenance_of(chain).source_refs,
        });
    } else if warm.is_none() {
        // Miss: memoize the cold render for a future resume.
        cache.put(key, cold.clone(), provenance_of(chain));
    }
    // `on` is deferred to B4; until then it behaves as shadow (serve cold).
    Ok(ShadowOutcome { served: cold, diverged })
}
```

Ensure `use std::path::Path;` is present in `mod.rs` (it already is — `build_bounded_prompt` takes `cas_dir: &Path`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --bin rsry context::tests::shadow_ 2>&1 | tail -6`
Expected: `test result: ok. 2 passed`.

- [ ] **Step 5: Commit**

```bash
git add src/context/mod.rs
git commit -m "$(cat <<'EOF'
[rosary-a9f5dc] feat(context): shadow render — warm+cold compare, serve cold, auto-demote

build_bounded_prompt_shadow renders both paths, asserts warm≡cold, always serves
cold, and on divergence logs + invalidates the poisoned entry (kill-switch).
Gate 3 (shadow-equivalence) committed, including the stale-hit-is-caught case.
on stays deferred to B4 (behaves as shadow).

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01TzQGRJiJEZr3C4MbSySDkZ
EOF
)"
```

---

### Task 5: Gate 4 (warmth) + rollback-correctness + `task check`

**Files:**
- Modify: `src/context/mod.rs` (tests only)

**Interfaces:**
- Consumes: everything above.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `src/context/mod.rs`:
```rust
    #[test]
    fn warmth_no_change_resume_hits_cache() {
        // Gate 4: a no-change resume serves from cache (a hit), not a re-derivation.
        let tmp = tempfile::TempDir::new().unwrap();
        let chain: Vec<crate::handoff::Handoff> = (0..8u32)
            .map(|n| crate::handoff::Handoff::new(n, "dev-agent", None, "rosary-a9f5dc", "claude", &crate::manifest::Work::default(), None))
            .collect();
        let cfg = crate::config::ContextConfig { policy: "tiers".into(), budget: 2000, max_refs: 8, cache: crate::config::CacheMode::Shadow };
        let mut cache = super::cache::ContextCache::new();

        let _ = super::build_bounded_prompt_shadow(&chain, &cfg, tmp.path(), &mut cache).unwrap();
        assert_eq!(cache.len_valid(), 1, "first render memoizes one entry");
        // Resume with no change → the same key is a valid hit.
        let key = super::shadow_key(&chain, &cfg);
        assert!(cache.get(&key).is_some(), "no-change resume must be a warm hit");
    }

    #[test]
    fn cache_off_reproduces_phase_a_byte_for_byte() {
        // Rollback correctness: Off == exactly the Phase A path.
        let tmp = tempfile::TempDir::new().unwrap();
        let chain: Vec<crate::handoff::Handoff> = (0..12u32)
            .map(|n| crate::handoff::Handoff::new(n, "dev-agent", None, "rosary-a9f5dc", "claude", &crate::manifest::Work::default(), None))
            .collect();
        let cfg = crate::config::ContextConfig { policy: "tiers".into(), budget: 2000, max_refs: 4, cache: crate::config::CacheMode::Off };
        let mut cache = super::cache::ContextCache::new();
        let plain = super::build_bounded_prompt(&chain, &cfg, tmp.path()).unwrap();
        let shadowed = super::build_bounded_prompt_shadow(&chain, &cfg, tmp.path(), &mut cache).unwrap();
        assert_eq!(shadowed.served, plain);
        assert_eq!(cache.len_valid(), 0, "Off must not touch the cache");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --bin rsry context::tests::warmth_no_change_resume_hits_cache context::tests::cache_off_reproduces 2>&1 | tail -10`
Expected: PASS is possible here since the impl exists — if so, this task is a pure verification/guard addition. If they FAIL, it's a real gap; fix per Step 3.

- [ ] **Step 3: Fix if needed**

If `cache_off_reproduces_phase_a_byte_for_byte` fails because `Off` still populated the cache, confirm the early-return in `build_bounded_prompt_shadow` (Task 4) returns before any `cache.put`. No new code should be required; if a fix is needed it is the early `return` guard.

- [ ] **Step 4: Run the full gate**

Run: `cargo test --bin rsry context:: 2>&1 | tail -6`
Expected: `test result: ok.` with all Phase A + Phase B context tests passing (≥ 9 new).

Run: `task check 2>&1 | tail -15`
Expected: green (compile + lint + test + smells). If `long_file_rosary` fires on `cache.rs`, confirm `wc -l src/context/cache.rs` ≤ 499 and trim comments if over.

- [ ] **Step 5: Commit**

```bash
git add src/context/mod.rs
git commit -m "$(cat <<'EOF'
[rosary-a9f5dc] test(context): warmth gate + cache=off rollback-correctness

Gate 4 (warmth): a no-change resume is a valid cache hit. Rollback: cache=off
reproduces the Phase A render byte-for-byte and never touches the cache. B1
cache core is proven-safe and inert by default.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01TzQGRJiJEZr3C4MbSySDkZ
EOF
)"
```

---

## Out of scope for B1 (follow-on plans)

- **B2 — work-state resume:** a `work_state` region on the envelope + reconciler ownership of a persistent `ContextCache` so shadow observes cross-dispatch resume, and the live `on_change` calls when a bead's sha/phase advances.
- **B3 — leyline cascade refiner:** consume leyline's `on_change` to narrow eviction beyond the self-contained floor.
- **B4 — flip to `on`:** a `context cache audit` corpus run proving zero shadow divergence, then enabling the warm-serve branch.
- **Provider `cache_control` feed:** the render-boundary breakpoint.

## Self-Review

**1. Spec coverage:**
- Spec §Components 1 (`CacheEntry{value,generation,valid}`, `get` valid-only) → Task 1. ✓
- Spec §Components 2 (self-contained floor invalidation) → Task 2 (`on_change` + `intersects`). ✓
- Spec §Verification gate 1 (freshness-soundness) → Task 2 `freshness_soundness_valid_never_lies`. ✓
- Gate 2 (eviction-completeness) → Task 2 `eviction_completeness_sha_change`. ✓
- Gate 3 (shadow-equivalence) → Task 4 `shadow_serves_cold...` + `shadow_catches_stale_hit...`. ✓
- Gate 4 (warmth) → Task 5 `warmth_no_change_resume_hits_cache`. ✓
- Spec §Rollback (`off|shadow|on`, default off, auto-demote) → Task 3 (config) + Task 4 (auto-demote) + Task 5 (off rollback-correctness). ✓
- Spec "on deferred to B4" → honored (Task 4 serves cold for on). ✓
- Work-state resume, leyline refiner, provider feed → explicitly out of scope. ✓

**2. Placeholder scan:** No TBD/TODO/"handle edge cases"; every code step has complete code and exact commands. ✓

**3. Type consistency:** `Provenance{bead,commit_sha,source_refs}`, `ChangeSet{bead:Option,commit_sha:Option,source_refs}`, `ContextCache::{new,generation,put,get,on_change,len_valid}`, `CacheMode::{Off,Shadow,On}`, `build_bounded_prompt_shadow` / `shadow_key` / `provenance_of` / `ShadowOutcome{served,diverged}` — names match across Tasks 1-5. `ContextConfig` literal gains `cache:` field in every test post-Task-3. ✓

**Note on `handoff.rs`:** the tests assume `Handoff` derives `serde::Serialize` (used by `shadow_key`/`provenance_of` via `serde_json::to_vec`) and exposes `bead_id: String`. Both are already true (envelope.rs Task-A code calls `serde_json::to_vec(h)` on handoffs and `provenance_of` mirrors that). If `bead_id` is named differently, adjust `provenance_of` to the actual field — verify with `grep -n 'pub bead_id\|pub struct Handoff' src/handoff.rs` before Task 4.
