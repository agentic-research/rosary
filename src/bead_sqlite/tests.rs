use super::*;

/// rosary-21e2d4: a bd embedded-Dolt store (no server-mode `dolt/`) must
/// produce a loud warning, not a silent fall-through to empty SQLite.
#[test]
fn warns_on_embedded_dolt_backend() {
    let tmp = tempfile::tempdir().unwrap();
    let beads = tmp.path().join(".beads");
    std::fs::create_dir_all(beads.join("embeddeddolt")).unwrap();
    let w = unreadable_backend_warning(&beads);
    assert!(w.is_some(), "embedded-Dolt store must warn, got None");
    assert!(
        w.unwrap().contains("embeddeddolt"),
        "warning should name the embedded store"
    );
}

/// Server-mode (`dolt/`) is readable → no warning.
#[test]
fn no_warning_for_server_mode_or_bare() {
    let tmp = tempfile::tempdir().unwrap();
    let beads = tmp.path().join(".beads");
    std::fs::create_dir_all(beads.join("dolt")).unwrap();
    // even if an embeddeddolt dir co-exists, server mode is readable
    std::fs::create_dir_all(beads.join("embeddeddolt")).unwrap();
    assert!(
        unreadable_backend_warning(&beads).is_none(),
        "server mode is readable"
    );

    let tmp2 = tempfile::tempdir().unwrap();
    let bare = tmp2.path().join(".beads");
    std::fs::create_dir_all(&bare).unwrap();
    assert!(
        unreadable_backend_warning(&bare).is_none(),
        "bare/uninitialized .beads is a normal bootstrap, not unreadable"
    );
}

/// rosary-65c2ff: an `embeddeddolt/` dir co-existing with a readable
/// `beads.db` is NOT a "cannot read / 0 beads" situation — rsry reads the
/// SQLite `beads.db` (ADR-0014), so the scary warning must be suppressed.
#[test]
fn no_warning_when_beads_db_present_alongside_embeddeddolt() {
    let tmp = tempfile::tempdir().unwrap();
    let beads = tmp.path().join(".beads");
    std::fs::create_dir_all(beads.join("embeddeddolt")).unwrap();
    std::fs::write(beads.join("beads.db"), b"sqlite-data").unwrap();
    assert!(
        unreadable_backend_warning(&beads).is_none(),
        "beads.db present → rsry reads it; no unreadable warning"
    );
}

fn test_store() -> SqliteBeadStore {
    SqliteBeadStore::connect(Path::new(":memory:")).unwrap()
}

#[tokio::test]
async fn create_and_get_bead() {
    let store = test_store();
    store
        .create_bead("test-1", "Test bead", "A description", 2, "task")
        .await
        .unwrap();

    let bead = store.get_bead("test-1", "rosary").await.unwrap().unwrap();
    assert_eq!(bead.id, "test-1");
    assert_eq!(bead.title, "Test bead");
    assert_eq!(bead.status, "open");
    assert_eq!(bead.priority, 2);
}

/// ADR-0021 slice 2: `create_bead` is a thin projection onto the one writer
/// (`create_bead_full`), so the basic and full create paths must produce the
/// same stored row for equivalent inputs — they can't drift into two INSERTs.
#[tokio::test]
async fn create_bead_agrees_with_create_bead_full() {
    let store = test_store();
    store
        .create_bead("basic-1", "same", "body", 3, "chore")
        .await
        .unwrap();
    store
        .create_bead_full(crate::store::NewBead {
            id: "full-1".into(),
            title: "same".into(),
            description: "body".into(),
            priority: 3,
            issue_type: "chore".into(),
            owner: String::new(),
            files: vec![],
            test_files: vec![],
            depends_on: vec![],
            created_by: None,
            scope: String::new(),
            derived_from: vec![],
            acceptance_criteria: String::new(),
        })
        .await
        .unwrap();

    let basic = store.get_bead("basic-1", "r").await.unwrap().unwrap();
    let full = store.get_bead("full-1", "r").await.unwrap().unwrap();
    // Every field that could drift between the two INSERTs must match.
    assert_eq!(basic.status, full.status);
    assert_eq!(basic.priority, full.priority);
    assert_eq!(basic.issue_type, full.issue_type);
    assert_eq!(basic.scope, full.scope);
    assert_eq!(basic.created_by, full.created_by);
    assert_eq!(basic.acceptance_criteria, full.acceptance_criteria);
    assert_eq!(basic.owner, full.owner);
    assert_eq!(basic.files, full.files);
}

