use super::*;

fn test_bead(
    status: &str,
    dependency_count: u32,
    issue_type: &str,
    description: &str,
    files: &[&str],
    acceptance_criteria: &str,
) -> crate::bead::Bead {
    crate::bead::Bead {
        id: "rosary-test".to_string(),
        title: "test".to_string(),
        description: description.to_string(),
        status: status.to_string(),
        priority: 1,
        issue_type: issue_type.to_string(),
        owner: None,
        repo: "rosary".to_string(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        dependency_count,
        dependent_count: 0,
        comment_count: 0,
        branch: None,
        pr_url: None,
        jj_change_id: None,
        external_ref: None,
        files: files.iter().map(|s| (*s).to_string()).collect(),
        test_files: vec![],
        created_by: None,
        scope: String::new(),
        derived_from: vec![],
        acceptance_criteria: acceptance_criteria.to_string(),
    }
}

#[test]
fn status_counts_include_dispatchable_subset() {
    let scoped_impl = test_bead(
        "open",
        0,
        "task",
        "This description is long enough to count as refined for dispatch.",
        &["src/main.rs"],
        "cargo test --bin rsry dispatchable",
    );
    let merely_ready = test_bead(
        "open",
        0,
        "task",
        "This description is also long enough to count as refined for dispatch.",
        &[],
        "",
    );
    let blocked = test_bead(
        "open",
        1,
        "task",
        "Blocked bead still has enough description to avoid refinement checks.",
        &["src/lib.rs"],
        "cargo test",
    );

    // Exercises the single-source rollup (crate::status) that both the CLI and
    // MCP now project from.
    let counts = crate::status::status_json(&[scoped_impl, merely_ready, blocked]);
    assert_eq!(counts["total"].as_u64(), Some(3));
    assert_eq!(counts["ready"].as_u64(), Some(2));
    assert_eq!(counts["dispatchable"].as_u64(), Some(1));
    assert_eq!(counts["blocked"].as_u64(), Some(1));
}

#[test]
fn bead_matches_dispatchable_virtual_status() {
    let dispatchable = test_bead(
        "open",
        0,
        "task",
        "This description is long enough to count as refined for dispatch.",
        &["src/main.rs"],
        "cargo test --bin rsry dispatchable",
    );
    let ready_only = test_bead(
        "open",
        0,
        "task",
        "This description is long enough to count as refined for dispatch.",
        &[],
        "",
    );

    assert!(bead_matches_status(&dispatchable, Some("dispatchable")));
    assert!(!bead_matches_status(&ready_only, Some("dispatchable")));
    assert!(bead_matches_status(&ready_only, Some("ready")));
}

// ---- tool_review (rosary-cd5d2a) --------------------------------------

/// Phase 0 of rosary-ccd5a2. Caller must supply `bead_id`; the error
/// message names the missing arg so MCP clients learn the shape from
/// one rejection.
#[tokio::test]
async fn review_rejects_missing_bead_id() {
    let args = json!({ "repo_path": "/tmp" });
    let err = tool_review(&args, None).await.unwrap_err();
    assert!(
        err.to_string().contains("bead_id"),
        "error must name the missing field; got: {err}"
    );
}

/// Whitespace-only `bead_id` is rejected — same validation surface as
/// "missing entirely" so the error UX stays consistent.
#[tokio::test]
async fn review_rejects_blank_bead_id() {
    let args = json!({ "bead_id": "   ", "repo_path": "/tmp" });
    let err = tool_review(&args, None).await.unwrap_err();
    assert!(
        err.to_string().contains("bead_id"),
        "blank bead_id must hit the same gate; got: {err}"
    );
}

/// `repo_path` is required in Phase 0 — scope→path resolution is a
/// follow-up. The error names the missing arg so the user knows what
/// to add.
#[tokio::test]
async fn review_rejects_missing_repo_path() {
    let args = json!({ "bead_id": "rosary-cd5d2a" });
    let err = tool_review(&args, None).await.unwrap_err();
    assert!(
        err.to_string().contains("repo_path"),
        "error must name the missing field; got: {err}"
    );
}

#[tokio::test]
async fn review_surfaces_scoped_agent_run_events() {
    use crate::store::tests::InMemoryStore;

    let repo_dir = tempfile::TempDir::new().unwrap();
    let repo_name = repo_dir
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let repo_path = repo_dir.path().to_string_lossy().to_string();
    let beads_dir = repo_dir.path().join(".beads");
    let bead_store = crate::bead_sqlite::SqliteBeadStore::connect(&beads_dir.join("beads.db"))
        .expect("connect temp bead store");
    bead_store
        .create_bead("rosary-scoped", "scoped review fixture", "", 1, "task")
        .await
        .unwrap();

    let backend = InMemoryStore::new();
    tool_agent_run_event_record(
        &json!({
            "id": "evt-scoped",
            "dispatch_id": "dispatch-scoped",
            "repo": repo_name,
            "bead_id": "rosary-scoped",
            "scope": "team/auth",
            "event_type": "review_finding",
            "summary": "scoped partial evidence",
            "payload": { "severity": "should-fix" }
        }),
        Some(&backend),
    )
    .await
    .unwrap();

    let got = tool_review(
        &json!({
            "repo_path": repo_path,
            "bead_id": "rosary-scoped",
            "scope": "team/auth"
        }),
        Some(&backend),
    )
    .await
    .expect("review should render");

    assert_eq!(
        got["evidence"]["agent_run_event_count"].as_u64(),
        Some(1),
        "scoped event must be visible to rsry_review"
    );
    assert_eq!(got["agent_run_events"][0]["id"], "evt-scoped");
}

// ---- tool_ticket_load (rosary-5dc9b0) ---------------------------------

/// Caller must supply `ticket_id`; the error message names the missing arg
/// so future MCP clients learn the right shape from one rejection.
#[tokio::test]
async fn ticket_load_rejects_missing_ticket_id() {
    let args = json!({});
    let err = tool_ticket_load(&args, &crate::pool::RepoPool::empty())
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("ticket_id"),
        "error must name the missing field; got: {msg}"
    );
}

