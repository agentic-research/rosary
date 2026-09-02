use super::*;
use crate::store::LinkageStore;
use serde_json::json;

fn empty_pool() -> crate::pool::RepoPool {
    crate::pool::RepoPool::empty()
}

// Validation fires before DB access, so an empty pool + nonexistent path is fine.
const FAKE_REPO: &str = "/nonexistent/repo";

#[test]
fn native_session_health_uses_session_ref_without_pid() {
    let session = crate::session::SessionEntry {
        bead_id: "rosary-native".into(),
        repo: "rosary".into(),
        provider: "codex".into(),
        pid: None,
        session_ref: Some(crate::dispatch::AgentSessionRef::new("codex", "thread-123")),
        work_dir: "/tmp/native".into(),
        started_at: chrono::Utc::now(),
        title: "Native session".into(),
        agent: "dev-agent".into(),
        workspace_vcs: "git".into(),
        repo_path: "/tmp/repo".into(),
        last_activity: None,
        last_comment: None,
    };

    assert_eq!(check_agent_health(&session), "healthy");
}

// ---- tool_bead_create --------------------------------------------------

#[tokio::test]
async fn create_rejects_blank_title() {
    let args = json!({ "repo_path": FAKE_REPO, "title": "   " });
    let err = tool_bead_create(&args, &empty_pool(), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("blank"), "{err}");
}

#[tokio::test]
async fn create_rejects_title_too_long() {
    let long = "x".repeat(TITLE_MAX_LEN + 1);
    let args = json!({ "repo_path": FAKE_REPO, "title": long });
    let err = tool_bead_create(&args, &empty_pool(), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("exceeds"), "{err}");
}

#[tokio::test]
async fn create_rejects_priority_out_of_range() {
    let args = json!({ "repo_path": FAKE_REPO, "title": "T", "priority": 4 });
    let err = tool_bead_create(&args, &empty_pool(), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("priority must be 0"), "{err}");
}

#[tokio::test]
async fn create_rejects_priority_wrong_type() {
    let args = json!({ "repo_path": FAKE_REPO, "title": "T", "priority": "high" });
    let err = tool_bead_create(&args, &empty_pool(), None)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("priority must be an integer"),
        "{err}"
    );
}

#[tokio::test]
async fn create_rejects_unknown_issue_type() {
    let args = json!({ "repo_path": FAKE_REPO, "title": "T", "issue_type": "story" });
    let err = tool_bead_create(&args, &empty_pool(), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("unknown issue_type"), "{err}");
}

#[tokio::test]
async fn create_rejects_issue_type_wrong_type() {
    let args = json!({ "repo_path": FAKE_REPO, "title": "T", "issue_type": 42 });
    let err = tool_bead_create(&args, &empty_pool(), None)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("issue_type must be a string"),
        "{err}"
    );
}

#[tokio::test]
async fn create_rejects_unknown_work_mode() {
    let args = json!({ "repo_path": FAKE_REPO, "title": "T", "work_mode": "story" });
    let err = tool_bead_create(&args, &empty_pool(), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("unknown work_mode"), "{err}");
}

#[tokio::test]
async fn create_rejects_work_mode_wrong_type() {
    let args = json!({ "repo_path": FAKE_REPO, "title": "T", "work_mode": 42 });
    let err = tool_bead_create(&args, &empty_pool(), None)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("work_mode must be a string"),
        "{err}"
    );
}

#[tokio::test]
async fn create_accepts_work_mode_before_repo_resolution() {
    let args = json!({ "repo_path": FAKE_REPO, "title": "T", "work_mode": "investigation" });
    let err = tool_bead_create(&args, &empty_pool(), None)
        .await
        .unwrap_err();
    assert!(
        !err.to_string().contains("unknown work_mode"),
        "known work_mode must pass validation before fake repo lookup: {err}"
    );
    assert!(
        !err.to_string().contains("files required"),
        "investigation work_mode should default to research, not task: {err}"
    );
}

#[tokio::test]
async fn create_defaults_missing_close_condition_rather_than_rejecting() {
    // A missing close condition no longer fails authoring — it's defaulted
    // (PR-merge) so bare create works, 1:1 across CLI + MCP. Validation
    // passes it through; the only error here is the fake repo's store
    // access, NOT a "no close condition" rejection.
    let args = json!({
        "repo_path": FAKE_REPO, "title": "T", "issue_type": "task", "files": ["a.rs"]
    });
    let err = tool_bead_create(&args, &empty_pool(), None)
        .await
        .unwrap_err();
    assert!(
        !err.to_string().contains("no close condition"),
        "missing close condition must be defaulted, not rejected: {err}"
    );
}

#[tokio::test]
async fn dispatch_rejects_missing_close_condition_before_provider_resolution() {
    let repo = crate::testutil::TestRepo::new();
    let store = crate::bead_sqlite::connect_bead_store(&repo.path().join(".beads"))
        .await
        .unwrap();
    store
        .create_bead_full(crate::store::NewBead {
            id: "rsry-mcp-no-close".into(),
            title: "MCP dispatch without close condition".into(),
            description: "No runnable close condition here.".into(),
            priority: 1,
            issue_type: "task".into(),
            owner: "dev-agent".into(),
            files: vec!["src/serve/handlers.rs".into()],
            created_by: Some("test".into()),
            ..Default::default()
        })
        .await
        .unwrap();

    let repo_path = repo.path().to_string_lossy().to_string();
    let args = json!({
        "repo_path": repo_path,
        "bead_id": "rsry-mcp-no-close",
        "provider": "not-a-provider",
        "isolate": false
    });
    let err = tool_dispatch(&args, "rosary.toml").await.unwrap_err();

    assert!(err.to_string().contains("no close condition"), "{err}");
    assert_eq!(
        store
            .get_status("rsry-mcp-no-close")
            .await
            .unwrap()
            .as_deref(),
        Some("open"),
        "MCP dispatch must not create workspace/session state for unclosable beads"
    );
}

#[tokio::test]
async fn bead_close_rejects_missing_verifiable_test_command() {
    let repo = crate::testutil::TestRepo::new();
    let store = crate::bead_sqlite::connect_bead_store(&repo.path().join(".beads"))
        .await
        .unwrap();
    store
        .create_bead_full(crate::store::NewBead {
            id: "rsry-close-no-test".into(),
            title: "MCP close without close condition".into(),
            description: "No runnable close command here.".into(),
            priority: 1,
            issue_type: "task".into(),
            owner: "dev-agent".into(),
            files: vec!["src/serve/handlers.rs".into()],
            created_by: Some("test".into()),
            ..Default::default()
        })
        .await
        .unwrap();

    let repo_path = repo.path().to_string_lossy().to_string();
    let args = json!({
        "repo_path": repo_path,
        "id": "rsry-close-no-test",
    });
    let err = tool_bead_close(&args, &empty_pool(), None)
        .await
        .unwrap_err();

    assert!(
        err.to_string().contains("no verifiable test command"),
        "{err}"
    );
    assert_eq!(
        store
            .get_status("rsry-close-no-test")
            .await
            .unwrap()
            .as_deref(),
        Some("open"),
        "MCP close must not close an implementation bead without a close gate"
    );
}

