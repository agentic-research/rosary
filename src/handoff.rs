//! Structured handoff between pipeline phases.
//!
//! Written by the orchestrator (not the agent) after each phase completes.
//! The next agent's prompt references this file for context about what
//! the previous phase did, what to review, and where to look.
//!
//! ## Handoff struct
//!
//! Each [`Handoff`] captures one phase's output:
//! - `phase`, `from_agent`, `to_agent` — pipeline position
//! - `bead_id` — the work item being processed
//! - `provider` — execution backend (e.g. "claude", "gemini")
//! - `thread_id` — optional thread from [`HierarchyStore`], giving agents
//!   context about their position in a larger progression of work
//! - `summary`, `files_changed`, `lines_changed` — what changed
//! - `review_hints` — auto-generated focus areas for the reviewing agent
//! - `artifacts` — paths to manifest, log, and previous handoff
//! - `verdict` — review result (filled by staging/prod agents)
//!
//! ## Key operations
//!
//! - [`Handoff::read_chain`] reads the full sequence of handoffs from a workspace
//! - [`Handoff::format_for_prompt`] renders the chain as markdown for agent prompts
//! - [`Handoff::chain_hash`] produces a tamper-evident SHA-256 hash chain
//!
//! ## File naming
//!
//! Handoffs are stored as `.rsry-handoff-{phase}.json` in the workspace directory.
//!
//! Backend-agnostic: works with any orchestrator, provider, or execution backend.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A structured handoff between pipeline phases.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Handoff {
    pub schema_version: String,
    pub phase: u32,
    pub from_agent: String,
    pub to_agent: Option<String>,
    pub bead_id: String,
    pub provider: String,
    /// Thread this bead belongs to (from HierarchyStore). Gives agents
    /// context about their position in a larger progression of work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,

    /// One-line summary of what this phase accomplished.
    pub summary: String,
    /// Files modified in this phase.
    pub files_changed: Vec<String>,
    /// Lines added/removed.
    pub lines_changed: LinesChanged,

    /// Hints for the reviewing agent — what to focus on.
    pub review_hints: Vec<String>,

    /// Paths to related artifacts in the workspace.
    pub artifacts: Artifacts,

    /// Review verdict (filled by staging/prod agents, null for dev).
    pub verdict: Option<Verdict>,

    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LinesChanged {
    pub added: u64,
    pub removed: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Artifacts {
    /// Path to .rsry-dispatch.json (SBOM manifest).
    pub manifest: Option<String>,
    /// Path to .rsry-stream.jsonl (agent output log).
    pub log: Option<String>,
    /// Path to previous phase's handoff (handoff chain).
    pub previous_handoff: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    /// "approve", "request_changes", or "reject"
    pub decision: String,
    /// Specific concerns found during review.
    pub concerns: Vec<String>,
    /// Suggestions for improvement (non-blocking).
    pub suggestions: Vec<String>,
}

impl Handoff {
    /// Content hash of this handoff, forming a hash chain with previous phases.
    ///
    /// Covers: phase, from_agent, bead_id, summary, files_changed, previous_handoff.
    /// The chain property: each handoff's hash includes the previous handoff's
    /// hash reference, making the pipeline tamper-evident.
    ///
    /// Does NOT include timestamp (non-deterministic) or verdict (may be added later).
    pub fn chain_hash(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(self.phase.to_le_bytes());
        hasher.update(self.from_agent.as_bytes());
        hasher.update(b"\0");
        hasher.update(self.bead_id.as_bytes());
        hasher.update(b"\0");
        hasher.update(self.summary.as_bytes());
        hasher.update(b"\0");
        for f in &self.files_changed {
            hasher.update(f.as_bytes());
            hasher.update(b"\0");
        }
        // Chain: include previous handoff reference (path or hash)
        if let Some(ref prev) = self.artifacts.previous_handoff {
            hasher.update(prev.as_bytes());
        }
        hasher.finalize().into()
    }

    /// Hex-encoded chain hash (for display, storage, and chain references).
    pub fn chain_hash_hex(&self) -> String {
        hex::encode(self.chain_hash())
    }

    /// Create a handoff from orchestrator state after a phase completes.
    ///
    /// `summary` is extracted from the agent's commit messages or final
    /// output. `review_hints` are derived from changed files and bead
    /// description keywords.
    pub fn new(
        phase: u32,
        from_agent: &str,
        to_agent: Option<&str>,
        bead_id: &str,
        provider: &str,
        work: &crate::manifest::Work,
    ) -> Self {
        // Generate review hints from file patterns
        let mut hints = Vec::new();
        for f in &work.files_changed {
            if f.contains("test") {
                hints.push(format!("Test changes in {f} — verify coverage"));
            }
            if f.ends_with("reconcile.rs") || f.ends_with("dispatch.rs") {
                hints.push(format!(
                    "Core dispatch path changed: {f} — check concurrency"
                ));
            }
        }

        let summary = work
            .commits
            .first()
            .map(|c| c.message.clone())
            .unwrap_or_else(|| {
                format!(
                    "Changed {} files (+{}/-{})",
                    work.files_changed.len(),
                    work.lines_added,
                    work.lines_removed
                )
            });

        Handoff {
            schema_version: "1".to_string(),
            phase,
            from_agent: from_agent.to_string(),
            to_agent: to_agent.map(|s| s.to_string()),
            bead_id: bead_id.to_string(),
            provider: provider.to_string(),
            thread_id: None, // Set by reconciler when hierarchy is available
            summary,
            files_changed: work.files_changed.clone(),
            lines_changed: LinesChanged {
                added: work.lines_added,
                removed: work.lines_removed,
            },
            review_hints: hints,
            artifacts: Artifacts {
                manifest: Some(".rsry-dispatch.json".to_string()),
                log: Some(format!(".rsry-stream-{phase}.jsonl")),
                previous_handoff: if phase > 0 {
                    Some(format!(".rsry-handoff-{}.json", phase - 1))
                } else {
                    None
                },
            },
            verdict: None,
            timestamp: Utc::now(),
        }
    }

    /// Write the handoff to the workspace directory.
    pub fn write_to(&self, workspace_dir: &Path) -> anyhow::Result<PathBuf> {
        let filename = format!(".rsry-handoff-{}.json", self.phase);
        let path = workspace_dir.join(&filename);
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, &content)?;
        eprintln!("[handoff] wrote {}", path.display());
        Ok(path)
    }

    /// Read a handoff from a workspace directory.
    pub fn read_from(workspace_dir: &Path, phase: u32) -> anyhow::Result<Self> {
        let filename = format!(".rsry-handoff-{phase}.json");
        let path = workspace_dir.join(filename);
        let content = std::fs::read_to_string(&path)?;
        Ok(serde_json::from_str(&content)?)
    }

    /// Read all handoffs in a workspace (the full chain).
    pub fn read_chain(workspace_dir: &Path) -> Vec<Self> {
        (0..10)
            .map_while(|phase| Self::read_from(workspace_dir, phase).ok())
            .collect()
    }

    /// Format the handoff chain as context for the next agent's prompt.
    pub fn format_for_prompt(chain: &[Self]) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{CommitInfo, Work};

    fn sample_work() -> Work {
        Work {
            commits: vec![CommitInfo {
                sha: "abc123".to_string(),
                message: "fix(reconcile): handle timeout=0 edge case".to_string(),
                author: "dev-agent".to_string(),
            }],
            files_changed: vec![
                "src/reconcile.rs".to_string(),
                "src/reconcile_test.rs".to_string(),
            ],
            lines_added: 47,
            lines_removed: 12,
            diff_stat: Some("2 files changed".to_string()),
        }
    }

    #[test]
    fn handoff_roundtrip() {
        let work = sample_work();
        let h = Handoff::new(
            0,
            "dev-agent",
            Some("staging-agent"),
            "rosary-abc",
            "claude",
            &work,
        );

        let json = serde_json::to_string_pretty(&h).unwrap();
        let parsed: Handoff = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.phase, 0);
        assert_eq!(parsed.from_agent, "dev-agent");
        assert_eq!(parsed.to_agent.as_deref(), Some("staging-agent"));
        assert_eq!(parsed.files_changed.len(), 2);
        assert!(parsed.summary.contains("timeout=0"));
        assert!(parsed.artifacts.previous_handoff.is_none()); // phase 0
    }

    #[test]
    fn handoff_chain_links() {
        let work = sample_work();
        let h1 = Handoff::new(
            1,
            "staging-agent",
            Some("prod-agent"),
            "rosary-abc",
            "gemini",
            &work,
        );

        assert_eq!(
            h1.artifacts.previous_handoff.as_deref(),
            Some(".rsry-handoff-0.json")
        );
    }

    #[test]
    fn handoff_write_read() {
        let tmp = tempfile::TempDir::new().unwrap();
        let work = sample_work();
        let h = Handoff::new(0, "dev-agent", None, "rosary-test", "claude", &work);

        h.write_to(tmp.path()).unwrap();
        let read = Handoff::read_from(tmp.path(), 0).unwrap();
        assert_eq!(read.bead_id, "rosary-test");
    }

    #[test]
    fn handoff_chain_read() {
        let tmp = tempfile::TempDir::new().unwrap();
        let work = sample_work();

        let h0 = Handoff::new(
            0,
            "dev-agent",
            Some("staging-agent"),
            "rosary-x",
            "claude",
            &work,
        );
        h0.write_to(tmp.path()).unwrap();

        let h1 = Handoff::new(1, "staging-agent", None, "rosary-x", "gemini", &work);
        h1.write_to(tmp.path()).unwrap();

        let chain = Handoff::read_chain(tmp.path());
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].from_agent, "dev-agent");
        assert_eq!(chain[1].from_agent, "staging-agent");
    }

    #[test]
    fn format_for_prompt_includes_context() {
        let work = sample_work();
        let h = Handoff::new(
            0,
            "dev-agent",
            Some("staging-agent"),
            "rosary-abc",
            "claude",
            &work,
        );
        let prompt = Handoff::format_for_prompt(&[h]);

        assert!(prompt.contains("Phase 0"));
        assert!(prompt.contains("dev-agent"));
        assert!(prompt.contains("timeout=0"));
        assert!(prompt.contains("mache MCP"));
    }

    // --- chain_hash tests ---

    #[test]
    fn chain_hash_deterministic() {
        let work = sample_work();
        let h = Handoff::new(0, "dev-agent", None, "rsry-abc", "claude", &work);
        assert_eq!(h.chain_hash(), h.chain_hash());
    }

    #[test]
    fn chain_hash_changes_with_phase() {
        let work = sample_work();
        let h0 = Handoff::new(0, "dev-agent", None, "rsry-abc", "claude", &work);
        let h1 = Handoff::new(1, "dev-agent", None, "rsry-abc", "claude", &work);
        assert_ne!(h0.chain_hash(), h1.chain_hash());
    }

    #[test]
    fn chain_hash_includes_previous_handoff() {
        let work = sample_work();
        let h0 = Handoff::new(
            0,
            "dev-agent",
            Some("staging-agent"),
            "rsry-abc",
            "claude",
            &work,
        );
        let h1 = Handoff::new(1, "staging-agent", None, "rsry-abc", "claude", &work);
        // h1 has previous_handoff = Some(".rsry-handoff-0.json"), h0 has None
        assert_ne!(
            h0.chain_hash(),
            h1.chain_hash(),
            "chain hash must differ when previous_handoff differs"
        );
    }

    #[test]
    fn chain_hash_hex_is_64_chars() {
        let work = sample_work();
        let h = Handoff::new(0, "dev-agent", None, "rsry-abc", "claude", &work);
        assert_eq!(h.chain_hash_hex().len(), 64);
    }

    #[test]
    fn review_hints_generated() {
        let work = Work {
            commits: vec![],
            files_changed: vec![
                "src/dispatch.rs".to_string(),
                "src/dispatch_test.rs".to_string(),
            ],
            lines_added: 10,
            lines_removed: 5,
            diff_stat: None,
        };
        let h = Handoff::new(0, "dev-agent", None, "rosary-abc", "claude", &work);

        assert!(h.review_hints.iter().any(|r| r.contains("concurrency")));
        assert!(h.review_hints.iter().any(|r| r.contains("coverage")));
    }
}