/// ADR-0021 slice 1: `acceptance_criteria` is WRITTEN by `create_bead_full`
/// but every reader's SELECT omitted it, so `bead_from_row`'s
/// `.unwrap_or_default()` silently returned "" — the close condition was
/// unreadable on any SQLite store. Every reader must project it, and `get_bead`
/// and `list_all_beads` must agree.
#[tokio::test]
async fn readers_project_acceptance_criteria_consistently() {
    let store = test_store();
    store
        .create_bead_full(crate::store::NewBead {
            id: "ac-1".into(),
            title: "has a close condition".into(),
            description: String::new(),
            priority: 1,
            issue_type: "bug".into(),
            owner: String::new(),
            files: vec![],
            test_files: vec![],
            depends_on: vec![],
            created_by: None,
            scope: String::new(),
            derived_from: vec![],
            acceptance_criteria: "cargo test export green".into(),
        })
        .await
        .unwrap();

    let got = store.get_bead("ac-1", "repo").await.unwrap().unwrap();
    assert_eq!(
        got.acceptance_criteria, "cargo test export green",
        "get_bead must project the close condition, not silently drop it"
    );

    let listed = store.list_all_beads("repo").await.unwrap();
    let l = listed.iter().find(|b| b.id == "ac-1").unwrap();
    assert_eq!(
        l.acceptance_criteria, "cargo test export green",
        "list_all_beads must agree with get_bead (one field set)"
    );
}

/// Status aliases are canonicalized on connect (migration) — no reader has to
/// absorb `closed`/`deadletter` at read time. Transform early, store canonical.
#[tokio::test]
async fn connect_canonicalizes_legacy_status_aliases() {
    let store = test_store();
    store.create_bead("t-1", "a", "", 2, "task").await.unwrap();
    // Simulate a legacy row written before the canonicalizing write boundary.
    {
        let conn = store.conn.lock().unwrap();
        conn.execute("UPDATE issues SET status = 'closed' WHERE id = 't-1'", [])
            .unwrap();
        conn.execute(
            "INSERT INTO issues (id, title, status, priority, issue_type, created_at, updated_at) \
             VALUES ('t-2','b','deadletter',2,'task', datetime('now'), datetime('now'))",
            [],
        )
        .unwrap();
        super::canonicalize_statuses(&conn); // idempotent; the connect-time pass
    }
    assert_eq!(
        store.get_status("t-1").await.unwrap().as_deref(),
        Some("done")
    );
    assert_eq!(
        store.get_status("t-2").await.unwrap().as_deref(),
        Some("dead_letter")
    );
}