#[tokio::test]
async fn bead_close_unresolvable_suffix_does_not_trigger_close_gate() {
    let repo = crate::testutil::TestRepo::new();
    let store = crate::bead_sqlite::connect_bead_store(&repo.path().join(".beads"))
        .await
        .unwrap();
    store
        .create_bead_full(crate::store::NewBead {
            id: "rsry-close-no-test".into(),
            title: "MCP close without close condition".into(),
            description: "No runnable close command here.".into(),
            priority: 1,
            issue_type: "task".into(),
            owner: "dev-agent".into(),
            files: vec!["src/serve/handlers.rs".into()],
            created_by: Some("test".into()),
            ..Default::default()
        })
        .await
        .unwrap();

    let repo_path = repo.path().to_string_lossy().to_string();
    let args = json!({
        "repo_path": repo_path,
        "id": "est",
    });
    let err = tool_bead_close(&args, &empty_pool(), None)
        .await
        .unwrap_err();

    assert!(
        err.to_string().contains("bead not found: est"),
        "unresolvable partial suffixes should fall through to the store resolver; got: {err}"
    );
    assert_eq!(
        store
            .get_status("rsry-close-no-test")
            .await
            .unwrap()
            .as_deref(),
        Some("open")
    );
}

#[tokio::test]
async fn bead_close_force_bypasses_verifiable_test_command_gate() {
    let repo = crate::testutil::TestRepo::new();
    let store = crate::bead_sqlite::connect_bead_store(&repo.path().join(".beads"))
        .await
        .unwrap();
    store
        .create_bead_full(crate::store::NewBead {
            id: "rsry-close-force".into(),
            title: "MCP close force".into(),
            description: "No runnable close command here.".into(),
            priority: 1,
            issue_type: "task".into(),
            owner: "dev-agent".into(),
            files: vec!["src/serve/handlers.rs".into()],
            created_by: Some("test".into()),
            ..Default::default()
        })
        .await
        .unwrap();

    let repo_path = repo.path().to_string_lossy().to_string();
    let args = json!({
        "repo_path": repo_path,
        "id": "rsry-close-force",
        "force": true,
    });
    tool_bead_close(&args, &empty_pool(), None).await.unwrap();

    assert_eq!(
        store
            .get_status("rsry-close-force")
            .await
            .unwrap()
            .as_deref(),
        Some("done")
    );
}

// ---- tool_bead_update --------------------------------------------------

#[tokio::test]
async fn update_rejects_blank_title() {
    let args = json!({ "repo_path": FAKE_REPO, "id": "x-1", "title": "" });
    let err = tool_bead_update(&args, &empty_pool(), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("blank"), "{err}");
}

#[tokio::test]
async fn update_rejects_priority_wrong_type() {
    let args = json!({ "repo_path": FAKE_REPO, "id": "x-1", "priority": -1 });
    let err = tool_bead_update(&args, &empty_pool(), None)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("priority must be an integer"),
        "{err}"
    );
}

#[tokio::test]
async fn update_rejects_out_of_range_priority() {
    let args = json!({ "repo_path": FAKE_REPO, "id": "x-1", "priority": 99 });
    let err = tool_bead_update(&args, &empty_pool(), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("priority must be 0"), "{err}");
}

#[tokio::test]
async fn update_rejects_unknown_issue_type() {
    let args = json!({ "repo_path": FAKE_REPO, "id": "x-1", "issue_type": "spike" });
    let err = tool_bead_update(&args, &empty_pool(), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("unknown issue_type"), "{err}");
}

// ---- tool_bead_comment -------------------------------------------------

#[tokio::test]
async fn comment_rejects_blank_body() {
    let args = json!({ "repo_path": FAKE_REPO, "id": "x-1", "body": "  " });
    let err = tool_bead_comment(&args, &empty_pool(), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("blank"), "{err}");
}

#[tokio::test]
async fn comment_rejects_oversized_body() {
    let big = "a".repeat(BODY_MAX_LEN + 1);
    let args = json!({ "repo_path": FAKE_REPO, "id": "x-1", "body": big });
    let err = tool_bead_comment(&args, &empty_pool(), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("exceeds"), "{err}");
}

// ---- tool_bead_link ----------------------------------------------------

#[tokio::test]
async fn link_rejects_self_dependency() {
    let args = json!({ "repo_path": FAKE_REPO, "id": "x-1", "depends_on": "x-1" });
    let err = tool_bead_link(&args, &empty_pool(), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("cannot depend on itself"), "{err}");
}

/// rosary-98b11d: when the caller passes the common-but-wrong
/// shorthand `{from_id, to_id, link_type}`, the error must name
/// the canonical parameters (`id`, `depends_on`) so the caller
/// doesn't have to guess three times to discover the real names.
#[tokio::test]
async fn link_error_names_canonical_params_on_missing_id() {
    let args = json!({
        "repo_path": FAKE_REPO,
        "from_id": "a-1",
        "to_id": "b-1",
        "link_type": "blocks"
    });
    let err = tool_bead_link(&args, &empty_pool(), None)
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("id") && msg.contains("depends_on"),
        "error must name both 'id' and 'depends_on'; got: {msg}"
    );
}

/// Sibling: caller passed `id` correctly but used a wrong-name
/// for the target. The `depends_on required` error must still
/// surface the canonical name so callers don't loop on a second
/// schema-discovery cycle.
#[tokio::test]
async fn link_error_names_canonical_params_on_missing_depends_on() {
    let args = json!({
        "repo_path": FAKE_REPO,
        "id": "a-1",
        "to_id": "b-1"
    });
    let err = tool_bead_link(&args, &empty_pool(), None)
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("depends_on"),
        "error must name 'depends_on'; got: {msg}"
    );
}

/// rosary-98ee93: when `depends_on` carries a `<repo>-<6hex>` prefix
/// matching a repo other than the calling `repo_path`, the handler
/// must auto-route through LinkageStore. Callers shouldn't have to
/// remember the explicit `cross_repo` argument shape — the canonical
/// bead-id namespace already encodes the target repo (per
/// `generate_bead_id`'s `<repo>-<6hex>` convention).
#[tokio::test]
async fn link_auto_routes_cross_repo_via_depends_on_prefix() {
    use crate::store::tests::InMemoryStore;

    let store = InMemoryStore::new();
    let args = json!({
        "repo_path": "/Users/test/cloister",
        "id": "cloister-963a5c",
        "depends_on": "signet-9605a3",
    });
    let result = tool_bead_link(&args, &empty_pool(), Some(&store)).await;
    assert!(
        result.is_ok(),
        "auto-routed cross-repo link must succeed via LinkageStore; got: {result:?}"
    );
    let deps = store
        .dependencies_of(&crate::store::WorkRef {
            repo: "cloister".into(),
            scope: String::new(),
            bead_id: "cloister-963a5c".into(),
        })
        .await
        .expect("query dependencies_of");
    assert!(
        deps.iter().any(|d| d.to.bead_id == "signet-9605a3"),
        "cross-repo dep must be present in LinkageStore; got: {deps:?}"
    );
}

