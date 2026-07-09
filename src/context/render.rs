//! Renderer selection for the handoff chain → prompt context. Concrete renderers
//! (plain full-chain, bounded content-addressed) behind one trait, so the choice
//! lives in one place instead of scattered fallbacks. First extraction toward
//! decomposing handoff.rs (rosary-6a143f).

use std::path::Path;

use crate::config::ContextConfig;
use crate::handoff::Handoff;

/// Render a handoff chain into a prompt-context string.
pub trait ContextRenderer {
    fn render(&self, chain: &[Handoff]) -> String;
}

/// The original full-chain markdown render. Moved verbatim from
/// `Handoff::format_for_prompt`, which now delegates here.
pub struct PlainRenderer;

impl ContextRenderer for PlainRenderer {
    fn render(&self, chain: &[Handoff]) -> String {
        if chain.is_empty() {
            return String::new();
        }

        let mut out = String::from("\n## Previous Phase Context\n\n");
        for h in chain {
            out.push_str(&format!(
                "### Phase {} ({} via {})\n",
                h.phase, h.from_agent, h.provider
            ));
            if let Some(ref tid) = h.thread_id {
                out.push_str(&format!("Thread: {tid}\n"));
            }
            out.push_str(&format!("Summary: {}\n", h.summary));
            if !h.files_changed.is_empty() {
                out.push_str(&format!("Files: {}\n", h.files_changed.join(", ")));
            }
            if !h.review_hints.is_empty() {
                out.push_str("Review hints:\n");
                for hint in &h.review_hints {
                    out.push_str(&format!("- {hint}\n"));
                }
            }
            if !h.tools_used.is_empty() {
                let approved: Vec<&str> = h
                    .tools_used
                    .iter()
                    .filter(|t| t.approved)
                    .map(|t| t.tool_name.as_str())
                    .collect();
                let rejected: Vec<&str> = h
                    .tools_used
                    .iter()
                    .filter(|t| !t.approved)
                    .map(|t| t.tool_name.as_str())
                    .collect();
                if !approved.is_empty() {
                    out.push_str(&format!("Tools used: {}\n", approved.join(", ")));
                }
                if !rejected.is_empty() {
                    out.push_str(&format!("Tools denied: {}\n", rejected.join(", ")));
                }
            }
            if let Some(ref v) = h.verdict {
                out.push_str(&format!("Verdict: {}\n", v.decision));
                for c in &v.concerns {
                    out.push_str(&format!("  Concern: {c}\n"));
                }
            }
            out.push('\n');
        }
        out.push_str("Handoff files are in your working directory. Use mache MCP tools to structurally review the changes.\n");
        out
    }
}

/// Bounded, content-addressed render (warm-resume). Falls back to the plain
/// render if the CAS is unavailable — the resilience the dispatch path needs.
pub struct BoundedRenderer<'a> {
    pub cfg: &'a ContextConfig,
    pub cas_dir: &'a Path,
}

impl ContextRenderer for BoundedRenderer<'_> {
    fn render(&self, chain: &[Handoff]) -> String {
        super::build_bounded_prompt(chain, self.cfg, self.cas_dir).unwrap_or_else(|e| {
            eprintln!("[context] bounded render failed ({e}); falling back to plain");
            PlainRenderer.render(chain)
        })
    }
}
