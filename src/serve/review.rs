// RED-phase scaffold: helpers + types are intentionally not yet wired through
// handlers.rs / tools.rs / CLI. GREEN of rosary-cd5d2a removes this allow when
// `tool_review` lands and the CLI subcommand calls into them.
#![allow(dead_code)]

//! Phase 0 of rosary-ccd5a2 (`rsry review` agent-native review substrate).
//!
//! Shape-proof MVP: prove that the reviewable unit
//! `(bead, evidence-chain, change-set)` is composable from rosary's existing
//! primitives — without yet inventing comment-anchoring, jj-aware slicing,
//! integration delegation, or MCP Apps rendering (those land in later phases
//! per rosary-ccd5a2 scope).
//!
//! Pure-ish helpers mirror the `ticket_load` pattern (Phase 0 of
//! rosary-5dc9b0): the orchestrator wires store reads + filesystem fetches,
//! and these helpers compose the result deterministically so tests need no
//! mocks. RED phase ships unimplemented bodies so the test set pins the
//! shape; GREEN phase fills them in.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::bead::Comment;
use crate::handoff::Handoff;

/// Aggregated evidence summary for the review panel.
///
/// Phase 0 surfaces the four counts a reviewer scans first: how many
/// observations exist in the bead's ADR-0010 log, what gates have run and
/// their pass-status, how many handoffs were emitted across the agent
/// pipeline, and how many comments accreted in the audit trail. Per-field
/// breakdowns + Guardrail integration land in later phases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceSummary {
    pub observation_count: usize,
    pub gate_count: usize,
    pub gate_pass_count: usize,
    pub handoff_count: usize,
    /// Timestamp of the most recent handoff (None if no handoffs emitted).
    pub latest_handoff_at: Option<DateTime<Utc>>,
    /// One-line summary of the most recent handoff (None if no handoffs).
    pub latest_handoff_summary: Option<String>,
    pub comment_count: usize,
}

/// One entry in the sliced change-set view.
///
/// Phase 0 sources from `git log base..HEAD --oneline` (per the bead
/// description); jj-aware `change_id` slicing defers to Phase 2 once
/// rsry-8c31a5 ships the change_id primitive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangeSetEntry {
    /// Short commit SHA (7 hex chars, matching `git log --oneline` default).
    pub sha: String,
    /// One-line commit summary (the subject line of the commit message).
    pub summary: String,
}

/// Workspace + branch metadata shown to the reviewer.
///
/// None when no agent has been dispatched on the bead yet — in which case
/// `assemble_review` renders the workspace field as JSON null and the
/// reviewer sees "no workspace — no agent dispatched yet."
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceInfo {
    pub work_dir: String,
    pub branch: String,
    /// Commits on the working branch ahead of its merge base with main.
    pub commits_ahead: usize,
}

/// Stubbed gate-result type. Wire to the Guardrail trait when rosary-cdd522
/// lands; Phase 0 accepts an empty slice from callers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GateResult {
    pub name: String,
    pub passed: bool,
}

/// Pure aggregation — summarize evidence sources into the EvidenceSummary
/// shape. Tests pin the shape; GREEN fills in the counting logic.
pub(crate) fn summarize_evidence(
    handoffs: &[Handoff],
    comments: &[Comment],
    observation_count: usize,
    gate_results: &[GateResult],
) -> EvidenceSummary {
    let _ = (handoffs, comments, observation_count, gate_results);
    unimplemented!("summarize_evidence: GREEN of rosary-cd5d2a fills this in");
}

/// Pure transformation — convert raw `(sha, summary)` pairs from
/// `git log base..HEAD --oneline` into the typed ChangeSetEntry view.
/// Caller controls ordering (typically most-recent-first); this helper does
/// NOT sort.
pub(crate) fn format_change_set(commits: Vec<(String, String)>) -> Vec<ChangeSetEntry> {
    let _ = commits;
    unimplemented!("format_change_set: GREEN of rosary-cd5d2a fills this in");
}