/// rosary-b5da2f PR 4: `tool_bead_link` accepts a canonical `scope`
/// arg in place of `repo_path`. This is the first MCP handler
/// converted to use the new `resolve_scope` boundary parser. The
/// LinkageStore write path must accept `scope: "repo:cloister"`
/// equivalently to `repo_path: "/Users/.../cloister"`.
#[tokio::test]
async fn link_accepts_scope_arg_in_place_of_repo_path() {
    use crate::store::tests::InMemoryStore;

    let store = InMemoryStore::new();
    // No `repo_path`; only `scope` in canonical form.
    let args = json!({
        "scope": "repo:cloister",
        "id": "cloister-963a5c",
        "depends_on": "signet-9605a3",
    });
    let result = tool_bead_link(&args, &empty_pool(), Some(&store)).await;
    assert!(
        result.is_ok(),
        "scope arg must work in place of repo_path for cross-repo deps; got: {result:?}"
    );
    let deps = store
        .dependencies_of(&crate::store::WorkRef {
            repo: "cloister".into(),
            scope: String::new(),
            bead_id: "cloister-963a5c".into(),
        })
        .await
        .expect("query dependencies_of");
    assert!(
        deps.iter().any(|d| d.to.bead_id == "signet-9605a3"),
        "cross-repo dep must land in LinkageStore when scope was used; got: {deps:?}"
    );
}

/// rosary-d7a98e: a *same-repo* dep expressed with `scope` alone (no
/// `repo_path`) must resolve the per-repo store through the pool — the
/// canonical scope API was previously not enough for same-repo links, which
/// still demanded `repo_path`.
#[tokio::test]
async fn link_scope_only_resolves_same_repo_dep_via_pool() {
    let repo = crate::testutil::TestRepo::new();
    let beads_dir = repo.path().join(".beads");
    // Seed the two beads (add_dependency resolves both ids).
    {
        let store = crate::bead_sqlite::connect_bead_store(&beads_dir)
            .await
            .unwrap();
        for id in ["myrepo-a", "myrepo-b"] {
            store
                .create_bead_full(crate::store::NewBead {
                    id: id.into(),
                    title: id.into(),
                    ..Default::default()
                })
                .await
                .unwrap();
        }
    }
    let store = crate::bead_sqlite::connect_bead_store(&beads_dir)
        .await
        .unwrap();
    let pool = crate::pool::RepoPool::from_client("myrepo", repo.path().to_path_buf(), store);

    // Scope only — no `repo_path`. `depends_on` carries the calling repo's
    // prefix so it stays a same-repo (not cross-repo) link.
    let args = json!({
        "scope": "repo:myrepo",
        "id": "myrepo-a",
        "depends_on": "myrepo-b",
    });
    let result = tool_bead_link(&args, &pool, None).await;
    assert!(
        result.is_ok(),
        "scope-only same-repo link must resolve via the pool; got: {result:?}"
    );

    // The dep landed in the pooled per-repo store.
    let store2 = crate::bead_sqlite::connect_bead_store(&beads_dir)
        .await
        .unwrap();
    let deps = store2.get_dependencies("myrepo-a").await.unwrap();
    assert_eq!(deps, vec!["myrepo-b"]);
}

/// rosary-b5da2f PR 4: `Global` scope can write cross-repo deps via
/// the LinkageStore bridge — meta-beads (the future incoming triage
/// queue per `rosary-1db9c9`) need to express deps without a
/// per-repo backing store. The `from.repo` field stores the
/// reserved `"global"` namespace (per `ScopeId::work_ref`).
#[tokio::test]
async fn link_from_global_scope_routes_via_linkage_store() {
    use crate::store::tests::InMemoryStore;

    let store = InMemoryStore::new();
    let args = json!({
        "scope": "global",
        "id": "global-meta-001",
        "depends_on": "signet-9605a3",
        // Explicit cross_repo because Global has no bead-id prefix
        // to auto-detect from.
        "cross_repo": "signet/signet-9605a3",
    });
    let result = tool_bead_link(&args, &empty_pool(), Some(&store)).await;
    assert!(
        result.is_ok(),
        "Global scope must support cross-repo deps via cross_repo; got: {result:?}"
    );
    let deps = store
        .dependencies_of(&crate::store::WorkRef {
            repo: "global".into(),
            scope: String::new(),
            bead_id: "global-meta-001".into(),
        })
        .await
        .expect("query dependencies_of from global scope");
    assert!(
        deps.iter().any(|d| d.to.bead_id == "signet-9605a3"),
        "Global → signet dep must land; got: {deps:?}"
    );
}

/// Copilot #212 finding: `cross_repo` target must NOT silently
/// accept reserved-namespace strings (`"global"`, `"external:..."`)
/// as if they were repo names — that would create rows in the
/// reserved namespace via the wrong-side arg, breaking the
/// round-trip invariant where Global-scope rows can only be
/// produced via `ScopeId::Global`.
#[tokio::test]
async fn link_rejects_global_namespace_in_cross_repo_target() {
    use crate::store::tests::InMemoryStore;
    let store = InMemoryStore::new();
    let args = json!({
        "scope": "repo:cloister",
        "id": "cloister-963a5c",
        "depends_on": "signet-9605a3",
        "cross_repo": "global/some-bead",   // RESERVED — must reject
    });
    let err = tool_bead_link(&args, &empty_pool(), Some(&store))
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("global") || msg.contains("reserved"),
        "error must name the reserved namespace; got: {msg}"
    );
}

/// Copilot #212 finding: same guard for the `external:` reserved
/// prefix in cross_repo. Parsing `"external:foo"` as `ScopeId`
/// produces `External(_)`, not `Repo(_)` — and cross_repo is a
/// repo-to-repo edge today.
#[tokio::test]
async fn link_rejects_external_namespace_in_cross_repo_target() {
    use crate::store::tests::InMemoryStore;
    let store = InMemoryStore::new();
    let args = json!({
        "scope": "repo:cloister",
        "id": "cloister-963a5c",
        "depends_on": "signet-9605a3",
        "cross_repo": "external:zen://foo/some-bead",
    });
    let err = tool_bead_link(&args, &empty_pool(), Some(&store))
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("external") || msg.contains("reserved"),
        "error must name the reserved namespace; got: {msg}"
    );
}

/// rosary-b5da2f PR 4: same-repo deps (no `cross_repo`, no
/// `depends_on` prefix match) from `External` or `Global` scope
/// don't make sense — they have no per-repo Dolt store. Must
/// error with an actionable message pointing the caller at the
/// `cross_repo` arg.
#[tokio::test]
async fn link_errors_on_same_repo_dep_from_global_scope() {
    use crate::store::tests::InMemoryStore;

    let store = InMemoryStore::new();
    let args = json!({
        "scope": "global",
        "id": "global-001",
        "depends_on": "global-002",   // Looks same-scope; no auto-route.
    });
    let err = tool_bead_link(&args, &empty_pool(), Some(&store))
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("global") || msg.contains("Global"),
        "error must surface that Global scope can't do same-scope deps; got: {msg}"
    );
    assert!(
        msg.contains("cross_repo") || msg.contains("LinkageStore"),
        "error must point at cross_repo arg as the alternative; got: {msg}"
    );
}