// Regression: short ID (suffix only) must resolve to full prefixed ID.
// Before fix: close_bead("2a3970") silently succeeded with 0 rows changed
// when the stored ID was "ley-line-open-2a3970".
#[tokio::test]
async fn close_bead_short_id_resolves() {
    let store = test_store();
    store
        .create_bead("repo-2a3970", "Some bead", "", 2, "task")
        .await
        .unwrap();

    // short suffix — must close the right row
    store.close_bead("2a3970").await.unwrap();

    let conn = store.conn.lock().unwrap();
    let status: String = conn
        .query_row(
            "SELECT status FROM issues WHERE id = 'repo-2a3970'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(status, "closed");
}

#[tokio::test]
async fn close_bead_unknown_id_errors() {
    let store = test_store();
    let err = store.close_bead("doesnotexist").await.unwrap_err();
    assert!(err.to_string().contains("not found"), "{err}");
}

#[tokio::test]
async fn list_beads_excludes_closed() {
    let store = test_store();
    store.create_bead("a", "Open", "", 1, "task").await.unwrap();
    store
        .create_bead("b", "Closed", "", 2, "task")
        .await
        .unwrap();
    store.close_bead("b").await.unwrap();

    let beads = store.list_beads("repo").await.unwrap();
    assert_eq!(beads.len(), 1);
    assert_eq!(beads[0].id, "a");
}

/// rosary-91e712: list_all_beads includes closed (full enumeration for
/// export/backup), unlike list_beads which excludes them.
#[tokio::test]
async fn list_all_beads_includes_closed() {
    let store = test_store();
    store.create_bead("a", "Open", "", 1, "task").await.unwrap();
    store
        .create_bead("b", "Closed", "", 2, "task")
        .await
        .unwrap();
    store.close_bead("b").await.unwrap();

    let active = store.list_beads("repo").await.unwrap();
    assert_eq!(active.len(), 1, "list_beads excludes closed");

    let all = store.list_all_beads("repo").await.unwrap();
    assert_eq!(all.len(), 2, "list_all_beads includes closed");
    let mut ids: Vec<_> = all.iter().map(|b| b.id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(ids, vec!["a", "b"]);
}

#[tokio::test]
async fn create_bead_full_with_deps() {
    let store = test_store();
    store
        .create_bead("dep-1", "Dep", "", 1, "task")
        .await
        .unwrap();
    store
        .create_bead_full(crate::store::NewBead {
            id: "main-1".into(),
            title: "Main".into(),
            description: "desc".into(),
            priority: 1,
            issue_type: "feature".into(),
            owner: "agent".into(),
            files: vec!["src/main.rs".into()],
            test_files: vec!["src/main_test.rs".into()],
            depends_on: vec!["dep-1".into()],
            created_by: Some("test-user".into()),
            ..Default::default()
        })
        .await
        .unwrap();

    let bead = store.get_bead("main-1", "repo").await.unwrap().unwrap();
    assert_eq!(bead.owner.as_deref(), Some("agent"));
    assert_eq!(bead.files, vec!["src/main.rs"]);
    assert_eq!(bead.test_files, vec!["src/main_test.rs"]);

    let deps = store.get_dependencies("main-1").await.unwrap();
    assert_eq!(deps, vec!["dep-1"]);
}

#[tokio::test]
async fn set_files_preserves_derived_from() {
    // set_files must not clobber provenance. It previously overwrote the
    // entire notes JSON with {files, test_files}, silently dropping
    // derived_from — provenance data loss. (rosary-027940)
    let store = test_store();
    let prov = bdr::provenance::ProvenanceRef::Session {
        transcript_path: "/tmp/session.jsonl".into(),
        summary: Some("origin session".into()),
    };
    store
        .create_bead_full(crate::store::NewBead {
            id: "b1".into(),
            title: "T".into(),
            description: "d".into(),
            priority: 1,
            issue_type: "feature".into(),
            owner: "agent".into(),
            files: vec!["a.rs".into()],
            derived_from: vec![prov.clone()],
            ..Default::default()
        })
        .await
        .unwrap();

    // Sanity: provenance round-trips through create + get.
    let b = store.get_bead("b1", "repo").await.unwrap().unwrap();
    assert_eq!(b.derived_from, vec![prov.clone()]);

    // set_files updates files but MUST preserve derived_from.
    store
        .set_files("b1", &["a.rs".into(), "b.rs".into()], &[])
        .await
        .unwrap();

    let b2 = store.get_bead("b1", "repo").await.unwrap().unwrap();
    assert_eq!(b2.files, vec!["a.rs", "b.rs"], "files should update");
    assert_eq!(
        b2.derived_from,
        vec![prov],
        "set_files must preserve derived_from provenance"
    );
}

#[tokio::test]
async fn update_bead_fields_preserves_derived_from() {
    // update_bead_fields rewrites the notes JSON when files change. It must
    // preserve derived_from — same clobber root as set_files. (rosary-027940)
    let store = test_store();
    let prov = bdr::provenance::ProvenanceRef::Adr {
        id: "ADR-007".into(),
    };
    store
        .create_bead_full(crate::store::NewBead {
            id: "u1".into(),
            title: "T".into(),
            description: "d".into(),
            priority: 2,
            issue_type: "feature".into(),
            owner: "agent".into(),
            files: vec!["x.rs".into()],
            derived_from: vec![prov.clone()],
            ..Default::default()
        })
        .await
        .unwrap();

    let update = BeadUpdate {
        files: Some(vec!["x.rs".into(), "y.rs".into()]),
        ..Default::default()
    };
    store.update_bead_fields("u1", &update).await.unwrap();

    let bead = store.get_bead("u1", "repo").await.unwrap().unwrap();
    assert_eq!(bead.files, vec!["x.rs", "y.rs"], "files should update");
    assert_eq!(
        bead.derived_from,
        vec![prov],
        "update_bead_fields must preserve derived_from provenance"
    );
}

#[tokio::test]
async fn epic_dep_does_not_block_child() {
    // A child whose only dependency is an EPIC must be ready, not blocked.
    // Epics are never dispatched (triage skips them) and complete by rollup
    // AFTER their children — so depending on one is containment, not ordering.
    // Counting it as a blocking dep deadlocks the child. (rosary-199cc4)
    let store = test_store();
    store
        .create_bead_full(crate::store::NewBead {
            id: "epic-1".into(),
            title: "Epic".into(),
            description: "d".into(),
            priority: 1,
            issue_type: "epic".into(),
            owner: "agent".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    store
        .create_bead_full(crate::store::NewBead {
            id: "child-1".into(),
            title: "Child".into(),
            description: "d".into(),
            priority: 1,
            issue_type: "feature".into(),
            owner: "agent".into(),
            depends_on: vec!["epic-1".into()],
            ..Default::default()
        })
        .await
        .unwrap();

    let child = store.get_bead("child-1", "repo").await.unwrap().unwrap();
    assert_eq!(
        child.dependency_count, 0,
        "an epic dependency must not count as blocking"
    );
    assert!(
        child.is_ready(),
        "a child whose only dep is an epic must be ready"
    );

    // Regression guard: a non-epic open dep STILL blocks (don't over-broaden).
    store
        .create_bead_full(crate::store::NewBead {
            id: "task-dep".into(),
            title: "Task".into(),
            description: "d".into(),
            priority: 1,
            issue_type: "task".into(),
            owner: "agent".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    store
        .create_bead_full(crate::store::NewBead {
            id: "child-2".into(),
            title: "Child2".into(),
            description: "d".into(),
            priority: 1,
            issue_type: "feature".into(),
            owner: "agent".into(),
            depends_on: vec!["task-dep".into()],
            ..Default::default()
        })
        .await
        .unwrap();
    let child2 = store.get_bead("child-2", "repo").await.unwrap().unwrap();
    assert_eq!(
        child2.dependency_count, 1,
        "a non-epic open dependency must still block"
    );
}

#[tokio::test]
async fn search_beads_by_title() {
    let store = test_store();
    store
        .create_bead("a", "Fix dispatch bug", "", 1, "bug")
        .await
        .unwrap();
    store
        .create_bead("b", "Add feature X", "", 2, "feature")
        .await
        .unwrap();

    let results = store.search_beads("dispatch", "repo", 10).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "a");
}

/// rosary-a9bc77: search hits comment text, not just title/description.
#[tokio::test]
async fn search_beads_matches_comment_text() {
    let store = test_store();
    store
        .create_bead("a", "Generic title one", "", 1, "task")
        .await
        .unwrap();
    store
        .create_bead("b", "Generic title two", "", 1, "task")
        .await
        .unwrap();
    // Distinctive content lives in a comment on bead `b`.
    store
        .add_comment("b", "investigating zarathustra anomaly", "u")
        .await
        .unwrap();

    // Title search misses both (no overlap with comment text).
    assert!(
        store
            .search_beads("zarathustra", "repo", 10)
            .await
            .unwrap()
            .iter()
            .any(|x| x.id == "b")
    );
    // Sanity: searching for a word in `a`'s title still works.
    let one = store
        .search_beads("Generic title one", "repo", 10)
        .await
        .unwrap();
    assert!(one.iter().any(|x| x.id == "a"));
}

/// rosary-a9bc77: soft-deleted comments must NOT contribute to search hits.
/// Otherwise scrubbed PII would still surface — which is the exact failure
/// mode the comment-edit primitive was added to fix.
#[tokio::test]
async fn search_beads_excludes_soft_deleted_comments() {
    let store = test_store();
    store
        .create_bead("a", "Title with no match", "", 1, "task")
        .await
        .unwrap();
    store
        .add_comment("a", "leaked /Users/alice/secret/path", "u")
        .await
        .unwrap();

    // Pre-delete: comment-text search hits.
    let pre = store.search_beads("alice", "repo", 10).await.unwrap();
    assert!(pre.iter().any(|b| b.id == "a"), "comment text should match");

    // Soft-delete the comment.
    let cid = store.list_comments("a", false).await.unwrap()[0].id.clone();
    store.delete_comment(&cid, Some("scrub")).await.unwrap();

    // Post-delete: comment is hidden from search.
    let post = store.search_beads("alice", "repo", 10).await.unwrap();
    assert!(
        !post.iter().any(|b| b.id == "a"),
        "soft-deleted comment text must not surface in search",
    );
}

#[tokio::test]
async fn search_beads_by_id() {
    let store = test_store();
    store
        .create_bead("rosary-abc123", "Fix dispatch bug", "", 1, "bug")
        .await
        .unwrap();
    store
        .create_bead("rosary-def456", "Add feature X", "", 2, "feature")
        .await
        .unwrap();

    // Exact ID match
    let results = store
        .search_beads("rosary-abc123", "repo", 10)
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "rosary-abc123");

    // Partial ID prefix match
    let results = store.search_beads("rosary-", "repo", 10).await.unwrap();
    assert_eq!(results.len(), 2);
}

#[tokio::test]
async fn search_beads_fts_stemming() {
    let store = test_store();
    store
        .create_bead(
            "a",
            "Dispatch agent workers",
            "Fix dispatching logic",
            1,
            "bug",
        )
        .await
        .unwrap();
    store
        .create_bead("b", "Add feature X", "Unrelated", 2, "feature")
        .await
        .unwrap();

    // Porter stemmer: "dispatching" matches "dispatch"
    let results = store
        .search_beads_fts("dispatch", "repo", 10)
        .await
        .unwrap();
    assert_eq!(results.len(), 1, "FTS should find stemmed match");
    assert_eq!(results[0].id, "a");

    // Multi-word: both terms must appear
    let results = store
        .search_beads_fts("dispatch workers", "repo", 10)
        .await
        .unwrap();
    assert_eq!(results.len(), 1);

    // No match for unrelated term
    let results = store
        .search_beads_fts("nonexistent", "repo", 10)
        .await
        .unwrap();
    assert!(results.is_empty());
}

#[tokio::test]
async fn search_excludes_closed_deps_from_dependency_count() {
    let store = test_store();
    // Create dep bead and main bead with a dependency on it.
    store
        .create_bead("dep", "Dep", "", 1, "task")
        .await
        .unwrap();
    store
        .create_bead("main", "Main", "", 1, "task")
        .await
        .unwrap();
    store.add_dependency("main", "dep").await.unwrap();

    // Before closing dep: search should show dependency_count = 1 (blocked).
    let results = store.search_beads("Main", "repo", 10).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].dependency_count, 1);
    assert!(results[0].is_blocked());

    // After closing dep: search should show dependency_count = 0 (unblocked).
    store.close_bead("dep").await.unwrap();
    let results = store.search_beads("Main", "repo", 10).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].dependency_count, 0);
    assert!(!results[0].is_blocked());
}

