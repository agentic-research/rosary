//! `jsonl_sync::publish_ids` — the commit-time splice P2 calls. Lives under
//! the decorator's tests because it shares the git-backed `Repo` fixture: the
//! decorator proves a write leaves the tree clean, these prove the primitive
//! that later publishes it renders exactly what the whole-file paths render.

use super::*;

/// `publish_ids` (the commit-time splice) renders exactly what `bead export
/// --published-from` (`export_published_beads_contract_jsonl`) renders over the
/// same published set, byte for byte — and its single record is field-identical
/// to the whole-file refresh (P4). Pinned rather than trusted: the fast path
/// renders from `get_bead`, the bounded paths from `list_all_beads`, and
/// ADR-0021 exists because those have drifted before.
#[tokio::test]
async fn publish_ids_matches_full_refresh_and_published_export_byte_for_byte() {
    let repo = Repo::new(&["r-0"]);
    let store = repo.store();
    store
        .create_bead_full(NewBead {
            files: vec!["src/a.rs".into()],
            test_files: vec!["tests/a.rs".into()],
            description: "a description".into(),
            ..new_bead("r-1", "one")
        })
        .await
        .unwrap();
    store.add_comment("r-1", "note", "tester").await.unwrap();
    let mut published_with_r1 = repo.published();
    published_with_r1.push(serde_json::json!({"id": "r-1"}));

    let report = publish_ids(
        store.inner.as_ref(),
        &repo.repo_name(),
        &repo.root,
        &s(&["r-1"]),
    )
    .await
    .unwrap();
    assert_eq!(report.inserted, s(&["r-1"]));
    let spliced_file = repo.raw();
    let spliced = repo.record("r-1").expect("spliced record");

    let exported = export_published_beads_contract_jsonl(
        store.inner.as_ref(),
        &published_with_r1,
        &repo.repo_name(),
    )
    .await
    .unwrap();
    assert_eq!(
        spliced_file, exported,
        "publish_ids diverged from `bead export --published-from`"
    );

    refresh_tracked_beads_jsonl(store.inner.as_ref(), &repo.repo_name(), &repo.root)
        .await
        .unwrap();
    assert_eq!(repo.record("r-1").expect("refreshed record"), spliced);
    assert_eq!(
        repo.raw(),
        spliced_file,
        "whole-file refresh rewrote a spliced file"
    );
}

/// Publishing is what dirties the tree — once per id, however often named.
#[tokio::test]
async fn publish_ids_inserts_a_named_bead_once() {
    let repo = Repo::new(&[]);
    let store = repo.store();
    store.create_bead("r-1", "t", "d", 1, "bug").await.unwrap();

    let report = publish_ids(&store, &repo.repo_name(), &repo.root, &s(&["r-1", "r-1"]))
        .await
        .unwrap();

    assert_eq!(
        report,
        PublishReport {
            inserted: s(&["r-1"]),
            ..Default::default()
        }
    );
    assert_eq!(repo.record("r-1").unwrap()["title"], "t");
    assert!(
        repo.jsonl_status().starts_with("M "),
        "got {:?}",
        repo.jsonl_status()
    );
}

#[tokio::test]
async fn publish_ids_reports_unchanged_then_updated() {
    let repo = Repo::new(&[]);
    let store = repo.store();
    store.create_bead("r-1", "t", "d", 1, "bug").await.unwrap();
    let name = repo.repo_name();
    let ids = s(&["r-1"]);
    publish_ids(&store, &name, &repo.root, &ids).await.unwrap();

    let again = publish_ids(&store, &name, &repo.root, &ids).await.unwrap();
    assert_eq!(again.unchanged, ids, "{again:?}");
    assert!(again.inserted.is_empty() && again.updated.is_empty());

    store.update_status("r-1", "in_progress").await.unwrap();
    let after = publish_ids(&store, &name, &repo.root, &ids).await.unwrap();
    assert_eq!(after.updated, ids, "{after:?}");
    let stored = store.get_status("r-1").await.unwrap().unwrap();
    assert_eq!(
        repo.record("r-1").unwrap()["status"],
        serde_json::json!(stored)
    );
}

/// A repo whose store was rebuilt must not have the rest of its history blanked
/// by publishing one bead.
#[tokio::test]
async fn publish_ids_preserves_published_records_absent_from_the_store() {
    let repo = Repo::new(&["r-gone"]);
    let store = repo.store();
    store
        .create_bead_full(new_bead("r-new", "fresh"))
        .await
        .unwrap();

    let report = publish_ids(&store, &repo.repo_name(), &repo.root, &s(&["r-new"]))
        .await
        .unwrap();

    assert_eq!(report.inserted, s(&["r-new"]));
    assert_eq!(repo.ids(), s(&["r-gone", "r-new"]));
}

/// A subject naming an id the store does not hold (`rosary-000000` in
/// fixtures) is reported, not an error, and never blanks or dirties anything.
#[tokio::test]
async fn publish_ids_reports_an_id_the_store_does_not_hold_as_missing() {
    let repo = Repo::new(&[]);
    let store = repo.store();
    store.create_bead("r-1", "t", "d", 1, "bug").await.unwrap();
    let name = repo.repo_name();

    let only_missing = publish_ids(&store, &name, &repo.root, &s(&["rosary-000000"]))
        .await
        .unwrap();
    assert_eq!(
        only_missing,
        PublishReport {
            missing: s(&["rosary-000000"]),
            ..Default::default()
        }
    );
    assert_eq!(
        repo.jsonl_status(),
        "",
        "a missing id must not touch the file"
    );

    let mixed = publish_ids(&store, &name, &repo.root, &s(&["r-1", "rosary-000000"]))
        .await
        .unwrap();
    assert_eq!(mixed.inserted, s(&["r-1"]));
    assert_eq!(mixed.missing, s(&["rosary-000000"]));
    assert_eq!(repo.ids(), s(&["r-1"]));
}

/// An untracked `beads.jsonl` is not opted in — for `publish_ids` too.
#[tokio::test]
async fn publish_ids_does_not_publish_to_an_untracked_file() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let beads = root.join(".beads");
    std::fs::create_dir_all(&beads).unwrap();
    std::fs::write(beads.join("beads.jsonl"), "").unwrap();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(root)
        .output()
        .unwrap();
    let store = SqliteBeadStore::connect(&beads.join("beads.db")).unwrap();
    store.create_bead("r-1", "t", "d", 1, "bug").await.unwrap();

    let report = publish_ids(&store, "x", root, &s(&["r-1"])).await.unwrap();

    assert_eq!(report, PublishReport::default());
    assert_eq!(
        std::fs::read_to_string(beads.join("beads.jsonl")).unwrap(),
        ""
    );
}
