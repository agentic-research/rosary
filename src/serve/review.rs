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

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};

use crate::bead::Comment;
use crate::handoff::Handoff;
use crate::store::{AgentRunEvent, BeadStore};

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
    pub agent_run_event_count: usize,
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
/// shape. Latest-handoff metadata is picked by `timestamp` max; ties broken
/// by the last entry in the slice (caller stability).
pub(crate) fn summarize_evidence(
    handoffs: &[Handoff],
    comments: &[Comment],
    observation_count: usize,
    gate_results: &[GateResult],
    agent_run_events: &[AgentRunEvent],
) -> EvidenceSummary {
    let latest = handoffs.iter().max_by_key(|h| h.timestamp);
    EvidenceSummary {
        observation_count,
        gate_count: gate_results.len(),
        gate_pass_count: gate_results.iter().filter(|g| g.passed).count(),
        handoff_count: handoffs.len(),
        latest_handoff_at: latest.map(|h| h.timestamp),
        latest_handoff_summary: latest.map(|h| h.summary.clone()),
        comment_count: comments.len(),
        agent_run_event_count: agent_run_events.len(),
    }
}

/// Pure transformation — convert raw `(sha, summary)` pairs from
/// `git log base..HEAD --oneline` into the typed ChangeSetEntry view.
/// Caller controls ordering (typically most-recent-first); this helper does
/// NOT sort.
pub(crate) fn format_change_set(commits: Vec<(String, String)>) -> Vec<ChangeSetEntry> {
    commits
        .into_iter()
        .map(|(sha, summary)| ChangeSetEntry { sha, summary })
        .collect()
}

/// Pure JSON assembly — compose the four review-panel components into the
/// `rsry review` response shape. Deterministic: same inputs → identical
/// output. Workspace is rendered as JSON `null` when absent (not omitted)
/// so callers can distinguish "checked, no workspace" from "didn't check."
pub(crate) fn assemble_review(
    bead: Value,
    workspace: Option<WorkspaceInfo>,
    change_set: Vec<ChangeSetEntry>,
    evidence: EvidenceSummary,
    agent_run_events: Vec<AgentRunEvent>,
) -> Value {
    json!({
        "bead": bead,
        "workspace": workspace,
        "change_set": change_set,
        "evidence": evidence,
        "agent_run_events": agent_run_events,
    })
}

/// Parse `git log --oneline` output into `(sha, summary)` pairs. Pure — no
/// I/O. Splits each line on its FIRST whitespace character (space, tab,
/// etc.) to keep summaries with embedded spaces intact. Honors the
/// whitespace contract noted by Copilot review on PR #220 — tab-separated
/// downstream output parses correctly.
pub(crate) fn parse_oneline(stdout: &str) -> Vec<(String, String)> {
    stdout
        .lines()
        .filter_map(|line| {
            let split_at = line.find(char::is_whitespace)?;
            let sha = &line[..split_at];
            if sha.is_empty() {
                return None;
            }
            // Skip the one whitespace char; preserve the rest of the line
            // as-is so summary punctuation/spacing stays intact.
            let summary = &line[split_at + 1..];
            Some((sha.to_string(), summary.to_string()))
        })
        .collect()
}

/// Orchestrator: read bead + comments from the store, detect workspace state,
/// shell out to git for change-set + branch metadata, compose via the pure
/// helpers above. Errors when the bead isn't found in the store — caller
/// renders the error to the reviewer ("no such bead in this repo").
///
/// Workspace-scoped fields (handoffs, change-set, branch) are populated only
/// when an agent workspace actually exists on disk; otherwise the response
/// carries `workspace: null` + empty change-set + zero handoffs, which the
/// reviewer reads as "no agent dispatched yet."
pub(crate) async fn collect_review_for_bead(
    store: &dyn BeadStore,
    repo_name: &str,
    repo_path: &Path,
    bead_id: &str,
    agent_run_events: Vec<AgentRunEvent>,
) -> anyhow::Result<Value> {
    let bead = store
        .get_bead(bead_id, repo_name)
        .await?
        .ok_or_else(|| anyhow::anyhow!("bead {bead_id} not found in repo {repo_name}"))?;
    let bead_json = serde_json::to_value(&bead)?;

    let comments = store.list_comments(bead_id, false).await?;

    let ws = crate::workspace::Workspace::from_existing(bead_id, repo_name, repo_path);
    let (workspace_info, raw_commits, handoffs) = if ws.vcs == crate::workspace::VcsKind::None {
        (None, vec![], vec![])
    } else {
        let handoffs = Handoff::read_chain(&ws.work_dir);
        let (branch, commits) = collect_git_state(&ws.work_dir);
        let info = WorkspaceInfo {
            work_dir: ws.work_dir.to_string_lossy().to_string(),
            branch,
            commits_ahead: commits.len(),
        };
        (Some(info), commits, handoffs)
    };

    let change_set = format_change_set(raw_commits);
    let evidence = summarize_evidence(&handoffs, &comments, 0, &[], &agent_run_events);
    Ok(assemble_review(
        bead_json,
        workspace_info,
        change_set,
        evidence,
        agent_run_events,
    ))
}