#[tokio::test]
async fn typed_deps_and_get_children() {
    // rosary-649660: get_children returns only containment edges
    // (parent-child / discovered-from), never plain blocks edges.
    let store = test_store();
    for id in ["parent", "kid", "found", "blocker"] {
        store.create_bead(id, id, "", 1, "task").await.unwrap();
    }
    // kid is a child of parent; found was discovered from parent; blocker is a
    // plain blocks edge (parent depends on blocker).
    store
        .add_dependency_typed("kid", "parent", "parent-child")
        .await
        .unwrap();
    store
        .add_dependency_typed("found", "parent", "discovered-from")
        .await
        .unwrap();
    store.add_dependency("parent", "blocker").await.unwrap();

    let mut children = store.get_children("parent").await.unwrap();
    children.sort();
    assert_eq!(children, vec!["found".to_string(), "kid".to_string()]);
    // The blocks edge must NOT surface as a child.
    assert!(store.get_children("blocker").await.unwrap().is_empty());
}

#[tokio::test]
async fn add_dependency_typed_upserts_type() {
    // Re-linking an existing pair promotes its type (blocks → parent-child)
    // rather than silently ignoring the second write.
    let store = test_store();
    store.create_bead("c", "c", "", 1, "task").await.unwrap();
    store.create_bead("p", "p", "", 1, "task").await.unwrap();
    store.add_dependency("c", "p").await.unwrap(); // blocks
    assert!(store.get_children("p").await.unwrap().is_empty());
    store
        .add_dependency_typed("c", "p", "parent-child")
        .await
        .unwrap();
    assert_eq!(
        store.get_children("p").await.unwrap(),
        vec!["c".to_string()]
    );
}

