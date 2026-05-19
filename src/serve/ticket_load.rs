//! Phase 0 of the Linear-escalation-triage EPIC (rosary-5dc9b0).
//!
//! Consolidates Linear + GitHub + Zendesk + bead context for a single ticket
//! into one MCP response, replacing the 4-5 separate manual lookups the user
//! performs today.
//!
//! Four pure-ish helpers compose the orchestrator `tool_ticket_load`:
//! - `extract_github_link` / `extract_zendesk_link` — URL-shape detection in free text
//! - `find_triage_bead` — locates an existing tracking bead by ticket id
//! - `assemble_context` — pure JSON assembly from the fetched parts
//!
//! Live integration (hitting Linear's GraphQL) is gated behind LINEAR_API_KEY
//! to keep `cargo test` deterministic and network-free.

use serde_json::Value;

use crate::pool::RepoPool;

/// Detect a GitHub issue or pull-request URL embedded in free text.
///
/// Matches `https://github.com/{org}/{repo}/(issues|pull)/{N}`. Returns the
/// first match. No regex — token-walk + `str::strip_prefix` only.
pub(crate) fn extract_github_link(_text: &str) -> Option<String> {
    unimplemented!("rosary-5dc9b0 RED phase — implement in GREEN")
}

/// Detect a Zendesk ticket URL embedded in free text.
///
/// Matches `https://{subdomain}.zendesk.com/agent/tickets/{N}`. Returns the
/// first match. No regex — token-walk + `str::strip_prefix` only.
pub(crate) fn extract_zendesk_link(_text: &str) -> Option<String> {
    unimplemented!("rosary-5dc9b0 RED phase — implement in GREEN")
}

/// Search the configured triage repo's beads for one referencing this Linear
/// ticket. Returns the matching bead summary as JSON, or None if absent.
pub(crate) async fn find_triage_bead(_pool: &RepoPool, _ticket_id: &str) -> Option<Value> {
    unimplemented!("rosary-5dc9b0 RED phase — implement in GREEN")
}

/// Pure JSON assembly: combine the fetched Linear data, comments, optional GH
/// link, optional Zendesk URL, and optional existing-bead reference into the
/// `rsry_ticket_load` response shape documented in the bead.
pub(crate) fn assemble_context(
    _linear: Value,
    _comments: Vec<Value>,
    _github: Option<Value>,
    _zendesk: Option<String>,
    _bead: Option<Value>,
) -> Value {
    unimplemented!("rosary-5dc9b0 RED phase — implement in GREEN")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `extract_github_link` recognizes the canonical issues URL shape.
    #[test]
    fn extract_github_link_finds_issue_url() {
        let body = "Related: https://github.com/agentic-research/rosary/issues/42 — see thread.";
        assert_eq!(
            extract_github_link(body).as_deref(),
            Some("https://github.com/agentic-research/rosary/issues/42"),
        );
    }

    /// Absent the canonical URL shape, returns None — null-safe for callers.
    #[test]
    fn extract_github_link_returns_none_when_absent() {
        assert!(extract_github_link("no links here, plain prose").is_none());
    }

    /// Recognizes the Zendesk agent ticket URL shape.
    #[test]
    fn extract_zendesk_link_finds_ticket_url() {
        let body = "Customer ref: https://chainguard.zendesk.com/agent/tickets/12345 — escalated.";
        assert!(
            extract_zendesk_link(body).is_some(),
            "expected a Zendesk URL to be detected in: {body}",
        );
    }

    /// `find_triage_bead` returns Some when a bead's title references the ticket id.
    /// (Stub for now — GREEN phase wires through a seeded test pool.)
    #[tokio::test]
    async fn find_triage_bead_returns_match_when_ticket_in_title() {
        let pool = RepoPool::empty();
        let _ = find_triage_bead(&pool, "CUS-495").await;
    }

    /// `assemble_context` produces the documented MCP response shape:
    /// non-null fields where data was provided, JSON null where absent.
    #[test]
    fn assemble_context_handles_all_optionals_present() {
        let linear = json!({ "title": "x", "body": "y", "status": "Triage" });
        let comments = vec![json!({"body": "first"})];
        let gh = Some(json!({"url": "https://github.com/o/r/issues/1", "state": "open"}));
        let zendesk = Some("https://zd.example.com/agent/tickets/9".to_string());
        let bead: Option<Value> = None;

        let out = assemble_context(linear, comments, gh, zendesk, bead);

        assert_eq!(out["linear"]["title"].as_str(), Some("x"));
        assert_eq!(out["linked_github"]["state"].as_str(), Some("open"));
        assert_eq!(
            out["linked_zendesk"].as_str(),
            Some("https://zd.example.com/agent/tickets/9"),
        );
        assert!(out["existing_bead"].is_null());
    }
}
