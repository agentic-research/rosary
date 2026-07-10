//! Bounded, content-addressed pipeline context — warm-resume (rosary-dd5828).
//!
//! "No context bloat" and "warm resume" are one mechanism: the pipeline carries
//! a bounded working set plus content-addressed *references* to demoted material
//! instead of inlining the whole handoff chain into every phase's prompt.

pub mod cache;
pub mod envelope;
pub mod policy;
pub mod ref_store;
pub mod render;

use std::path::Path;

use crate::config::CacheMode;

/// Render a handoff chain into a bounded, content-addressed prompt section.
/// The provable path: prunes per `cfg.policy`/`budget`, demotes older phases to
/// CAS refs under `cas_dir`, and returns a render that stays under `cfg.budget`.
pub fn build_bounded_prompt(
    chain: &[crate::handoff::Handoff],
    cfg: &crate::config::ContextConfig,
    cas_dir: &Path,
) -> anyhow::Result<String> {
    let mut store = ref_store::RefStore::new(leyline_core::FsBlobStore::open(cas_dir)?);
    let policy = policy::policy_from_name(&cfg.policy);
    let env = envelope::build(chain, policy.as_ref(), cfg.budget, cfg.max_refs, &mut store)?;
    Ok(env.render())
}

/// Content-addressed cache key for a (chain, cfg) render — same inputs, same key.
#[allow(dead_code)] // wired live in B2 (rosary-a9f5dc)
pub(crate) fn shadow_key(
    chain: &[crate::handoff::Handoff],
    cfg: &crate::config::ContextConfig,
) -> String {
    let mut bytes = serde_json::to_vec(chain).unwrap_or_default();
    bytes.extend_from_slice(cfg.policy.as_bytes());
    bytes.extend_from_slice(&cfg.budget.to_le_bytes());
    bytes.extend_from_slice(&cfg.max_refs.to_le_bytes());
    crate::cas::content_hash(&bytes)
}

/// Provenance for a chain's render: the bead it belongs to, the latest phase's
/// content hash as the commit/source signal. A new phase or a bead move changes
/// this, so `on_change` can invalidate derivations of the old state.
#[allow(dead_code)] // wired live in B2 (rosary-a9f5dc)
pub(crate) fn provenance_of(chain: &[crate::handoff::Handoff]) -> cache::Provenance {
    let last = chain.last();
    let bead = last.map(|h| h.bead_id.clone()).unwrap_or_default();
    let sig = crate::cas::content_hash(&serde_json::to_vec(chain).unwrap_or_default());
    cache::Provenance {
        bead,
        commit_sha: sig.clone(),
        source_refs: vec![sig],
    }
}

/// Result of a shadow render: what was served (always cold) and whether warm
/// diverged from cold (an invalidation bug — auto-demotes the cache).
#[allow(dead_code)] // wired live in B2 (rosary-a9f5dc)
pub struct ShadowOutcome {
    pub served: String,
    pub diverged: bool,
}