#[tokio::test]
async fn update_status_and_close() {
    let store = test_store();
    store.create_bead("x", "Test", "", 1, "task").await.unwrap();

    store.update_status("x", "dispatched").await.unwrap();
    assert_eq!(
        store.get_status("x").await.unwrap().as_deref(),
        Some("dispatched")
    );

    store.close_bead("x").await.unwrap();
    assert_eq!(
        store.get_status("x").await.unwrap().as_deref(),
        Some("closed")
    );
}

#[tokio::test]
async fn comments_and_events() {
    let store = test_store();
    store.create_bead("c", "Test", "", 1, "task").await.unwrap();

    store
        .add_comment("c", "progress note", "dev-agent")
        .await
        .unwrap();
    store.log_event("c", "dispatched", "agent started").await;

    let event = store.get_latest_event("c", "dispatched").await.unwrap();
    assert_eq!(event.as_deref(), Some("agent started"));
}

/// rosary-a96b06: list returns audit-trail-aware comments.
#[tokio::test]
async fn list_comments_oldest_first_excludes_deleted_by_default() {
    let store = test_store();
    store.create_bead("c", "T", "", 1, "task").await.unwrap();
    store.add_comment("c", "first", "alice").await.unwrap();
    store.add_comment("c", "second", "bob").await.unwrap();
    store.add_comment("c", "third", "carol").await.unwrap();

    let listed = store.list_comments("c", false).await.unwrap();
    assert_eq!(listed.len(), 3);
    assert_eq!(listed[0].text, "first");
    assert_eq!(listed[2].text, "third");

    // Soft-delete the middle one.
    let mid_id = listed[1].id.clone();
    store.delete_comment(&mid_id, None).await.unwrap();

    let visible = store.list_comments("c", false).await.unwrap();
    assert_eq!(visible.len(), 2);
    assert!(visible.iter().all(|c| !c.is_deleted()));

    let all = store.list_comments("c", true).await.unwrap();
    assert_eq!(all.len(), 3);
    let deleted: Vec<_> = all.iter().filter(|c| c.is_deleted()).collect();
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0].id, mid_id);
}

