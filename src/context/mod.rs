//! Bounded, content-addressed pipeline context — warm-resume (rosary-dd5828).
//!
//! "No context bloat" and "warm resume" are one mechanism: the pipeline carries
//! a bounded working set plus content-addressed *references* to demoted material
//! instead of inlining the whole handoff chain into every phase's prompt.

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
        };
        let out = super::build_bounded_prompt(&chain, &cfg, tmp.path()).unwrap();
        assert!(out.len() <= cfg.budget);
    }
}
