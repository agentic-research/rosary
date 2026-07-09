//! Deterministic pruning policies: how many of the most-recent pipeline phases
//! stay "hot" (rendered in full) vs are demoted to CAS refs. Deterministic by
//! construction so the bounded-context proof-gate stays reproducible in CI.

/// Cheap, deterministic token estimate (~4 chars/token).
pub fn estimate_tokens(s: &str) -> usize {
    s.len().div_ceil(4)
}

pub trait PruningPolicy {
    /// Given per-phase sizes (oldest→newest) and a budget, return how many of
    /// the MOST RECENT phases stay hot. Deterministic.
    fn hot_count(&self, phase_sizes: &[usize], budget: usize) -> usize;
}

/// Current phase hot, everything older demoted. Dead simple, deterministic.
pub struct TiersPolicy;
impl PruningPolicy for TiersPolicy {
    fn hot_count(&self, phase_sizes: &[usize], _budget: usize) -> usize {
        if phase_sizes.is_empty() { 0 } else { 1 }
    }
}

/// Keep the most-recent phases whose cumulative size fits `budget` (always at
/// least the current phase). Deterministic.
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

/// Resolve a config policy name to a policy. Unknown → `tiers` + a warning, so
/// a typo never silently changes behavior.
pub fn policy_from_name(name: &str) -> Box<dyn PruningPolicy> {
    match name {
        "recency" => Box::new(RecencyPolicy),
        "tiers" => Box::new(TiersPolicy),
        other => {
            eprintln!("[context] unknown context.policy '{other}', defaulting to tiers");
            Box::new(TiersPolicy)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_keeps_only_current_hot() {
        let sizes = vec![100, 100, 100];
        assert_eq!(TiersPolicy.hot_count(&sizes, 10_000), 1);
        assert_eq!(TiersPolicy.hot_count(&[], 10_000), 0);
    }

    #[test]
    fn recency_keeps_most_recent_that_fit() {
        let sizes = vec![100, 100, 100];
        assert_eq!(RecencyPolicy.hot_count(&sizes, 250), 2);
        assert_eq!(RecencyPolicy.hot_count(&sizes, 10_000), 3);
        assert_eq!(RecencyPolicy.hot_count(&sizes, 1), 1);
    }

    #[test]
    fn policy_from_name_is_total() {
        assert_eq!(policy_from_name("tiers").hot_count(&[9, 9], 10_000), 1);
        assert_eq!(policy_from_name("recency").hot_count(&[9, 9], 10_000), 2);
        assert_eq!(policy_from_name("nonsense").hot_count(&[9, 9], 10_000), 1);
    }
}