/// Pure JSON assembly — compose the four review-panel components into the
/// `rsry review` response shape. Deterministic: same inputs → identical
/// output. Tests assert this property explicitly so the reviewer sees the
/// same panel across repeated runs unless the underlying state changed.
pub(crate) fn assemble_review(
    bead: Value,
    workspace: Option<WorkspaceInfo>,
    change_set: Vec<ChangeSetEntry>,
    evidence: EvidenceSummary,
) -> Value {
    let _ = (bead, workspace, change_set, evidence);
    unimplemented!("assemble_review: GREEN of rosary-cd5d2a fills this in");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handoff::{Artifacts, LinesChanged};
    use chrono::TimeZone;
    use serde_json::json;

    fn comment_at(idx: usize, ts: DateTime<Utc>) -> Comment {
        Comment {
            id: format!("c{idx}"),
            issue_id: "rosary-test".to_string(),
            text: format!("comment {idx}"),
            author: "tester".to_string(),
            created_at: ts,
            edited_at: None,
            edit_reason: None,
            original_text: None,
            deleted_at: None,
            delete_reason: None,
        }
    }

    fn handoff_at(idx: u32, ts: DateTime<Utc>, summary: &str) -> Handoff {
        Handoff {
            schema_version: "1".to_string(),
            phase: idx,
            from_agent: "dev-agent".to_string(),
            to_agent: Some("staging-agent".to_string()),
            bead_id: "rosary-test".to_string(),
            provider: "claude".to_string(),
            thread_id: None,
            summary: summary.to_string(),
            files_changed: vec![],
            lines_changed: LinesChanged::default(),
            review_hints: vec![],
            artifacts: Artifacts {
                manifest: None,
                log: None,
                previous_handoff: None,
            },
            verdict: None,
            tools_used: vec![],
            previous_chain_hash: None,
            commit_shas: vec![],
            timestamp: ts,
        }
    }

    /// All-empty inputs produce a summary with all zero counts and no
    /// latest-handoff metadata. Reviewer sees "no evidence yet" cleanly,
    /// not a synthesized lie.
    #[test]
    fn summarize_evidence_zero_case_all_counts_zero() {
        let got = summarize_evidence(&[], &[], 0, &[]);
        let want = EvidenceSummary {
            observation_count: 0,
            gate_count: 0,
            gate_pass_count: 0,
            handoff_count: 0,
            latest_handoff_at: None,
            latest_handoff_summary: None,
            comment_count: 0,
        };
        assert_eq!(got, want);
    }

    /// Each input source contributes to its own counter — no cross-bleed.
    /// Pins the contract that callers can wire stores independently.
    #[test]
    fn summarize_evidence_counts_each_source_independently() {
        let ts = Utc.with_ymd_and_hms(2026, 6, 17, 12, 0, 0).unwrap();
        let comments = vec![comment_at(1, ts), comment_at(2, ts)];
        let handoffs = vec![handoff_at(1, ts, "dev done")];
        let gates = vec![
            GateResult {
                name: "tests".to_string(),
                passed: true,
            },
            GateResult {
                name: "clippy".to_string(),
                passed: false,
            },
        ];
        let got = summarize_evidence(&handoffs, &comments, 7, &gates);

        assert_eq!(got.handoff_count, 1, "handoffs");
        assert_eq!(got.comment_count, 2, "comments");
        assert_eq!(got.observation_count, 7, "observations passed through");
        assert_eq!(got.gate_count, 2, "gate total");
        assert_eq!(got.gate_pass_count, 1, "gates that passed");
    }

    /// Most-recent handoff wins for the latest-* fields — pinning ordering
    /// so the reviewer scans the freshest context first.
    #[test]
    fn summarize_evidence_records_latest_handoff_metadata() {
        let early = Utc.with_ymd_and_hms(2026, 6, 17, 10, 0, 0).unwrap();
        let late = Utc.with_ymd_and_hms(2026, 6, 17, 14, 30, 0).unwrap();
        let handoffs = vec![
            handoff_at(1, early, "scaffold landed"),
            handoff_at(2, late, "tests green"),
        ];
        let got = summarize_evidence(&handoffs, &[], 0, &[]);

        assert_eq!(got.latest_handoff_at, Some(late));
        assert_eq!(got.latest_handoff_summary.as_deref(), Some("tests green"));
    }

    /// Empty input → empty output. Caller's responsibility to handle
    /// "no commits yet" downstream (e.g. workspace exists but agent hasn't
    /// committed).
    #[test]
    fn format_change_set_empty_input_empty_output() {
        let got = format_change_set(vec![]);
        assert!(got.is_empty());
    }

    /// Caller controls ordering — this helper does NOT sort. `git log`
    /// produces most-recent-first by default; passing through preserves
    /// reviewer expectation.
    #[test]
    fn format_change_set_preserves_caller_ordering() {
        let input = vec![
            ("aaaaaaa".to_string(), "third commit".to_string()),
            ("bbbbbbb".to_string(), "second commit".to_string()),
            ("ccccccc".to_string(), "first commit".to_string()),
        ];
        let got = format_change_set(input);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].sha, "aaaaaaa");
        assert_eq!(got[2].summary, "first commit");
    }

    fn sample_review_inputs() -> (
        Value,
        Option<WorkspaceInfo>,
        Vec<ChangeSetEntry>,
        EvidenceSummary,
    ) {
        let bead = json!({
            "id": "rosary-cd5d2a",
            "title": "[review P0] shape-proof MVP",
            "priority": 1,
            "status": "open",
        });
        let workspace = Some(WorkspaceInfo {
            work_dir: "/tmp/ws/feat-rsry-review-p0".to_string(),
            branch: "feat/rsry-review-p0".to_string(),
            commits_ahead: 3,
        });
        let change_set = vec![ChangeSetEntry {
            sha: "abc1234".to_string(),
            summary: "scaffold review module".to_string(),
        }];
        let evidence = EvidenceSummary {
            observation_count: 0,
            gate_count: 0,
            gate_pass_count: 0,
            handoff_count: 0,
            latest_handoff_at: None,
            latest_handoff_summary: None,
            comment_count: 4,
        };
        (bead, workspace, change_set, evidence)
    }

    /// The assembled response carries every documented top-level key —
    /// pins the JSON schema the CLI text formatter and downstream renderers
    /// (MCP Apps in later phases) will key on.
    #[test]
    fn assemble_review_emits_all_documented_top_level_keys() {
        let (bead, ws, cs, ev) = sample_review_inputs();
        let got = assemble_review(bead, ws, cs, ev);

        assert!(got.get("bead").is_some(), "bead key present");
        assert!(got.get("workspace").is_some(), "workspace key present");
        assert!(got.get("change_set").is_some(), "change_set key present");
        assert!(got.get("evidence").is_some(), "evidence key present");
    }

    /// `workspace: null` is the canonical "no agent dispatched" signal.
    /// Caller distinguishes "we checked and there's no workspace" (null)
    /// from "we didn't check" (key absent) — the key must always be present.
    #[test]
    fn assemble_review_workspace_absent_serializes_as_null() {
        let (bead, _ws, cs, ev) = sample_review_inputs();
        let got = assemble_review(bead, None, cs, ev);
        assert!(
            got.get("workspace").map(|v| v.is_null()).unwrap_or(false),
            "workspace key present and JSON null"
        );
    }

    /// Pure function: identical inputs → identical outputs. Pins the
    /// determinism guarantee the bead's acceptance criteria call out
    /// ("reviewers must see the same thing across runs unless state
    /// changed").
    #[test]
    fn assemble_review_is_idempotent() {
        let (bead1, ws1, cs1, ev1) = sample_review_inputs();
        let (bead2, ws2, cs2, ev2) = sample_review_inputs();
        let a = assemble_review(bead1, ws1, cs1, ev1);
        let b = assemble_review(bead2, ws2, cs2, ev2);
        assert_eq!(a, b);
    }
}