/// Best-effort git inspection: branch name + commits ahead of `main`.
/// Returns `(String::new(), vec![])` on any git failure — review still
/// renders, evidence panel shows zero commits-ahead. Falls back to "last
/// 20 commits" when `main` doesn't exist (e.g. cloned with a single
/// branch).
fn collect_git_state(work_dir: &Path) -> (String, Vec<(String, String)>) {
    let branch = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(work_dir)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    let base = std::process::Command::new("git")
        .args(["merge-base", "HEAD", "main"])
        .current_dir(work_dir)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let log_args: Vec<String> = match &base {
        Some(b) => vec![
            "log".into(),
            format!("{b}..HEAD"),
            "--oneline".into(),
            "-n".into(),
            "50".into(),
        ],
        None => vec!["log".into(), "--oneline".into(), "-n".into(), "20".into()],
    };

    let commits = std::process::Command::new("git")
        .args(&log_args)
        .current_dir(work_dir)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| parse_oneline(&s))
        .unwrap_or_default();

    (branch, commits)
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
        let got = summarize_evidence(&[], &[], 0, &[], &[]);
        let want = EvidenceSummary {
            observation_count: 0,
            gate_count: 0,
            gate_pass_count: 0,
            handoff_count: 0,
            latest_handoff_at: None,
            latest_handoff_summary: None,
            comment_count: 0,
            agent_run_event_count: 0,
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
        let got = summarize_evidence(&handoffs, &comments, 7, &gates, &[]);

        assert_eq!(got.handoff_count, 1, "handoffs");
        assert_eq!(got.comment_count, 2, "comments");
        assert_eq!(got.observation_count, 7, "observations passed through");
        assert_eq!(got.gate_count, 2, "gate total");
        assert_eq!(got.gate_pass_count, 1, "gates that passed");
    }

    #[test]
    fn assemble_review_surfaces_partial_agent_run_events() {
        let ts = Utc.with_ymd_and_hms(2026, 6, 26, 16, 0, 0).unwrap();
        let event = crate::store::AgentRunEvent {
            id: "evt-review-1".to_string(),
            dispatch_id: "dispatch-review".to_string(),
            bead_ref: crate::store::WorkRef {
                repo: "rosary".to_string(),
                bead_id: "rosary-run".to_string(),
                scope: String::new(),
            },
            session_ref: Some(crate::dispatch::AgentSessionRef::new("codex", "thread-123")),
            event_type: "review_finding".to_string(),
            summary: "malformed session_ref should be rejected".to_string(),
            payload: json!({ "severity": "should-fix" }),
            created_at: ts,
        };

        let events = vec![event];
        let evidence = summarize_evidence(&[], &[], 0, &[], &events);
        let got = assemble_review(
            json!({ "id": "rosary-run" }),
            None,
            vec![],
            evidence,
            events,
        );

        assert_eq!(got["agent_run_events"][0]["event_type"], "review_finding");
        assert_eq!(
            got["agent_run_events"][0]["summary"],
            "malformed session_ref should be rejected"
        );
        assert_eq!(got["evidence"]["agent_run_event_count"], 1);
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
        let got = summarize_evidence(&handoffs, &[], 0, &[], &[]);

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
            agent_run_event_count: 0,
        };
        (bead, workspace, change_set, evidence)
    }

    /// The assembled response carries every documented top-level key —
    /// pins the JSON schema the CLI text formatter and downstream renderers
    /// (MCP Apps in later phases) will key on.
    #[test]
    fn assemble_review_emits_all_documented_top_level_keys() {
        let (bead, ws, cs, ev) = sample_review_inputs();
        let got = assemble_review(bead, ws, cs, ev, vec![]);

        assert!(got.get("bead").is_some(), "bead key present");
        assert!(got.get("workspace").is_some(), "workspace key present");
        assert!(got.get("change_set").is_some(), "change_set key present");
        assert!(got.get("evidence").is_some(), "evidence key present");
        assert!(
            got.get("agent_run_events").is_some(),
            "agent_run_events key present"
        );
    }

    /// `workspace: null` is the canonical "no agent dispatched" signal.
    /// Caller distinguishes "we checked and there's no workspace" (null)
    /// from "we didn't check" (key absent) — the key must always be present.
    #[test]
    fn assemble_review_workspace_absent_serializes_as_null() {
        let (bead, _ws, cs, ev) = sample_review_inputs();
        let got = assemble_review(bead, None, cs, ev, vec![]);
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
        let a = assemble_review(bead1, ws1, cs1, ev1, vec![]);
        let b = assemble_review(bead2, ws2, cs2, ev2, vec![]);
        assert_eq!(a, b);
    }

    /// Empty input → empty output. Caller distinguishes "no commits" from
    /// "git failed" (the latter is the orchestrator's job, not the parser's).
    #[test]
    fn parse_oneline_empty_input_empty_output() {
        assert!(parse_oneline("").is_empty());
        assert!(parse_oneline("\n\n").is_empty());
    }

    /// Each line is split on its FIRST whitespace so commit summaries
    /// containing further spaces stay intact.
    #[test]
    fn parse_oneline_splits_sha_and_summary_at_first_space() {
        let raw = "abc1234 first commit message\ndef5678 fix(scope): subject line";
        let got = parse_oneline(raw);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, "abc1234");
        assert_eq!(got[0].1, "first commit message");
        assert_eq!(got[1].1, "fix(scope): subject line");
    }

    /// Lines without a space are skipped — protects against malformed git
    /// output rather than producing entries with empty summaries.
    #[test]
    fn parse_oneline_skips_lines_without_separator() {
        let raw = "abc1234 ok line\nbroken-no-space\n";
        let got = parse_oneline(raw);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "abc1234");
    }

    /// The docstring promises "first whitespace" — a tab is whitespace.
    /// Copilot review on PR #220: `git log --oneline` defaults to a
    /// space separator but downstream tooling occasionally swaps in a
    /// tab; the parser must honor its own contract.
    #[test]
    fn parse_oneline_splits_on_tab_when_present() {
        let raw = "abc1234\ttab-separated summary text";
        let got = parse_oneline(raw);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "abc1234");
        assert_eq!(got[0].1, "tab-separated summary text");
    }

    /// Bead not present in the store → orchestrator surfaces a "not found"
    /// error so the CLI/MCP layer can render it to the reviewer.
    #[tokio::test]
    async fn collect_review_for_bead_errors_when_bead_missing() {
        use crate::bead_sqlite::SqliteBeadStore;
        let store = SqliteBeadStore::connect(Path::new(":memory:")).unwrap();
        let nowhere = Path::new("/nonexistent-for-review-test");
        let err = collect_review_for_bead(&store, "rosary", nowhere, "rosary-ghost", vec![])
            .await
            .expect_err("missing bead must error");
        assert!(
            err.to_string().contains("not found"),
            "error should name the missing-bead condition; got: {err}"
        );
    }

    /// Bead found but no workspace dir on disk → response carries
    /// `workspace: null`, empty change_set, zero handoffs — but the
    /// bead's own seeded comment count IS reflected. Pins the
    /// "compose from whatever exists" semantics.
    #[tokio::test]
    async fn collect_review_for_bead_no_workspace_renders_null_with_comments_counted() {
        use crate::bead_sqlite::SqliteBeadStore;
        let store = SqliteBeadStore::connect(Path::new(":memory:")).unwrap();
        store
            .create_bead("rosary-rev1", "review fixture", "", 1, "feat")
            .await
            .unwrap();
        store
            .add_comment("rosary-rev1", "first note", "tester")
            .await
            .unwrap();
        store
            .add_comment("rosary-rev1", "second note", "tester")
            .await
            .unwrap();

        let event = AgentRunEvent {
            id: "evt-review-collect".to_string(),
            dispatch_id: "dispatch-review-collect".to_string(),
            bead_ref: crate::store::WorkRef {
                repo: "rosary".to_string(),
                bead_id: "rosary-rev1".to_string(),
                scope: String::new(),
            },
            session_ref: Some(crate::dispatch::AgentSessionRef::new(
                "codex",
                "thread-review",
            )),
            event_type: "review_finding".to_string(),
            summary: "partial review evidence".to_string(),
            payload: json!({ "severity": "should-fix" }),
            created_at: Utc::now(),
        };

        let nowhere = Path::new("/nonexistent-for-review-test");
        let got = collect_review_for_bead(&store, "rosary", nowhere, "rosary-rev1", vec![event])
            .await
            .expect("seeded bead must be retrievable");

        assert!(got["workspace"].is_null(), "workspace must be JSON null");
        assert_eq!(
            got["change_set"].as_array().map(Vec::len),
            Some(0),
            "no workspace → no change_set"
        );
        assert_eq!(
            got["evidence"]["comment_count"].as_u64(),
            Some(2),
            "seeded comments must be counted"
        );
        assert_eq!(
            got["evidence"]["handoff_count"].as_u64(),
            Some(0),
            "no workspace → no handoffs"
        );
        assert_eq!(
            got["evidence"]["agent_run_event_count"].as_u64(),
            Some(1),
            "partial agent events must be counted"
        );
        assert_eq!(
            got["agent_run_events"][0]["summary"],
            "partial review evidence"
        );
        assert_eq!(
            got["bead"]["id"].as_str(),
            Some("rosary-rev1"),
            "bead summary must be present"
        );
    }
}
