//! Byte-identity pin between `jsonl_sync::render_bead_line` (what the
//! pre-push gate compares against, rosary-e5c037) and what
//! `upsert_tracked_bead` writes.

use super::*;

/// `verify_pushed` compares the pushed blob's line against
/// `jsonl_sync::render_bead_line`. That is only an exact check if the line
/// `upsert_tracked_bead` actually writes IS that rendering, byte for byte —
/// pinned here rather than assumed, so a divergence in either renderer turns
/// the gate's "stale" verdict from a fact into a guess and this test red.
#[tokio::test]
async fn render_bead_line_is_the_line_upsert_writes() {
    let repo = Repo::new(&[]);
    let store = repo.store();
    let repo_name = repo
        .root
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    store
        .create_bead_full(NewBead {
            id: "rosary-render1".to_string(),
            title: "rendered by upsert".to_string(),
            issue_type: "task".to_string(),
            acceptance_criteria: "cargo test render".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
    store
        .add_comment("rosary-render1", "a comment travels too", "t")
        .await
        .unwrap();

    // Under ADR-0024 amendment A the store write above publishes nothing;
    // the commit-time path (P2) publishes through this exact primitive.
    let changed = crate::jsonl_sync::upsert_tracked_bead(
        &store,
        "rosary-render1",
        &repo_name,
        &repo.root,
        true,
    )
    .await
    .unwrap();
    assert!(changed, "upsert must insert a bead the projection lacks");
    let written = std::fs::read_to_string(repo.root.join(".beads/beads.jsonl")).unwrap();
    let written_line = written
        .lines()
        .find(|l| l.contains("\"rosary-render1\""))
        .expect("upsert wrote the record");
    let bead = store
        .get_bead("rosary-render1", &repo_name)
        .await
        .unwrap()
        .unwrap();
    let rendered = crate::jsonl_sync::render_bead_line(&store, &bead)
        .await
        .unwrap();
    assert_eq!(written_line, rendered);
    assert!(rendered.contains("a comment travels too"));
}
