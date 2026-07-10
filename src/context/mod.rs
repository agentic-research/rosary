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
}