/// rosary-a96b06: first edit captures original_text; subsequent edits do not.
#[tokio::test]
async fn update_comment_first_edit_captures_original_text() {
    let store = test_store();
    store.create_bead("c", "T", "", 1, "task").await.unwrap();
    store
        .add_comment("c", "the original body", "u")
        .await
        .unwrap();
    let listed = store.list_comments("c", false).await.unwrap();
    let cid = listed[0].id.clone();

    let edited1 = store
        .update_comment(&cid, "first revision", Some("typo fix"))
        .await
        .unwrap();
    assert_eq!(edited1.text, "first revision");
    assert_eq!(
        edited1.original_text.as_deref(),
        Some("the original body"),
        "first edit must capture the prior body in original_text",
    );
    assert_eq!(edited1.edit_reason.as_deref(), Some("typo fix"));
    assert!(edited1.is_edited());

    // Second edit must NOT overwrite original_text.
    let edited2 = store
        .update_comment(&cid, "second revision", Some("clarify"))
        .await
        .unwrap();
    assert_eq!(edited2.text, "second revision");
    assert_eq!(
        edited2.original_text.as_deref(),
        Some("the original body"),
        "subsequent edits must NOT rewrite original_text — audit-trail invariant",
    );
    assert_eq!(edited2.edit_reason.as_deref(), Some("clarify"));
}

/// rosary-a96b06: update on non-existent id errors cleanly.
#[tokio::test]
async fn update_comment_nonexistent_errors() {
    let store = test_store();
    let r = store.update_comment("99999", "body", None).await;
    assert!(r.is_err());
    let msg = r.unwrap_err().to_string();
    assert!(msg.contains("99999") && msg.contains("not found"));
}