/// Same-repo deps (where `depends_on`'s repo prefix matches the
/// calling `repo_path`) must NOT route through LinkageStore — they
/// stay on the per-repo Dolt path. Otherwise we'd silently divert
/// every same-repo link to the cross-repo store and confuse the
/// existing `add_dependency` semantics.
#[tokio::test]
async fn link_same_repo_does_not_auto_route_to_linkage_store() {
    use crate::store::tests::InMemoryStore;

    let store = InMemoryStore::new();
    // Both beads in the same repo (`cloister`). Should NOT touch
    // LinkageStore. With no Dolt pool wired here, the same-repo
    // path will Err — that's expected and proves auto-route did
    // not engage (otherwise it'd succeed via the store).
    let args = json!({
        "repo_path": "/Users/test/cloister",
        "id": "cloister-963a5c",
        "depends_on": "cloister-aaaaaa",
    });
    let _ = tool_bead_link(&args, &empty_pool(), Some(&store)).await;
    let deps = store
        .dependencies_of(&crate::store::WorkRef {
            repo: "cloister".into(),
            scope: String::new(),
            bead_id: "cloister-963a5c".into(),
        })
        .await
        .expect("query dependencies_of");
    assert!(
        deps.is_empty(),
        "same-repo links must NOT route through LinkageStore; got: {deps:?}"
    );
}

// ---- scope-arg acceptance per converted handler (rosary-b5da2f PR 6) --
//
// These tests are the repeatable test harness the user asked for:
// each converted handler must accept `scope: "repo:<name>"` as a
// substitute for `repo_path: "/path/to/repo"`. The empty_pool +
// FAKE_REPO pattern means resolve_repo_client falls to get_client
// which itself errors on FS lookup — what we're pinning is that
// **the error is NOT** `"repo_path required"`, which would mean
// the handler is still doing bespoke arg parsing rather than
// delegating to resolve_repo_client.

/// Helper: assert that the error doesn't come from the legacy
/// `repo_path required` path (i.e. handler is wired to
/// resolve_repo_client and the scope arg flowed through).
fn assert_scope_path_engaged(err: &anyhow::Error) {
    let msg = err.to_string();
    assert!(
        !msg.contains("repo_path required"),
        "handler must delegate to resolve_repo_client when only `scope` is passed; \
             error names the legacy parser instead: {msg}"
    );
}

#[tokio::test]
async fn bead_create_accepts_scope_arg() {
    let args = json!({
        "scope": "repo:nonexistent",
        "title": "Test bead",
        "issue_type": "task",
        "files": ["a.rs"],
        "force": true, // bypass close-condition gate; this test exercises scope resolution
    });
    let err = tool_bead_create(&args, &empty_pool(), None)
        .await
        .unwrap_err();
    assert_scope_path_engaged(&err);
}

#[tokio::test]
async fn bead_update_accepts_scope_arg() {
    let args = json!({
        "scope": "repo:nonexistent",
        "id": "x-1",
        "title": "new title",
    });
    let err = tool_bead_update(&args, &empty_pool(), None)
        .await
        .unwrap_err();
    assert_scope_path_engaged(&err);
}

#[tokio::test]
async fn bead_close_accepts_scope_arg() {
    let args = json!({
        "scope": "repo:nonexistent",
        "id": "x-1",
    });
    let err = tool_bead_close(&args, &empty_pool(), None)
        .await
        .unwrap_err();
    assert_scope_path_engaged(&err);
}

#[tokio::test]
async fn bead_comment_accepts_scope_arg() {
    let args = json!({
        "scope": "repo:nonexistent",
        "id": "x-1",
        "body": "test",
    });
    let err = tool_bead_comment(&args, &empty_pool(), None)
        .await
        .unwrap_err();
    assert_scope_path_engaged(&err);
}

#[tokio::test]
async fn bead_comment_list_accepts_scope_arg() {
    let args = json!({ "scope": "repo:nonexistent", "id": "x-1" });
    let err = tool_bead_comment_list(&args, &empty_pool(), None)
        .await
        .unwrap_err();
    assert_scope_path_engaged(&err);
}

#[tokio::test]
async fn bead_comment_update_accepts_scope_arg() {
    let args = json!({
        "scope": "repo:nonexistent",
        "comment_id": "c-1",
        "body": "edited",
    });
    let err = tool_bead_comment_update(&args, &empty_pool(), None)
        .await
        .unwrap_err();
    assert_scope_path_engaged(&err);
}

#[tokio::test]
async fn bead_comment_delete_accepts_scope_arg() {
    let args = json!({ "scope": "repo:nonexistent", "comment_id": "c-1" });
    let err = tool_bead_comment_delete(&args, &empty_pool(), None)
        .await
        .unwrap_err();
    assert_scope_path_engaged(&err);
}

#[tokio::test]
async fn bead_search_accepts_scope_arg() {
    let args = json!({ "scope": "repo:nonexistent", "query": "auth" });
    let err = tool_bead_search(&args, &empty_pool(), None)
        .await
        .unwrap_err();
    assert_scope_path_engaged(&err);
}