/// Whitespace-only ticket_id is rejected — same validation surface as a
/// missing field so the error UX stays consistent.
#[tokio::test]
async fn ticket_load_rejects_blank_ticket_id() {
    let args = json!({ "ticket_id": "   " });
    let err = tool_ticket_load(&args, &crate::pool::RepoPool::empty())
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("ticket_id"),
        "error must name the rejected field; got: {msg}"
    );
}

#[tokio::test]
async fn pipeline_upsert_errors_without_backend() {
    let args = json!({
        "repo": "rosary",
        "bead_id": "rsry-001",
        "pipeline_phase": 0,
        "pipeline_agent": "dev-agent",
    });
    let result = tool_pipeline_upsert(&args, None).await;
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("backend store not configured"), "got: {msg}");
}

#[tokio::test]
async fn pipeline_upsert_rejects_missing_required_fields() {
    // Missing pipeline_agent
    let args = json!({
        "repo": "rosary",
        "bead_id": "rsry-001",
        "pipeline_phase": 0,
    });
    let result = tool_pipeline_upsert(&args, None).await;
    // Should fail on backend check before field validation, but if backend were present
    // it would fail on missing pipeline_agent. Test the backend-absent path first.
    assert!(result.is_err());
}

// Regression tests for rosary-b0b69a: exercises the same parse_bool_arg
// helper that call_tool uses, so regressions are caught.

#[test]
fn run_once_dry_run_defaults_to_false() {
    assert!(
        !parse_bool_arg(&json!({}), "dry_run", false),
        "dry_run must default to false — MCP dispatch won't work otherwise"
    );
}

#[test]
fn run_once_dry_run_explicit_true() {
    assert!(parse_bool_arg(&json!({"dry_run": true}), "dry_run", false));
}

#[test]
fn run_once_dry_run_explicit_false() {
    assert!(!parse_bool_arg(
        &json!({"dry_run": false}),
        "dry_run",
        false
    ));
}

#[test]
fn run_once_dry_run_string_value_defaults_to_false() {
    // If a client sends "false" as a string, as_bool() returns None
    assert!(
        !parse_bool_arg(&json!({"dry_run": "false"}), "dry_run", false),
        "string 'false' must not become true"
    );
}

// Tests for user_id propagation through CallerIdentity -> tool_run_once ->
// ReconcilerConfig.user_id.  We test the user_scope() extraction layer (the
// only part that can be unit-tested without loading config or running the
// reconciler) to lock in the contract: authenticated callers produce
// Some(user_id), anonymous/machine callers produce None.

#[test]
fn user_scope_authenticated_user_yields_some() {
    let id = super::super::CallerIdentity::User("alice".to_string());
    assert_eq!(id.user_scope(), Some("alice"));
}

#[test]
fn user_scope_machine_as_user_yields_some() {
    let id = super::super::CallerIdentity::MachineAsUser {
        user_id: "bob".to_string(),
        service: "ingester".to_string(),
    };
    assert_eq!(id.user_scope(), Some("bob"));
}

#[test]
fn user_scope_machine_yields_none() {
    let id = super::super::CallerIdentity::Machine("ingester".to_string());
    assert_eq!(
        id.user_scope(),
        None,
        "machine-level service must not scope to a user"
    );
}

#[test]
fn user_scope_anonymous_yields_none() {
    let id = super::super::CallerIdentity::Anonymous;
    assert_eq!(
        id.user_scope(),
        None,
        "anonymous/CLI callers must not scope to a user"
    );
}

#[tokio::test]
async fn expand_ref_returns_stored_blob() {
    let tmp = tempfile::TempDir::new().unwrap();
    // Seed a blob through a RefStore rooted at the cas dir the tool reads.
    let mut rs = crate::context::ref_store::RefStore::new(
        leyline_core::FsBlobStore::open(tmp.path()).unwrap(),
    );
    let hash = rs.put(b"demoted phase body").unwrap();

    let args = serde_json::json!({ "hash": hash, "cas_dir": tmp.path().to_str().unwrap() });
    let out = super::tool_expand_ref(&args).await.unwrap();
    assert_eq!(out["content"].as_str().unwrap(), "demoted phase body");

    let miss =
        serde_json::json!({ "hash": "0".repeat(64), "cas_dir": tmp.path().to_str().unwrap() });
    assert!(super::tool_expand_ref(&miss).await.unwrap()["content"].is_null());
}