/// rosary-a96b06 regression (Copilot review on PR #188): the deletion
/// reason must land in its own column so it survives even when the
/// comment was previously edited. The first cut overloaded
/// `edit_reason` via `COALESCE(edit_reason, ?)`, which silently dropped
/// the deletion reason whenever an edit had already populated it.
#[tokio::test]
async fn delete_reason_survives_prior_edit() {
    let store = test_store();
    store.create_bead("c", "T", "", 1, "task").await.unwrap();
    store.add_comment("c", "v1", "u").await.unwrap();
    let cid = store.list_comments("c", false).await.unwrap()[0].id.clone();

    // Edit first, with an edit reason.
    let edited = store
        .update_comment(&cid, "v2", Some("typo fix"))
        .await
        .unwrap();
    assert_eq!(edited.edit_reason.as_deref(), Some("typo fix"));
    assert!(edited.delete_reason.is_none());

    // Now soft-delete with a deletion reason. This is the case the
    // first implementation got wrong.
    store
        .delete_comment(&cid, Some("no longer relevant"))
        .await
        .unwrap();

    let after = store.list_comments("c", true).await.unwrap();
    assert_eq!(after.len(), 1);
    let c = &after[0];
    assert!(c.is_deleted());
    assert_eq!(
        c.edit_reason.as_deref(),
        Some("typo fix"),
        "edit_reason must be preserved verbatim from the prior edit",
    );
    assert_eq!(
        c.delete_reason.as_deref(),
        Some("no longer relevant"),
        "delete_reason must be recorded in its dedicated column, not lost",
    );
}

/// rosary-a96b06: soft-delete preserves the row + audit trail; hard-delete removes it.
#[tokio::test]
async fn soft_delete_preserves_audit_trail_hard_delete_removes_row() {
    let store = test_store();
    store.create_bead("c", "T", "", 1, "task").await.unwrap();
    store
        .add_comment("c", "/Users/alice/leak", "u")
        .await
        .unwrap();
    let cid = store.list_comments("c", false).await.unwrap()[0].id.clone();

    // Soft-delete with a reason.
    store
        .delete_comment(&cid, Some("contains absolute path"))
        .await
        .unwrap();
    let after_soft = store.list_comments("c", true).await.unwrap();
    assert_eq!(after_soft.len(), 1);
    assert!(after_soft[0].is_deleted());
    // Reason lands in delete_reason (the dedicated column), NOT
    // edit_reason. This preserves it across previously-edited comments
    // — see delete_reason_survives_prior_edit for the regression case.
    assert_eq!(
        after_soft[0].delete_reason.as_deref(),
        Some("contains absolute path"),
    );
    assert!(
        after_soft[0].edit_reason.is_none(),
        "edit_reason must not be touched by delete",
    );

    // Soft-delete is idempotent.
    store.delete_comment(&cid, None).await.unwrap();

    // Hard-delete actually removes the row.
    store.hard_delete_comment(&cid).await.unwrap();
    let after_hard = store.list_comments("c", true).await.unwrap();
    assert!(after_hard.is_empty());

    // Hard-delete on a missing id errors cleanly.
    let r = store.hard_delete_comment(&cid).await;
    assert!(r.is_err());
}

#[tokio::test]
async fn log_event_synthetic_id_skips_resolution() {
    // IDs starting with `_` are synthetic (e.g. `_schema` for migration
    // records). They must not be resolved against the issues table —
    // before the fix, every migration logged "bead not found: _schema"
    // and the audit row was silently dropped.
    let store = test_store();
    // No bead created — synthetic ID has no real bead behind it.
    store.log_event("_schema", "migration", "001_initial").await;

    // The event should still be visible via direct query (resolve_id
    // would fail on a synthetic ID, but the row was written).
    let conn = store.conn.lock().unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM events WHERE issue_id = '_schema'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "synthetic _schema event must be persisted");
}

#[tokio::test]
async fn external_ref_roundtrip() {
    let store = test_store();
    store.create_bead("e", "Test", "", 1, "task").await.unwrap();
    store.set_external_ref("e", "AGE-42").await.unwrap();

    assert_eq!(
        store.get_external_ref("e").await.unwrap().as_deref(),
        Some("AGE-42")
    );
    assert_eq!(
        store
            .find_by_external_ref("AGE-42")
            .await
            .unwrap()
            .as_deref(),
        Some("e")
    );
}

