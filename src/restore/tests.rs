use super::*;
use crate::import::bead_to_contract_value;
use crate::testutil::make_bead;

fn sample_comment(id: &str, issue: &str, text: &str, author: &str) -> crate::bead::Comment {
    crate::bead::Comment {
        id: id.to_string(),
        issue_id: issue.to_string(),
        text: text.to_string(),
        author: author.to_string(),
        created_at: chrono::Utc::now(),
        edited_at: None,
        edit_reason: None,
        original_text: None,
        deleted_at: None,
        delete_reason: None,
    }
}

/// rosary-9d4951: the id-PRESERVING restore is the inverse of the contract
/// export — unlike `import_bead` (which re-keys), a restored bead keeps its
/// ORIGINAL id, status, dependency edge, and comment, and a second restore
/// is a no-op (idempotent by id, never clobbers).
#[tokio::test]
async fn restore_from_contract_preserves_id_status_and_deps() {
    let store =
        crate::bead_sqlite::SqliteBeadStore::connect(std::path::Path::new(":memory:")).unwrap();
    let mut b = make_bead("ley-line-open-4aeb4f", "research", "ley-line-open");
    b.title = "Research: Turso".to_string();
    b.status = "closed".to_string();
    b.priority = 1;
    // A cross-repo / not-yet-restored dep target — restore_dependency must
    // preserve it verbatim (no existence check), like the migration.
    let deps = vec!["ley-line-open-71b20c".to_string()];
    let comments = vec![sample_comment(
        "c1",
        "ley-line-open-4aeb4f",
        "the finding",
        "bob",
    )];
    let v = bead_to_contract_value(&b, &deps, &comments);

    let r = restore_beads_from_contract(std::slice::from_ref(&v), &store, "ley-line-open")
        .await
        .unwrap();
    assert_eq!(r.restored, 1);
    assert_eq!(r.dependencies, 1);
    assert_eq!(r.comments, 1);
    assert_eq!(r.skipped_existing, 0);

    // Id preserved verbatim (the whole point — import_bead would re-key).
    let got = store
        .get_bead("ley-line-open-4aeb4f", "ley-line-open")
        .await
        .unwrap()
        .expect("bead restored under its original id");
    assert_eq!(got.id, "ley-line-open-4aeb4f");
    assert_eq!(got.priority, 1);
    assert_eq!(
        crate::bead::BeadState::from(got.status.as_str()),
        crate::bead::BeadState::Done,
        "status restored verbatim, not reset to open"
    );
    assert_eq!(
        store
            .get_dependencies("ley-line-open-4aeb4f")
            .await
            .unwrap(),
        vec!["ley-line-open-71b20c".to_string()],
        "dependency edge preserved verbatim (dangling target ok)"
    );
    assert!(
        store
            .list_comments("ley-line-open-4aeb4f", false)
            .await
            .unwrap()
            .iter()
            .any(|c| c.text == "the finding"),
        "comment preserved"
    );

    // Idempotent: a second restore skips the now-present id, no clobber, no
    // duplicate comment.
    let r2 = restore_beads_from_contract(&[v], &store, "ley-line-open")
        .await
        .unwrap();
    assert_eq!(r2.restored, 0, "already-present id is skipped");
    assert_eq!(r2.updated, 0, "equal timestamps => tie => keep local");
    assert_eq!(r2.skipped_existing, 1);
    assert_eq!(
        store
            .list_comments("ley-line-open-4aeb4f", false)
            .await
            .unwrap()
            .len(),
        1,
        "no duplicate comment on re-restore"
    );
}

/// rosary-4ebf52: a peer's state transition must actually land. This is the
/// "vigil closed it but no other machine knows" case — under the old
/// skip-existing rule the close was silently dropped.
#[tokio::test]
async fn lww_applies_newer_incoming_and_keeps_newer_local() {
    let store =
        crate::bead_sqlite::SqliteBeadStore::connect(std::path::Path::new(":memory:")).unwrap();
    let t0 = chrono::DateTime::parse_from_rfc3339("2026-07-20T10:00:00+00:00")
        .unwrap()
        .with_timezone(&chrono::Utc);

    let mut b = make_bead("rosary-lww001", "bug", "rosary");
    b.title = "Original".to_string();
    b.status = "open".to_string();
    b.created_at = t0;
    b.updated_at = t0;
    let v0 = bead_to_contract_value(&b, &[], &[]);
    let r = restore_beads_from_contract(&[v0], &store, "rosary")
        .await
        .unwrap();
    assert_eq!(r.restored, 1);

    // Timestamps preserved verbatim — the precondition that makes LWW
    // meaningful and keeps a re-export byte-stable.
    let got = store
        .get_bead("rosary-lww001", "rosary")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        got.updated_at, t0,
        "updated_at preserved, not stamped now()"
    );

    // A NEWER peer record closing the bead must win.
    let mut newer = b.clone();
    newer.status = "closed".to_string();
    newer.title = "Closed by peer".to_string();
    newer.updated_at = t0 + chrono::Duration::hours(1);
    let r2 = restore_beads_from_contract(
        &[bead_to_contract_value(&newer, &[], &[])],
        &store,
        "rosary",
    )
    .await
    .unwrap();
    assert_eq!(r2.updated, 1, "newer incoming applied");
    assert_eq!(r2.skipped_existing, 0);
    let got = store
        .get_bead("rosary-lww001", "rosary")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        crate::bead::BeadState::from(got.status.as_str()),
        crate::bead::BeadState::Done,
        "peer's close propagated"
    );
    assert_eq!(got.title, "Closed by peer");
    assert_eq!(got.updated_at, newer.updated_at, "winner's timestamp kept");

    // An OLDER record must NOT clobber the newer local state.
    let mut older = b.clone();
    older.status = "open".to_string();
    older.title = "Stale".to_string();
    older.updated_at = t0 - chrono::Duration::hours(1);
    let r3 = restore_beads_from_contract(
        &[bead_to_contract_value(&older, &[], &[])],
        &store,
        "rosary",
    )
    .await
    .unwrap();
    assert_eq!(r3.updated, 0, "older incoming ignored");
    assert_eq!(r3.skipped_existing, 1);
    let got = store
        .get_bead("rosary-lww001", "rosary")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got.title, "Closed by peer", "local newer state survived");
    assert_eq!(
        crate::bead::BeadState::from(got.status.as_str()),
        crate::bead::BeadState::Done
    );
}
