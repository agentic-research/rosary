//! rosary-664e25: dependency edges and comments must survive the contract
//! round-trip AND reach a peer, the way the tracked projection carries them
//! (export on one clone → post-merge restore on another).
//!
//! Each "peer" test models the real sequence: clone A and clone B start in
//! sync, A makes an edge- or comment-ONLY edit, A exports, B restores. Before
//! the fix, B's last-writer-wins check saw "nothing newer" and skipped the
//! record — and B's post-merge trunk refresh then committed a revert.

use super::*;
use crate::bead_sqlite::SqliteBeadStore;
use crate::store::{BeadStore, NewBead};

const REPO: &str = "rosary";

fn store() -> SqliteBeadStore {
    SqliteBeadStore::connect(std::path::Path::new(":memory:")).unwrap()
}

async fn seed(store: &SqliteBeadStore, ids: &[&str]) {
    for id in ids {
        store
            .create_bead_full(NewBead {
                id: id.to_string(),
                title: format!("bead {id}"),
                ..Default::default()
            })
            .await
            .unwrap();
        // Back-date, so an edit made "now" is unambiguously newer than the
        // synced copy even within the store's one-second timestamp precision.
        let past = chrono::Utc::now() - chrono::Duration::hours(1);
        store.restore_timestamps(id, past, past).await.unwrap();
    }
}

/// The contract export of every bead in `store`, as the restore input.
async fn export(store: &SqliteBeadStore) -> Vec<serde_json::Value> {
    let beads = store.list_all_beads(REPO).await.unwrap();
    crate::import::export_beads_contract_jsonl(store, &beads)
        .await
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

async fn sync(from: &SqliteBeadStore, to: &SqliteBeadStore) {
    restore_beads_from_contract(&export(from).await, to, REPO)
        .await
        .unwrap();
}

/// Two clones holding identical records.
async fn clones(ids: &[&str]) -> (SqliteBeadStore, SqliteBeadStore) {
    let a = store();
    seed(&a, ids).await;
    let b = store();
    sync(&a, &b).await;
    (a, b)
}

#[tokio::test]
async fn a_parent_child_edge_survives_the_round_trip() {
    let a = store();
    seed(&a, &["rosary-par001", "rosary-kid001"]).await;
    a.add_dependency_typed("rosary-kid001", "rosary-par001", "parent-child")
        .await
        .unwrap();

    let b = store();
    sync(&a, &b).await;

    assert_eq!(
        b.get_dependencies_typed("rosary-kid001").await.unwrap(),
        vec![("rosary-par001".to_string(), "parent-child".to_string())],
        "the edge type must round-trip, not collapse to blocks"
    );
    assert_eq!(
        b.get_children("rosary-par001").await.unwrap(),
        vec!["rosary-kid001".to_string()],
        "the close-merged containment gate reads children through the type"
    );
}

#[tokio::test]
async fn an_edge_only_addition_reaches_the_peer() {
    let (a, b) = clones(&["rosary-aaa001", "rosary-bbb001"]).await;
    a.add_dependency("rosary-aaa001", "rosary-bbb001")
        .await
        .unwrap();

    sync(&a, &b).await;

    assert_eq!(
        b.get_dependencies("rosary-aaa001").await.unwrap(),
        vec!["rosary-bbb001".to_string()]
    );
}

#[tokio::test]
async fn an_edge_removal_reaches_the_peer() {
    let (a, b) = clones(&["rosary-aaa002", "rosary-bbb002"]).await;
    a.add_dependency("rosary-aaa002", "rosary-bbb002")
        .await
        .unwrap();
    sync(&a, &b).await;
    assert_eq!(b.get_dependencies("rosary-aaa002").await.unwrap().len(), 1);

    // Back-date A's record again so the removal is the only newer change.
    let past = chrono::Utc::now() - chrono::Duration::minutes(30);
    a.restore_timestamps("rosary-aaa002", past, past)
        .await
        .unwrap();
    b.restore_timestamps("rosary-aaa002", past, past)
        .await
        .unwrap();
    a.remove_dependency("rosary-aaa002", "rosary-bbb002")
        .await
        .unwrap();

    sync(&a, &b).await;

    assert!(
        b.get_dependencies("rosary-aaa002")
            .await
            .unwrap()
            .is_empty(),
        "an insert-only restore can never propagate a removed edge"
    );
}

#[tokio::test]
async fn a_comment_only_addition_reaches_the_peer() {
    let (a, b) = clones(&["rosary-ccc001"]).await;
    a.add_comment("rosary-ccc001", "a finding", "alice")
        .await
        .unwrap();

    sync(&a, &b).await;

    let comments = b.list_comments("rosary-ccc001", true).await.unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0].text, "a finding");
}

/// After a sync the peer re-exports byte-for-byte what it ingested: the
/// restore re-stamps the source timestamps after wiring edges and comments,
/// so the copy never looks newer than its source (no LWW ping-pong, and the
/// tracked projection doesn't churn on every post-merge import).
#[tokio::test]
async fn a_synced_peer_re_exports_identically() {
    let a = store();
    seed(&a, &["rosary-par002", "rosary-kid002"]).await;
    a.add_dependency_typed("rosary-kid002", "rosary-par002", "parent-child")
        .await
        .unwrap();
    a.add_comment("rosary-kid002", "note", "bob").await.unwrap();
    // Pin A's records in the past AFTER its edits: a peer that forgot to
    // re-stamp would carry "now" (its own edge/comment writes) instead.
    let past = chrono::Utc::now() - chrono::Duration::hours(2);
    for id in ["rosary-par002", "rosary-kid002"] {
        a.restore_timestamps(id, past, past).await.unwrap();
    }

    let b = store();
    sync(&a, &b).await;

    let strip = |v: Vec<serde_json::Value>| -> Vec<serde_json::Value> {
        v.into_iter()
            .map(|mut r| {
                // Comment ids and created_at are minted per store.
                if let Some(cs) = r.get_mut("comments").and_then(|c| c.as_array_mut()) {
                    for c in cs {
                        c.as_object_mut().unwrap().remove("id");
                        c.as_object_mut().unwrap().remove("created_at");
                    }
                }
                r
            })
            .collect()
    };
    assert_eq!(strip(export(&b).await), strip(export(&a).await));
}