#[tokio::test]
async fn dependency_lifecycle() {
    let store = test_store();
    store.create_bead("a", "A", "", 1, "task").await.unwrap();
    store.create_bead("b", "B", "", 1, "task").await.unwrap();

    store.add_dependency("b", "a").await.unwrap();
    assert_eq!(store.get_dependencies("b").await.unwrap(), vec!["a"]);
    assert_eq!(store.get_dependents("a").await.unwrap(), vec!["b"]);

    store.remove_dependency("b", "a").await.unwrap();
    assert!(store.get_dependencies("b").await.unwrap().is_empty());
}

#[tokio::test]
async fn update_bead_fields() {
    let store = test_store();
    store
        .create_bead("u", "Original", "", 2, "task")
        .await
        .unwrap();

    let update = BeadUpdate {
        title: Some("Updated".into()),
        priority: Some(1),
        ..Default::default()
    };
    let fields = store.update_bead_fields("u", &update).await.unwrap();
    assert!(fields.contains(&"title".to_string()));
    assert!(fields.contains(&"priority".to_string()));

    let bead = store.get_bead("u", "repo").await.unwrap().unwrap();
    assert_eq!(bead.title, "Updated");
    assert_eq!(bead.priority, 1);
}

#[tokio::test]
async fn created_by_and_scope_round_trip() {
    let store = test_store();
    store
        .create_bead_full(crate::store::NewBead {
            id: "s-1".into(),
            title: "Scoped bead".into(),
            description: "desc".into(),
            priority: 2,
            issue_type: "task".into(),
            owner: "agent".into(),
            created_by: Some("alice".into()),
            scope: "payments".into(),
            ..Default::default()
        })
        .await
        .unwrap();

    let bead = store.get_bead("s-1", "repo").await.unwrap().unwrap();
    assert_eq!(bead.created_by.as_deref(), Some("alice"));
    assert_eq!(bead.scope, "payments");
}

#[tokio::test]
async fn scope_defaults_to_empty_for_simple_create() {
    let store = test_store();
    store
        .create_bead("plain", "Plain bead", "", 1, "task")
        .await
        .unwrap();

    let bead = store.get_bead("plain", "repo").await.unwrap().unwrap();
    assert_eq!(bead.scope, "");
    assert_eq!(bead.created_by, None);
}

#[tokio::test]
async fn scope_appears_in_list_beads() {
    let store = test_store();
    store
        .create_bead_full(crate::store::NewBead {
            id: "ls-1".into(),
            title: "Listed bead".into(),
            priority: 2,
            issue_type: "task".into(),
            created_by: Some("bob".into()),
            scope: "auth".into(),
            ..Default::default()
        })
        .await
        .unwrap();

    let beads = store.list_beads("repo").await.unwrap();
    assert_eq!(beads.len(), 1);
    assert_eq!(beads[0].scope, "auth");
    assert_eq!(beads[0].created_by.as_deref(), Some("bob"));
    // An omitted owner (NewBead default "") must read back as unset, NOT
    // Some("") — otherwise reconcile's `owner.is_some()` auto-assign never fires.
    assert_eq!(
        beads[0].owner, None,
        "empty owner must persist as NULL/None"
    );
}

#[tokio::test]
async fn derived_from_round_trip() {
    use bdr::provenance::ProvenanceRef;

    let store = test_store();
    let provenance = vec![
        ProvenanceRef::Adr {
            id: "ADR-007".into(),
        },
        ProvenanceRef::Doc {
            path: "docs/spec.md".into(),
        },
    ];
    store
        .create_bead_full(crate::store::NewBead {
            id: "prov-1".into(),
            title: "Provenance bead".into(),
            description: "desc".into(),
            priority: 2,
            issue_type: "task".into(),
            owner: "agent".into(),
            derived_from: provenance.clone(),
            ..Default::default()
        })
        .await
        .unwrap();

    let bead = store.get_bead("prov-1", "repo").await.unwrap().unwrap();
    assert_eq!(bead.derived_from.len(), 2);
    assert_eq!(bead.derived_from[0].label(), "adr:ADR-007");
    assert_eq!(bead.derived_from[1].label(), "doc:docs/spec.md");
}