/// Cross-cutting: every converted handler MUST surface the
/// resolve_scope error when neither `scope` nor `repo_path` is
/// provided — error names BOTH accepted args so callers know
/// either is valid. Regression catch for any handler that
/// reintroduces bespoke `args["repo_path"].ok_or_else(...)` logic.
#[tokio::test]
async fn all_converted_handlers_surface_resolve_scope_error() {
    // Args that pass all per-handler validation EXCEPT scope/repo_path,
    // so the resolve_scope-missing-arg path is the actual failure
    // point in every case.
    let bare_args_pairs = [
        (
            "bead_create",
            json!({
                "title": "x", "issue_type": "task", "files": ["a.rs"], "force": true
            }),
        ),
        ("bead_update", json!({"id": "x-1", "title": "x"})),
        ("bead_close", json!({"id": "x-1"})),
        ("bead_comment", json!({"id": "x-1", "body": "x"})),
        ("bead_comment_list", json!({"id": "x-1"})),
        (
            "bead_comment_update",
            json!({"comment_id": "c-1", "body": "x"}),
        ),
        ("bead_comment_delete", json!({"comment_id": "c-1"})),
        ("bead_search", json!({"query": "x"})),
    ];

    for (handler_name, args) in bare_args_pairs {
        let result = match handler_name {
            "bead_create" => tool_bead_create(&args, &empty_pool(), None).await,
            "bead_update" => tool_bead_update(&args, &empty_pool(), None).await,
            "bead_close" => tool_bead_close(&args, &empty_pool(), None).await,
            "bead_comment" => tool_bead_comment(&args, &empty_pool(), None).await,
            "bead_comment_list" => tool_bead_comment_list(&args, &empty_pool(), None).await,
            "bead_comment_update" => tool_bead_comment_update(&args, &empty_pool(), None).await,
            "bead_comment_delete" => tool_bead_comment_delete(&args, &empty_pool(), None).await,
            "bead_search" => tool_bead_search(&args, &empty_pool(), None).await,
            _ => unreachable!(),
        };
        let err = match result {
            Ok(_) => panic!("handler `{handler_name}` accepted args with no scope/repo_path"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("scope") && msg.contains("repo_path"),
            "handler `{handler_name}` error must list both accepted args; got: {msg}"
        );
    }
}

// ---- resolve_repo_client (rosary-b5da2f PR 5) -------------------------

/// `resolve_repo_client` rejects `Global` scope with a message
/// naming the supported addressing paths — Global is identifier-
/// only, no per-repo bead store.
#[tokio::test]
async fn resolve_repo_client_rejects_global_scope() {
    let args = json!({ "scope": "global", "id": "x", "depends_on": "y" });
    let err = match resolve_repo_client(&args, &empty_pool()).await {
        Ok(_) => panic!("resolve_repo_client must reject; returned Ok"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(
        msg.contains("global") || msg.contains("Global"),
        "error must name the rejected scope; got: {msg}"
    );
    assert!(
        msg.contains("Repo-only") || msg.contains("Personal") || msg.contains("LinkageStore"),
        "error must explain the right alternative addressing; got: {msg}"
    );
}

/// Same guard for `External` scope.
#[tokio::test]
async fn resolve_repo_client_rejects_external_scope() {
    let args = json!({ "scope": "external:zen://inbox", "id": "x" });
    let err = match resolve_repo_client(&args, &empty_pool()).await {
        Ok(_) => panic!("resolve_repo_client must reject; returned Ok"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(
        msg.contains("external") || msg.contains("External"),
        "error must name the rejected scope; got: {msg}"
    );
}

/// When `scope: "repo:foo"` is passed but `foo` isn't in the pool
/// AND no `repo_path` is provided, the error must point the caller
/// at the two recovery paths: register the repo, or pass repo_path.
#[tokio::test]
async fn resolve_repo_client_errors_when_repo_not_in_pool_and_no_repo_path() {
    let args = json!({ "scope": "repo:unloaded-repo" });
    let err = match resolve_repo_client(&args, &empty_pool()).await {
        Ok(_) => panic!("resolve_repo_client must reject; returned Ok"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(
        msg.contains("unloaded-repo"),
        "error must name the missing repo; got: {msg}"
    );
    assert!(
        msg.contains("rsry_repo_register") || msg.contains("repo_path"),
        "error must surface the two recovery paths; got: {msg}"
    );
}

/// rosary-ea412f (friction #5): filing into an unregistered repo whose
/// `.beads/` store can't be opened (missing / read-only from this
/// workspace, e.g. `repo:agents`) must fail with a DETERMINISTIC
/// "not registered → register it" message, not a raw filesystem errno.
#[tokio::test]
async fn get_client_unregistered_unwritable_repo_gives_deterministic_error() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("agents");
    std::fs::create_dir(&repo).unwrap();
    // Block the store: put a regular file where `.beads/` must be a dir, so
    // connect_bead_store fails deterministically on every platform.
    std::fs::write(repo.join(".beads"), b"not a directory").unwrap();

    let err = match get_client(repo.to_str().unwrap(), &empty_pool()).await {
        Ok(_) => panic!("opening a blocked bead store must fail"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(
        msg.contains("not registered") && msg.contains("rsry_repo_register"),
        "must be a deterministic 'register the repo' error, not a raw FS error; got: {msg}"
    );
}

/// `resolve_repo_client` is the unified arg-parser; both `scope`
/// and `repo_path` paths must error symmetrically when *neither*
/// arg is provided. Delegates to `resolve_scope` for the message
/// shape (already TDD'd in `serve::scope_args::tests`); this test
/// pins the delegation contract.
#[tokio::test]
async fn resolve_repo_client_errors_when_neither_scope_nor_repo_path() {
    let args = json!({ "id": "x" });
    let err = match resolve_repo_client(&args, &empty_pool()).await {
        Ok(_) => panic!("resolve_repo_client must reject; returned Ok"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(
        msg.contains("scope") && msg.contains("repo_path"),
        "error must list both accepted args (delegated to resolve_scope); got: {msg}"
    );
}

/// Copilot #213 finding: when both `scope` and `repo_path` are
/// passed AND they name different repos, the resolver MUST reject
/// the pair. Otherwise the caller could operate on repo A's store
/// while labeling all writes with scope B — silent mis-attribution.
/// The fix engages whether or not the scope-named repo is in the
/// pool: the path's basename must match the scope's repo name.
#[tokio::test]
async fn resolve_repo_client_rejects_scope_path_mismatch() {
    let args = json!({
        "scope": "repo:cloister",
        "repo_path": "/Users/test/signet",   // basename = "signet" ≠ "cloister"
    });
    let err = match resolve_repo_client(&args, &empty_pool()).await {
        Ok(_) => panic!("resolver must reject mismatched scope/path; returned Ok"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(
        msg.contains("cloister") && msg.contains("signet"),
        "error must name both the scope's repo and the path's basename so the operator \
             can see the disagreement; got: {msg}"
    );
    assert!(
        msg.contains("disagree") || msg.contains("mismatch"),
        "error must explicitly call out the mismatch; got: {msg}"
    );
}

/// When the scope-named repo is in the pool AND a non-conflicting
/// repo_path is passed (e.g. same path or absent), the pool lookup
/// takes priority. This pins that the mismatch guard ONLY fires on
/// actual disagreement, not on redundant/consistent specification.
#[tokio::test]
async fn resolve_repo_client_accepts_matching_scope_and_repo_path() {
    // Both args specify "cloister" (scope canonical + path basename).
    // No pool entry, so falls to repo_path. FAKE_REPO uses
    // /nonexistent/repo (basename "repo") so we can't use it here;
    // use a path whose basename matches the scope.
    let args = json!({
        "scope": "repo:cloister",
        "repo_path": "/Users/test/cloister",  // basename matches scope
    });
    // FS lookup at /Users/test/cloister will fail (path doesn't
    // exist), but we want to see the resolver get PAST the
    // mismatch guard and into the get_client path — that's the
    // success criterion for THIS test.
    let err = match resolve_repo_client(&args, &empty_pool()).await {
        Ok(_) => return, // pool/FS happened to resolve; fine
        Err(e) => e,
    };
    let msg = err.to_string();
    // The error must NOT be the mismatch guard — that would prove
    // the guard fires on consistent specification (false positive).
    assert!(
        !msg.contains("disagree") && !msg.contains("mismatch"),
        "matching scope+path must not trigger the mismatch guard; got: {msg}"
    );
}

/// When `repo_path` (legacy arg) is passed alone, `resolve_repo_client`
/// must still work — that's the back-compat contract for the 13
/// handlers about to migrate. Uses FAKE_REPO path to exercise the
/// fallback to `get_client` (which itself will error on FS lookup,
/// but resolve_repo_client gets that far cleanly).
#[tokio::test]
async fn resolve_repo_client_falls_back_to_repo_path() {
    let args = json!({ "repo_path": FAKE_REPO });
    // The fallback path calls `get_client(FAKE_REPO, ...)` which
    // fails at FS lookup. resolve_repo_client itself is correct;
    // the error must surface from get_client (not the scope
    // parser), proving the fallback engaged.
    let err = match resolve_repo_client(&args, &empty_pool()).await {
        Ok(_) => panic!("resolve_repo_client must reject; returned Ok"),
        Err(e) => e,
    };
    let msg = err.to_string();
    // The error should NOT be a scope-parser error — that would
    // mean resolve_repo_client never reached the fallback.
    assert!(
        !msg.starts_with("scope") && !msg.contains("not loaded in the repo pool"),
        "fallback must reach get_client (not error in scope parsing); got: {msg}"
    );
}

// ---- tool_decade_create / tool_thread_create (rosary-992e79) ----------

/// rosary-992e79: dedicated `rsry_decade_create` returns the created
/// decade's metadata (not just a confirmation), so callers can chain
/// "create decade → create thread under it → file beads" in one
/// session without a separate read step.
#[tokio::test]
async fn decade_create_returns_created_metadata() {
    use crate::store::tests::InMemoryStore;
    let store = InMemoryStore::new();
    let args = json!({
        "id": "substrate-idl",
        "title": "Substrate IDL Decade",
        "source_path": "docs/design/substrate-idl.md",
    });
    let result = tool_decade_create(&args, Some(&store))
        .await
        .expect("decade_create must succeed");
    assert_eq!(result["id"], "substrate-idl");
    assert_eq!(result["title"], "Substrate IDL Decade");
    assert_eq!(result["source_path"], "docs/design/substrate-idl.md");
    assert_eq!(result["status"], "active");
    assert_eq!(result["action"], "created");
}

/// Idempotency: re-creating with the same title + source_path is a
/// no-op success, not an error. Lets agents safely retry without a
/// pre-existence check.
#[tokio::test]
async fn decade_create_is_idempotent_on_identical_payload() {
    use crate::store::tests::InMemoryStore;
    let store = InMemoryStore::new();
    let args = json!({ "id": "d-1", "title": "First", "source_path": "x.md" });
    tool_decade_create(&args, Some(&store))
        .await
        .expect("first");
    let again = tool_decade_create(&args, Some(&store))
        .await
        .expect("re-create with identical payload must succeed");
    assert_eq!(again["action"], "existed");
}

/// Conflict: re-creating with the same id but a DIFFERENT title
/// must error — silent overwrite would let agents stomp curated
/// decade names. The bead's acceptance criteria pin this contract.
#[tokio::test]
async fn decade_create_errors_on_conflicting_title() {
    use crate::store::tests::InMemoryStore;
    let store = InMemoryStore::new();
    let args1 = json!({ "id": "d-1", "title": "First" });
    tool_decade_create(&args1, Some(&store))
        .await
        .expect("first");
    let args2 = json!({ "id": "d-1", "title": "Second" });
    let err = tool_decade_create(&args2, Some(&store)).await.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("d-1") && msg.contains("conflict"),
        "error must name the conflicting id; got: {msg}"
    );
}

/// rosary-992e79: `rsry_thread_create` returns the created thread's
/// metadata including the derived feature_branch (matches the
/// existing thread_assign branch-naming convention).
#[tokio::test]
async fn thread_create_returns_created_metadata() {
    use crate::store::tests::InMemoryStore;
    let store = InMemoryStore::new();
    tool_decade_create(
        &json!({ "id": "d-1", "title": "Test Decade" }),
        Some(&store),
    )
    .await
    .expect("create parent decade");

    let args = json!({
        "decade_id": "d-1",
        "id": "d-1/substrate",
        "name": "Substrate work",
    });
    let result = tool_thread_create(&args, Some(&store))
        .await
        .expect("thread_create must succeed");
    assert_eq!(result["id"], "d-1/substrate");
    assert_eq!(result["name"], "Substrate work");
    assert_eq!(result["decade_id"], "d-1");
    assert_eq!(result["action"], "created");
}

#[tokio::test]
async fn thread_assign_does_not_clobber_existing_thread_decade() {
    // thread_assign assigns a BEAD; it must not redefine the thread.
    // Assigning a bead to an existing thread WITHOUT re-passing decade_id
    // previously upserted the thread with decade_id="ungrouped", clobbering
    // its real decade (the mache session hit this: thread_create under
    // mache-pure-go-arena, then assign → threads moved to ungrouped, so
    // list_threads(decade) returned empty). (rosary-427446)
    use crate::store::HierarchyStore;
    use crate::store::tests::InMemoryStore;
    let store = InMemoryStore::new();
    tool_decade_create(&json!({ "id": "d-x", "title": "X" }), Some(&store))
        .await
        .unwrap();
    tool_thread_create(
        &json!({ "decade_id": "d-x", "id": "d-x/t1", "name": "T1" }),
        Some(&store),
    )
    .await
    .unwrap();

    // Assign a bead WITHOUT decade_id — the normal flow after a create.
    tool_thread_assign(
        &json!({ "thread_id": "d-x/t1", "bead_id": "rosary-1", "repo": "rosary" }),
        Some(&store),
    )
    .await
    .unwrap();

    let under_dx = store.list_threads("d-x").await.unwrap();
    assert!(
        under_dx.iter().any(|t| t.id == "d-x/t1"),
        "thread_assign must not clobber the thread's decade_id"
    );
    let under_ungrouped = store.list_threads("ungrouped").await.unwrap();
    assert!(
        !under_ungrouped.iter().any(|t| t.id == "d-x/t1"),
        "thread must not be moved to ungrouped by assign"
    );
}

/// thread_create must refuse when the parent decade doesn't exist.
/// `thread_assign` auto-creates an `ungrouped` decade as a
/// fall-through, but the explicit create-by-id flow must surface
/// missing parents loudly so agents don't accidentally orphan
/// threads under stub decades.
#[tokio::test]
async fn thread_create_errors_when_parent_decade_missing() {
    use crate::store::tests::InMemoryStore;
    let store = InMemoryStore::new();
    let args = json!({
        "decade_id": "does-not-exist",
        "id": "orphan",
        "name": "Orphan thread",
    });
    let err = tool_thread_create(&args, Some(&store)).await.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("does-not-exist"),
        "error must name the missing parent decade; got: {msg}"
    );
}

/// Idempotency for thread_create: same (decade_id, id, name) is a
/// no-op success — agents can safely retry.
#[tokio::test]
async fn thread_create_is_idempotent_on_identical_payload() {
    use crate::store::tests::InMemoryStore;
    let store = InMemoryStore::new();
    tool_decade_create(&json!({ "id": "d-1", "title": "D" }), Some(&store))
        .await
        .expect("create parent");
    let args = json!({ "decade_id": "d-1", "id": "d-1/t", "name": "T" });
    tool_thread_create(&args, Some(&store))
        .await
        .expect("first");
    let again = tool_thread_create(&args, Some(&store))
        .await
        .expect("re-create with identical payload must succeed");
    assert_eq!(again["action"], "existed");
}

/// Copilot #205 finding: the in-decade existence check in
/// `tool_thread_create` would miss a thread with the same `id`
/// living under a *different* decade, and silently let upsert
/// re-parent it. Global uniqueness across decades is the right
/// contract — otherwise two callers issuing the same thread id
/// against different decades would clobber each other.
#[tokio::test]
async fn thread_create_errors_on_global_id_conflict_across_decades() {
    use crate::store::tests::InMemoryStore;
    let store = InMemoryStore::new();
    tool_decade_create(&json!({ "id": "d-1", "title": "D1" }), Some(&store))
        .await
        .expect("create d-1");
    tool_decade_create(&json!({ "id": "d-2", "title": "D2" }), Some(&store))
        .await
        .expect("create d-2");
    tool_thread_create(
        &json!({ "decade_id": "d-1", "id": "shared-thread-id", "name": "First" }),
        Some(&store),
    )
    .await
    .expect("first thread_create");

    // Same id under a DIFFERENT decade must error — otherwise the
    // second create would silently re-parent the first thread.
    let err = tool_thread_create(
        &json!({ "decade_id": "d-2", "id": "shared-thread-id", "name": "Second" }),
        Some(&store),
    )
    .await
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("shared-thread-id"),
        "error must name the conflicting thread id; got: {msg}"
    );
    assert!(
        msg.contains("d-1") || msg.contains("already exists"),
        "error must surface where the existing thread lives or that it already exists; got: {msg}"
    );
}

/// Copilot #205 finding: when only `source_path` differs (title
/// matches), the conflict message must not falsely claim "conflicting
/// title" — it should name the actual diverging field. This pins the
/// error-message accuracy contract so a reader of the failure isn't
/// misled about what to fix.
#[tokio::test]
async fn decade_create_conflict_message_distinguishes_title_from_source_path() {
    use crate::store::tests::InMemoryStore;
    let store = InMemoryStore::new();
    tool_decade_create(
        &json!({ "id": "d-1", "title": "Same Title", "source_path": "a.md" }),
        Some(&store),
    )
    .await
    .expect("first");
    // Title matches; source_path differs. The error must mention
    // source_path, not just title.
    let err = tool_decade_create(
        &json!({ "id": "d-1", "title": "Same Title", "source_path": "b.md" }),
        Some(&store),
    )
    .await
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("source_path"),
        "error must surface that source_path is the conflicting field, not just title; got: {msg}"
    );
    assert!(
        msg.contains("a.md") && msg.contains("b.md"),
        "error must show both source_paths so the operator can see what changed; got: {msg}"
    );
}

#[tokio::test]
async fn dispatch_record_roundtrips_native_session_ref() {
    use crate::store::tests::InMemoryStore;

    let store = InMemoryStore::new();
    tool_dispatch_record(
        &json!({
            "id": "dispatch-native",
            "repo": "rosary",
            "bead_id": "rosary-native",
            "agent": "dev-agent",
            "provider": "codex",
            "work_dir": "/tmp/native",
            "session_ref": {
                "provider": "codex",
                "id": "thread-123"
            }
        }),
        Some(&store),
    )
    .await
    .expect("record dispatch");

    let history = tool_dispatch_history(
        &json!({ "bead_id": "rosary-native", "active_only": true }),
        Some(&store),
    )
    .await
    .expect("query dispatch history");

    assert_eq!(history["count"], 1);
    assert_eq!(history["dispatches"][0]["session_id"], Value::Null);
    assert_eq!(history["dispatches"][0]["session_ref"]["provider"], "codex");
    assert_eq!(history["dispatches"][0]["session_ref"]["id"], "thread-123");
}

#[tokio::test]
async fn active_includes_backend_pipeline_and_dispatch_state() {
    use crate::store::tests::InMemoryStore;
    use crate::store::{DispatchRecord, DispatchStore, PipelineState, WorkRef};

    let store = InMemoryStore::new();
    let bead_ref = WorkRef {
        repo: "rosary".into(),
        scope: String::new(),
        bead_id: "rosary-backend-active".into(),
    };
    store
        .upsert_pipeline(&PipelineState {
            bead_ref: bead_ref.clone(),
            pipeline_phase: 1,
            pipeline_agent: "staging-agent".into(),
            phase_status: "executing".into(),
            retries: 0,
            consecutive_reverts: 0,
            highest_verify_tier: None,
            last_generation: 0,
            backoff_until: None,
        })
        .await
        .expect("record pipeline");
    store
        .record_dispatch(&DispatchRecord {
            id: "dispatch-backend-active".into(),
            bead_ref,
            agent: "staging-agent".into(),
            provider: "codex".into(),
            started_at: chrono::Utc::now(),
            completed_at: None,
            outcome: None,
            work_dir: "/tmp/rsry-backend-active".into(),
            session_id: None,
            session_ref: Some(crate::dispatch::AgentSessionRef::new(
                "codex",
                "thread-active",
            )),
            workspace_path: None,
            chain_hash: None,
        })
        .await
        .expect("record dispatch");

    let active =
        tool_active_with_registry(Some(&store), crate::session::SessionRegistry::default())
            .await
            .expect("query active");

    assert_eq!(active["running"], 1);
    assert_eq!(active["backend"]["active_dispatches"], 1);
    assert_eq!(active["backend"]["active_pipelines"], 1);
    assert_eq!(active["agents"][0]["source"], "backend");
    assert_eq!(active["agents"][0]["bead_id"], "rosary-backend-active");
    assert_eq!(
        active["agents"][0]["dispatch_id"],
        "dispatch-backend-active"
    );
    assert_eq!(active["agents"][0]["session_ref"]["provider"], "codex");
    assert_eq!(active["agents"][0]["pipeline"]["phase_status"], "executing");
}

#[tokio::test]
async fn dispatch_record_rejects_malformed_native_session_ref() {
    use crate::store::tests::InMemoryStore;

    let store = InMemoryStore::new();
    let err = tool_dispatch_record(
        &json!({
            "id": "dispatch-native",
            "repo": "rosary",
            "bead_id": "rosary-native",
            "agent": "dev-agent",
            "provider": "codex",
            "work_dir": "/tmp/native",
            "session_ref": {
                "provider": "codex"
            }
        }),
        Some(&store),
    )
    .await
    .unwrap_err();

    let msg = err.to_string();
    assert!(
        msg.contains("session_ref.id"),
        "error must name the malformed field; got: {msg}"
    );
    assert!(
        crate::store::DispatchStore::active_dispatches(&store)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn agent_run_event_record_and_list_roundtrip_partial_review() {
    use crate::store::tests::InMemoryStore;

    let store = InMemoryStore::new();
    tool_agent_run_event_record(
        &json!({
            "id": "evt-1",
            "dispatch_id": "dispatch-1",
            "repo": "rosary",
            "bead_id": "rosary-run",
            "event_type": "review_finding",
            "summary": "malformed session_ref should be rejected",
            "payload": { "severity": "should-fix" },
            "session_ref": {
                "provider": "codex",
                "id": "thread-123"
            }
        }),
        Some(&store),
    )
    .await
    .expect("record event");

    let got = tool_agent_run_events(
        &json!({ "repo": "rosary", "bead_id": "rosary-run" }),
        Some(&store),
    )
    .await
    .expect("list events");

    assert_eq!(got["count"], 1);
    assert_eq!(got["events"][0]["event_type"], "review_finding");
    assert_eq!(got["events"][0]["session_ref"]["provider"], "codex");
    assert_eq!(got["events"][0]["payload"]["severity"], "should-fix");
}

#[tokio::test]
async fn agent_run_event_record_rejects_malformed_payload() {
    use crate::store::tests::InMemoryStore;

    let store = InMemoryStore::new();
    let err = tool_agent_run_event_record(
        &json!({
            "id": "evt-1",
            "dispatch_id": "dispatch-1",
            "repo": "rosary",
            "bead_id": "rosary-run",
            "event_type": "review_finding",
            "summary": "bad payload",
            "payload": "not-object"
        }),
        Some(&store),
    )
    .await
    .unwrap_err();

    assert!(
        err.to_string().contains("payload"),
        "error must name payload; got: {err}"
    );
}

#[tokio::test]
async fn agent_session_addresses_resolve_claude_and_codex_for_bead() {
    use crate::store::tests::InMemoryStore;

    let store = InMemoryStore::new();
    tool_dispatch_record(
        &json!({
            "id": "dispatch-claude",
            "repo": "rosary",
            "bead_id": "rosary-addressable",
            "agent": "dev-agent",
            "provider": "claude",
            "work_dir": "/tmp/claude",
            "session_id": "claude-session-1"
        }),
        Some(&store),
    )
    .await
    .expect("record claude dispatch");
    tool_dispatch_record(
        &json!({
            "id": "dispatch-codex",
            "repo": "rosary",
            "bead_id": "rosary-addressable",
            "agent": "staging-agent",
            "provider": "codex",
            "work_dir": "/tmp/codex",
            "session_ref": {
                "provider": "codex",
                "id": "thread-123"
            }
        }),
        Some(&store),
    )
    .await
    .expect("record codex dispatch");
    tool_agent_run_event_record(
        &json!({
            "id": "evt-codex-1",
            "dispatch_id": "dispatch-codex",
            "repo": "rosary",
            "bead_id": "rosary-addressable",
            "event_type": "heartbeat",
            "summary": "codex is alive",
            "session_ref": {
                "provider": "codex",
                "id": "thread-123"
            }
        }),
        Some(&store),
    )
    .await
    .expect("record codex event");

    let got = tool_agent_session_addresses(
        &json!({ "repo": "rosary", "bead_id": "rosary-addressable" }),
        Some(&store),
    )
    .await
    .expect("resolve addresses");

    assert_eq!(got["count"], 2);
    assert_eq!(got["addresses"][0]["provider"], "claude");
    assert_eq!(got["addresses"][0]["id"], "claude-session-1");
    assert_eq!(got["addresses"][0]["active"], true);
    assert_eq!(got["addresses"][0]["sources"], json!(["dispatch"]));
    assert_eq!(got["addresses"][1]["provider"], "codex");
    assert_eq!(got["addresses"][1]["id"], "thread-123");
    assert_eq!(got["addresses"][1]["active"], true);
    assert_eq!(got["addresses"][1]["event_count"], 1);
    assert_eq!(
        got["addresses"][1]["sources"],
        json!(["dispatch", "agent_run_event"])
    );
}

#[tokio::test]
async fn agent_session_addresses_include_completed_dispatch_without_events() {
    use crate::store::{DispatchRecord, DispatchStore, WorkRef, tests::InMemoryStore};

    let store = InMemoryStore::new();
    let bead = WorkRef {
        repo: "rosary".to_string(),
        scope: String::new(),
        bead_id: "rosary-addressable".to_string(),
    };
    store
        .record_dispatch(&DispatchRecord {
            id: "dispatch-complete".to_string(),
            bead_ref: bead,
            agent: "prod-agent".to_string(),
            provider: "codex".to_string(),
            started_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            outcome: Some("success".to_string()),
            work_dir: "/tmp/codex-complete".to_string(),
            session_id: None,
            session_ref: Some(crate::dispatch::AgentSessionRef::new(
                "codex",
                "thread-complete",
            )),
            workspace_path: None,
            chain_hash: None,
        })
        .await
        .expect("record completed dispatch");

    let got = tool_agent_session_addresses(
        &json!({ "repo": "rosary", "bead_id": "rosary-addressable" }),
        Some(&store),
    )
    .await
    .expect("resolve addresses");

    assert_eq!(got["count"], 1);
    assert_eq!(got["addresses"][0]["provider"], "codex");
    assert_eq!(got["addresses"][0]["id"], "thread-complete");
    assert_eq!(got["addresses"][0]["active"], false);
    assert_eq!(got["addresses"][0]["sources"], json!(["dispatch"]));
}

#[tokio::test]
async fn agent_session_message_records_addressed_handoff_event() {
    use crate::store::tests::InMemoryStore;

    let store = InMemoryStore::new();
    tool_dispatch_record(
        &json!({
            "id": "dispatch-codex",
            "repo": "rosary",
            "bead_id": "rosary-addressable",
            "agent": "staging-agent",
            "provider": "codex",
            "work_dir": "/tmp/codex",
            "session_ref": {
                "provider": "codex",
                "id": "thread-123"
            }
        }),
        Some(&store),
    )
    .await
    .expect("record codex dispatch");

    let recorded = tool_agent_session_message_record(
        &json!({
            "id": "msg-1",
            "repo": "rosary",
            "bead_id": "rosary-addressable",
            "session_ref": {
                "provider": "codex",
                "id": "thread-123"
            },
            "message": "please review the stored findings",
            "payload": { "handoff_kind": "review" }
        }),
        Some(&store),
    )
    .await
    .expect("record message");

    assert_eq!(recorded["id"], "msg-1");
    assert_eq!(recorded["dispatch_id"], "dispatch-codex");
    assert_eq!(recorded["recorded"], true);

    let got = tool_agent_run_events(
        &json!({ "repo": "rosary", "bead_id": "rosary-addressable" }),
        Some(&store),
    )
    .await
    .expect("list events");

    assert_eq!(got["count"], 1);
    assert_eq!(got["events"][0]["event_type"], "handoff_message");
    assert_eq!(
        got["events"][0]["summary"],
        "please review the stored findings"
    );
    assert_eq!(got["events"][0]["session_ref"]["provider"], "codex");
    assert_eq!(got["events"][0]["payload"]["direction"], "outbound");
    assert_eq!(got["events"][0]["payload"]["handoff_kind"], "review");
}

/// rosary-d18be8 / rosary-d298a3: review + verify history is a Rosary-owned,
/// queryable artifact folded through the lattice — GitHub is a projection.
#[tokio::test]
async fn bead_history_returns_folded_observation_history() {
    use crate::observation::{Observation, PipelineVerdictValue, Source};
    use crate::store::WorkRef;

    let repo = crate::testutil::TestRepo::new();
    let beads_dir = repo.path().join(".beads");
    {
        let store = crate::bead_sqlite::connect_bead_store(&beads_dir)
            .await
            .unwrap();
        store
            .create_bead_full(crate::store::NewBead {
                id: "rosary-h1".into(),
                title: "H".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        for (v, phase) in [
            (PipelineVerdictValue::Verifying, 1u32),
            (PipelineVerdictValue::Pass, 2u32),
        ] {
            let obs = Observation::pipeline_verdict(
                WorkRef {
                    repo: "myrepo".into(),
                    scope: String::new(),
                    bead_id: "rosary-h1".into(),
                },
                Source::new("rosary"),
                format!("phase{phase}:dev-agent"),
                v,
                chrono::Utc::now(),
            );
            let detail = serde_json::to_string(
                &json!({ "observation": obs, "detail": "x", "git_sha": "deadbee" }),
            )
            .unwrap();
            store.log_event("rosary-h1", "observation", &detail).await;
        }
    }
    let store = crate::bead_sqlite::connect_bead_store(&beads_dir)
        .await
        .unwrap();
    let pool = crate::pool::RepoPool::from_client("myrepo", repo.path().to_path_buf(), store);

    let args = json!({ "scope": "repo:myrepo", "id": "rosary-h1" });
    let out = tool_bead_history(&args, &pool).await.unwrap();
    assert_eq!(
        out["folded_status"], "Pass",
        "chain-max folds Verifying→Pass"
    );
    assert_eq!(out["observation_count"], 2);
    let history = out["history"].as_array().unwrap();
    assert_eq!(history.len(), 2);
    // Newest entry (Pass) surfaces its verdict + the reviewed commit SHA.
    assert_eq!(history[1]["verdict"], "pass");
    assert_eq!(history[1]["git_sha"], "deadbee");
}