/// Render both the warm (cache) and cold (re-derive) paths, assert-and-log their
/// equality, and ALWAYS serve cold. With full-chain content keying a changed
/// chain yields a new key (a miss), so a same-key divergence means the cached
/// render no longer matches a fresh re-derivation for this exact chain — CAS
/// state drifted or the render turned non-deterministic (or the entry was
/// poisoned): log it, invalidate the entry (auto-demote), never serve warm.
/// This is the shadow-mode discipline (ADR-0010/R4b) — cold stays authoritative.
#[allow(dead_code)] // wired live in B2 (rosary-a9f5dc)
pub fn build_bounded_prompt_shadow(
    chain: &[crate::handoff::Handoff],
    cfg: &crate::config::ContextConfig,
    cas_dir: &Path,
    cache: &mut cache::ContextCache,
) -> anyhow::Result<ShadowOutcome> {
    let cold = build_bounded_prompt(chain, cfg, cas_dir)?;
    if cfg.cache == CacheMode::Off {
        return Ok(ShadowOutcome {
            served: cold,
            diverged: false,
        });
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
    Ok(ShadowOutcome {
        served: cold,
        diverged,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn build_bounded_prompt_is_under_budget() {
        let tmp = tempfile::TempDir::new().unwrap();
        let chain: Vec<crate::handoff::Handoff> = (0..20u32)
            .map(|n| {
                crate::handoff::Handoff::new(
                    n,
                    "dev-agent",
                    None,
                    "rosary-test",
                    "claude",
                    &crate::manifest::Work::default(),
                    None,
                )
            })
            .collect();
        let cfg = crate::config::ContextConfig {
            policy: "tiers".into(),
            budget: 2000,
            max_refs: 8,
            cache: crate::config::CacheMode::Off,
        };
        let out = super::build_bounded_prompt(&chain, &cfg, tmp.path()).unwrap();
        assert!(out.len() <= cfg.budget);
    }

    /// End-to-end dogfood of the real on-disk path a dispatch uses (rosary-dd5828):
    /// a deep handoff chain → `FsBlobStore` on disk → bounded render → warm resume.
    /// Beyond the unit tests (which use MemBlobStore), this asserts demoted phases
    /// actually persist to the CAS on disk (git-style sharded layout) and that a
    /// resume against the same CAS re-writes nothing.
    #[test]
    fn warm_resume_fs_end_to_end() {
        fn count_blobs(dir: &std::path::Path) -> usize {
            std::fs::read_dir(dir)
                .map(|rd| {
                    rd.flatten()
                        .map(|e| {
                            let p = e.path();
                            if p.is_dir() { count_blobs(&p) } else { 1 }
                        })
                        .sum()
                })
                .unwrap_or(0)
        }

        let tmp = tempfile::TempDir::new().unwrap();
        let cas = tmp.path();
        let chain: Vec<crate::handoff::Handoff> = (0..20u32)
            .map(|n| {
                crate::handoff::Handoff::new(
                    n,
                    "dev-agent",
                    None,
                    "rosary-dogfood",
                    "claude",
                    &crate::manifest::Work::default(),
                    None,
                )
            })
            .collect();
        let cfg = crate::config::ContextConfig {
            policy: "tiers".into(),
            budget: 2000,
            max_refs: 4,
            cache: crate::config::CacheMode::Off,
        };

        let prompt = super::build_bounded_prompt(&chain, &cfg, cas).unwrap();
        let blobs = count_blobs(cas);

        assert!(prompt.len() <= cfg.budget, "render exceeded budget");
        assert!(prompt.contains("Phase 19"), "the current phase must be hot");
        assert!(
            prompt.contains("Earlier context"),
            "older phases must demote to CAS refs"
        );
        assert!(
            blobs > 0,
            "demoted handoffs must persist to the CAS on disk (git-style shards)"
        );

        // Warm resume: re-render the same chain against the same on-disk CAS —
        // content-addressed puts are idempotent, so no new blobs are written.
        let _ = super::build_bounded_prompt(&chain, &cfg, cas).unwrap();
        assert_eq!(
            count_blobs(cas),
            blobs,
            "resume must not re-write already-held blobs"
        );
    }

    #[test]
    fn shadow_serves_cold_and_flags_no_divergence_when_fresh() {
        // Gate 3: warm ≡ cold on a fresh chain; the cache hit equals a fresh render.
        let tmp = tempfile::TempDir::new().unwrap();
        let chain: Vec<crate::handoff::Handoff> = (0..10u32)
            .map(|n| {
                crate::handoff::Handoff::new(
                    n,
                    "dev-agent",
                    None,
                    "rosary-a9f5dc",
                    "claude",
                    &crate::manifest::Work::default(),
                    None,
                )
            })
            .collect();
        let cfg = crate::config::ContextConfig {
            policy: "tiers".into(),
            budget: 2000,
            max_refs: 8,
            cache: crate::config::CacheMode::Shadow,
        };
        let mut cache = super::cache::ContextCache::new();

        let cold = super::build_bounded_prompt(&chain, &cfg, tmp.path()).unwrap();
        let first =
            super::build_bounded_prompt_shadow(&chain, &cfg, tmp.path(), &mut cache).unwrap();
        assert_eq!(first.served, cold, "shadow must serve cold");
        assert!(!first.diverged);
        // "Resume": same chain again is a cache hit that still equals cold.
        let second =
            super::build_bounded_prompt_shadow(&chain, &cfg, tmp.path(), &mut cache).unwrap();
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
            .map(|n| {
                crate::handoff::Handoff::new(
                    n,
                    "dev-agent",
                    None,
                    "rosary-a9f5dc",
                    "claude",
                    &crate::manifest::Work::default(),
                    None,
                )
            })
            .collect();
        let cfg = crate::config::ContextConfig {
            policy: "tiers".into(),
            budget: 2000,
            max_refs: 8,
            cache: crate::config::CacheMode::Shadow,
        };
        let mut cache = super::cache::ContextCache::new();
        let key = super::shadow_key(&chain, &cfg);
        cache.put(key, "STALE-POISON", super::provenance_of(&chain));

        let out = super::build_bounded_prompt_shadow(&chain, &cfg, tmp.path(), &mut cache).unwrap();
        assert!(out.diverged, "a stale hit must be detected");
        assert_ne!(
            out.served, "STALE-POISON",
            "must never serve the stale warm value"
        );
        let cold = super::build_bounded_prompt(&chain, &cfg, tmp.path()).unwrap();
        assert_eq!(out.served, cold, "must serve cold on divergence");
    }

    #[test]
    fn warmth_no_change_resume_hits_cache() {
        // Gate 4: a no-change resume serves from cache (a hit), not a re-derivation.
        let tmp = tempfile::TempDir::new().unwrap();
        let chain: Vec<crate::handoff::Handoff> = (0..8u32)
            .map(|n| {
                crate::handoff::Handoff::new(
                    n,
                    "dev-agent",
                    None,
                    "rosary-a9f5dc",
                    "claude",
                    &crate::manifest::Work::default(),
                    None,
                )
            })
            .collect();
        let cfg = crate::config::ContextConfig {
            policy: "tiers".into(),
            budget: 2000,
            max_refs: 8,
            cache: crate::config::CacheMode::Shadow,
        };
        let mut cache = super::cache::ContextCache::new();

        let _ = super::build_bounded_prompt_shadow(&chain, &cfg, tmp.path(), &mut cache).unwrap();
        assert_eq!(cache.len_valid(), 1, "first render memoizes one entry");
        // Resume with no change → the same key is a valid hit.
        let key = super::shadow_key(&chain, &cfg);
        assert!(
            cache.get(&key).is_some(),
            "no-change resume must be a warm hit"
        );
    }

    #[test]
    fn cache_off_reproduces_phase_a_byte_for_byte() {
        // Rollback correctness: Off == exactly the Phase A path.
        let tmp = tempfile::TempDir::new().unwrap();
        let chain: Vec<crate::handoff::Handoff> = (0..12u32)
            .map(|n| {
                crate::handoff::Handoff::new(
                    n,
                    "dev-agent",
                    None,
                    "rosary-a9f5dc",
                    "claude",
                    &crate::manifest::Work::default(),
                    None,
                )
            })
            .collect();
        let cfg = crate::config::ContextConfig {
            policy: "tiers".into(),
            budget: 2000,
            max_refs: 4,
            cache: crate::config::CacheMode::Off,
        };
        let mut cache = super::cache::ContextCache::new();
        let plain = super::build_bounded_prompt(&chain, &cfg, tmp.path()).unwrap();
        let shadowed =
            super::build_bounded_prompt_shadow(&chain, &cfg, tmp.path(), &mut cache).unwrap();
        assert_eq!(shadowed.served, plain);
        assert_eq!(cache.len_valid(), 0, "Off must not touch the cache");
    }
}
