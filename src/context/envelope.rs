//! Bounded, content-addressed view of the handoff chain. Recent phases render
//! hot; older ones demote to CAS refs (older-than-`max_refs` roll up into one
//! blob); the rendered whole stays under `budget`.

use anyhow::Result;
use leyline_core::BlobStore;

use super::policy::{PruningPolicy, estimate_tokens};
use super::ref_store::RefStore;
use crate::handoff::Handoff;

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

/// Build the bounded envelope: partition the chain into hot (rendered) vs
/// demoted (CAS refs) per `policy`, cap inline refs at `max_refs` (older roll up
/// into one blob), and enforce `render().len() <= budget`.
pub fn build<B: BlobStore>(
    chain: &[Handoff],
    policy: &dyn PruningPolicy,
    budget: usize,
    max_refs: usize,
    store: &mut RefStore<B>,
) -> Result<ContextEnvelope> {
    if chain.is_empty() {
        return Ok(ContextEnvelope {
            hot: vec![],
            refs: vec![],
            rollup: None,
        });
    }
    // Per-phase render sizes, oldest→newest.
    let sizes: Vec<usize> = chain
        .iter()
        .map(|h| estimate_tokens(&Handoff::format_for_prompt(std::slice::from_ref(h))))
        .collect();
    let mut hot_n = policy.hot_count(&sizes, budget).min(chain.len()).max(1);

    // Render hot; if the hot set alone busts budget, demote the oldest hot phase
    // until it fits — never silently exceed.
    let mut hot: Vec<String> = chain[chain.len() - hot_n..]
        .iter()
        .map(|h| Handoff::format_for_prompt(std::slice::from_ref(h)))
        .collect();
    while hot_n > 1 && hot.iter().map(|s| s.len()).sum::<usize>() > budget {
        eprintln!("[context] envelope over budget; demoting an extra hot phase");
        hot.remove(0);
        hot_n -= 1;
    }

    // Demoted = everything before the hot slice, newest-first.
    let demoted: Vec<&Handoff> = chain[..chain.len() - hot_n].iter().rev().collect();
    let mut refs = Vec::new();
    for h in demoted.iter().take(max_refs) {
        let blob = serde_json::to_vec(h)?;
        let hash = store.put(&blob)?;
        refs.push(RefLine {
            hash,
            digest: digest_of(h),
        });
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::policy::TiersPolicy;
    use crate::context::ref_store::RefStore;
    use leyline_core::MemBlobStore;

    fn phase(n: u32) -> Handoff {
        Handoff::new(
            n,
            "dev-agent",
            None,
            "rosary-test",
            "claude",
            &crate::manifest::Work::default(),
            None,
        )
    }

    #[test]
    fn bound_holds_and_is_flat_past_the_ref_cap() {
        let budget = 4000;
        let render_len = |depth: u32| {
            let chain: Vec<Handoff> = (0..depth).map(phase).collect();
            let mut rs = RefStore::new(MemBlobStore::default());
            build(&chain, &TiersPolicy, budget, 8, &mut rs)
                .unwrap()
                .render()
                .len()
        };
        let a = render_len(50);
        let b = render_len(200);
        assert!(a <= budget && b <= budget, "over budget: {a}, {b}");
        // Past the ref cap the render is flat in history — O(max_refs), not O(depth).
        assert!(a.abs_diff(b) <= 100, "render grew with history: {a} -> {b}");
    }

    #[test]
    fn older_than_max_refs_roll_up() {
        let chain: Vec<Handoff> = (0..12u32).map(phase).collect();
        let mut rs = RefStore::new(MemBlobStore::default());
        let env = build(&chain, &TiersPolicy, 8000, 4, &mut rs).unwrap();
        assert_eq!(env.refs.len(), 4); // capped
        assert!(env.rollup.is_some()); // remainder collapsed to one blob
    }
}
