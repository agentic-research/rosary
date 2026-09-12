use super::*;
use crate::bead_sqlite::SqliteBeadStore;
use crate::jsonl_sync::{
    PublishReport, export_published_beads_contract_jsonl, publish_ids, refresh_tracked_beads_jsonl,
};
use serde_json::Value;
use std::path::PathBuf;

mod publish_ids_tests;

/// A repo with a git-tracked `.beads/beads.jsonl` holding `seed_ids`.
///
/// The tracked check and the cleanliness check both shell out to git, so this
/// really does init a repo and commit the file. Faking that would test the fake.
struct Repo {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Repo {
    fn new(seed_ids: &[&str]) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let beads = root.join(".beads");
        std::fs::create_dir_all(&beads).unwrap();
        let seeded = seed_ids
            .iter()
            .map(|id| serde_json::json!({"id": id}).to_string())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(beads.join("beads.jsonl"), seeded).unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@example.com"],
            vec!["config", "user.name", "t"],
            vec!["add", ".beads/beads.jsonl"],
            vec!["commit", "-qm", "seed"],
        ] {
            let ok = std::process::Command::new("git")
                .args(&args)
                .current_dir(&root)
                .output()
                .unwrap();
            assert!(ok.status.success(), "git {args:?}: {ok:?}");
        }
        Self { _tmp: tmp, root }
    }

    fn store(&self) -> PublishingBeadStore {
        let beads = self.root.join(".beads");
        let inner = SqliteBeadStore::connect(&beads.join("beads.db")).unwrap();
        PublishingBeadStore::new(Box::new(inner), &beads)
    }

    fn repo_name(&self) -> String {
        self.root
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    /// `git status --porcelain` scoped to the tracked file, trimmed. Empty means
    /// the working tree is clean — the observation ADR-0024 amendment A is
    /// about, taken from git rather than from the store.
    fn jsonl_status(&self) -> String {
        let out = std::process::Command::new("git")
            .args(["status", "--porcelain", "--", ".beads/beads.jsonl"])
            .current_dir(&self.root)
            .output()
            .unwrap();
        assert!(out.status.success(), "git status: {out:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn raw(&self) -> String {
        std::fs::read_to_string(self.root.join(".beads/beads.jsonl")).unwrap()
    }

    fn published(&self) -> Vec<Value> {
        self.raw()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn ids(&self) -> Vec<String> {
        self.published()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect()
    }

    fn record(&self, id: &str) -> Option<Value> {
        self.published()
            .into_iter()
            .find(|r| r["id"] == serde_json::json!(id))
    }
}

fn new_bead(id: &str, title: &str) -> NewBead {
    NewBead {
        id: id.to_string(),
        title: title.to_string(),
        issue_type: "bug".to_string(),
        acceptance_criteria: "cargo test".to_string(),
        ..Default::default()
    }
}

fn s(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|i| i.to_string()).collect()
}

/// THE INVERSION of rosary-8ca6e5's regression test. `persist_status` drives
/// `update_status`, the loudest write path; under write-through every dispatch
/// transition rewrote the tracked file on whatever branch was checked out
/// (rosary-3d455a). Now the store moves and the tree stays clean.
///
/// Not vacuous: `r-1` IS published (seeded and committed), so under
/// write-through both the create and the transition would have dirtied it.
#[tokio::test]
async fn a_status_transition_leaves_the_projection_untouched() {
    let repo = Repo::new(&["r-1"]);
    let store = repo.store();
    store
        .create_bead_full(new_bead("r-1", "a bead"))
        .await
        .unwrap();
    store.update_status("r-1", "in_progress").await.unwrap();

    let stored = store.get_status("r-1").await.unwrap().expect("bead exists");
    assert_ne!(stored, "open", "the transition happened in the store");
    assert_eq!(
        repo.jsonl_status(),
        "",
        "a store write must not dirty the tracked projection"
    );
    assert_eq!(
        repo.record("r-1").unwrap(),
        serde_json::json!({"id": "r-1"}),
        "the committed record is untouched"
    );
}

/// A create — both surfaces — leaves the file exactly as committed.
#[tokio::test]
async fn a_create_leaves_the_projection_untouched() {
    let repo = Repo::new(&[]);
    let store = repo.store();
    store.create_bead("r-1", "t", "d", 1, "bug").await.unwrap();
    store
        .create_bead_full(new_bead("r-2", "two"))
        .await
        .unwrap();

    assert!(
        store
            .get_bead("r-1", &repo.repo_name())
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(repo.jsonl_status(), "", "a create must not dirty the tree");
    assert!(
        repo.ids().is_empty(),
        "nothing is published until a commit names it"
    );
}

/// The rosary-a7ee3a boundary, still true with the write gone: touching a
/// store-only bead never leaks it into the published set.
#[tokio::test]
async fn an_update_of_a_store_only_bead_leaves_the_projection_untouched() {
    let repo = Repo::new(&["r-published"]);
    let beads = repo.root.join(".beads");
    let inner = SqliteBeadStore::connect(&beads.join("beads.db")).unwrap();
    inner
        .create_bead_full(new_bead("r-secret", "private"))
        .await
        .unwrap();
    drop(inner);

    let store = repo.store();
    store
        .update_status("r-secret", "in_progress")
        .await
        .unwrap();

    assert_eq!(repo.jsonl_status(), "");
    assert_eq!(repo.ids(), s(&["r-published"]));
}

/// A published-but-absent-locally record sits next to a store-only create and
/// neither side is disturbed.
#[tokio::test]
async fn a_create_alongside_a_published_but_absent_record_leaves_the_file_untouched() {
    let repo = Repo::new(&["r-gone"]);
    let store = repo.store();
    store
        .create_bead_full(new_bead("r-new", "fresh"))
        .await
        .unwrap();

    assert_eq!(repo.jsonl_status(), "");
    assert_eq!(repo.ids(), s(&["r-gone"]));
}

/// Every `Update`-classified path on a PUBLISHED bead: the store changes, the
/// tree does not.
#[tokio::test]
async fn comments_and_dependencies_leave_the_projection_untouched() {
    let repo = Repo::new(&["r-1"]);
    let store = repo.store();
    store
        .create_bead_full(new_bead("r-1", "one"))
        .await
        .unwrap();
    store
        .create_bead_full(new_bead("r-2", "two"))
        .await
        .unwrap();

    store.add_comment("r-1", "a note", "tester").await.unwrap();
    store.add_dependency("r-1", "r-2").await.unwrap();
    store
        .update_bead_fields(
            "r-1",
            &BeadUpdate {
                title: Some("renamed".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    // Exercises the decorator's own `close_bead` classification; the
    // close-condition gate lives in bead_ops, above this seam.
    store.close_bead("r-1").await.unwrap(); // nosemgrep: bead-close-bypasses-gate

    assert_eq!(
        store.get_status("r-1").await.unwrap().as_deref(),
        Some("done")
    );
    assert_eq!(store.list_comments("r-1", false).await.unwrap().len(), 1);
    assert_eq!(store.get_dependencies("r-1").await.unwrap(), s(&["r-2"]));
    assert_eq!(repo.jsonl_status(), "", "updates must not dirty the tree");
    assert_eq!(
        repo.record("r-1").unwrap(),
        serde_json::json!({"id": "r-1"})
    );
}

/// A status CORRECTION (`bead correct`, rosary-e0e19f) writes through
/// `set_status_verbatim`; it is classified as an update and, like every update,
/// leaves the tree clean until a commit names the bead.
#[tokio::test]
async fn a_status_correction_leaves_the_projection_untouched() {
    let repo = Repo::new(&["r-1"]);
    let store = repo.store();
    store
        .create_bead_full(new_bead("r-1", "wrongly closed"))
        .await
        .unwrap();
    store.set_status_verbatim("r-1", "done").await.unwrap();

    crate::bead_correct::correct_status(
        &store,
        "r-1",
        "open",
        "auto-closed with acceptance criteria unmet",
    )
    .await
    .unwrap();

    assert_eq!(
        store.get_status("r-1").await.unwrap().as_deref(),
        Some("open")
    );
    assert_eq!(
        repo.jsonl_status(),
        "",
        "a correction must not dirty the tree"
    );
}

/// The `Whole` kind used to trigger a full-file refresh. Now nothing.
#[tokio::test]
async fn a_comment_delete_leaves_the_projection_untouched() {
    let repo = Repo::new(&["r-1"]);
    let store = repo.store();
    store
        .create_bead_full(new_bead("r-1", "one"))
        .await
        .unwrap();
    store.add_comment("r-1", "soft", "tester").await.unwrap();
    store.add_comment("r-1", "hard", "tester").await.unwrap();
    let comments = store.list_comments("r-1", false).await.unwrap();
    assert_eq!(comments.len(), 2);

    store.delete_comment(&comments[0].id, None).await.unwrap();
    store.hard_delete_comment(&comments[1].id).await.unwrap();

    assert!(store.list_comments("r-1", false).await.unwrap().is_empty());
    assert_eq!(
        repo.jsonl_status(),
        "",
        "a whole-kind write must not dirty the tree"
    );
}

/// A repo that has not opted in stays opted out. Publication is the owner's
/// decision; a write must not create the tracked file.
#[tokio::test]
async fn does_not_create_a_projection_that_does_not_exist() {
    let tmp = tempfile::tempdir().unwrap();
    let beads = tmp.path().join(".beads");
    std::fs::create_dir_all(&beads).unwrap();
    let inner = SqliteBeadStore::connect(&beads.join("beads.db")).unwrap();
    let store = PublishingBeadStore::new(Box::new(inner), &beads);

    store
        .create_bead_full(new_bead("r-1", "one"))
        .await
        .unwrap();

    assert!(
        !beads.join("beads.jsonl").exists(),
        "publication is opt-in; a write must not create the tracked file"
    );
}

/// An untracked `beads.jsonl` is also not opted in — the file existing is not
/// consent, being committed is.
#[tokio::test]
async fn does_not_publish_to_an_untracked_file() {
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

    let inner = SqliteBeadStore::connect(&beads.join("beads.db")).unwrap();
    let store = PublishingBeadStore::new(Box::new(inner), &beads);
    store
        .create_bead_full(new_bead("r-1", "one"))
        .await
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(beads.join("beads.jsonl")).unwrap(),
        "",
        "an uncommitted jsonl is not an opt-in"
    );
}

/// Dolt repos project elsewhere; the wrapper must not engage.
#[test]
fn dolt_repos_have_no_projection() {
    let tmp = tempfile::tempdir().unwrap();
    let beads = tmp.path().join(".beads");
    std::fs::create_dir_all(beads.join("dolt")).unwrap();
    assert!(
        Projection::discover(&beads).is_none(),
        "a Dolt store must not get a JSONL projection"
    );
}

/// Every `BeadStore` method is classified, and the classification is not a
/// comment — it is checked against the trait as the compiler sees it.
///
/// The compiler already forces mod.rs to implement every REQUIRED method (there
/// is no blanket forward). This test guards the other half: provided methods,
/// the count in the module docs, and the reviewer's mental model match reality.
#[test]
fn every_trait_method_is_classified() {
    // The wider vocabulary the runtime enum deliberately does not carry: a
    // method can be a projected write, a write the JSONL contract does not
    // represent, or a read. `log_event` is the only UnprojectedWrite —
    // `bead_to_contract_value` renders no event stream, so an event changes
    // nothing the projection would ever show.
    //
    // Adding a required trait method breaks the build in mod.rs first; this
    // catches the case where someone adds a method (required or provided) but
    // forgets to decide what it means for the projection.
    #[derive(Debug, PartialEq, Eq)]
    enum Class {
        Writes(Projected),
        UnprojectedWrite,
        Read,
    }
    use Class::{Read, UnprojectedWrite, Writes};

    const CLASSIFIED: &[(&str, Class)] = &[
        ("list_beads", Read),
        ("list_all_beads", Read),
        ("list_beads_scoped", Read),
        ("get_bead", Read),
        ("get_status", Read),
        ("search_beads", Read),
        ("search_beads_fts", Read),
        ("get_external_ref", Read),
        ("find_by_external_ref", Read),
        ("list_closed_linked_beads", Read),
        ("get_dependencies", Read),
        ("get_dependents", Read),
        ("get_children", Read),
        ("list_comments", Read),
        ("get_latest_event", Read),
        ("list_event_details", Read),
        ("create_bead", Writes(Projected::Create)),
        ("create_bead_full", Writes(Projected::Create)),
        ("update_bead_fields", Writes(Projected::Update)),
        ("update_status", Writes(Projected::Update)),
        ("set_status_verbatim", Writes(Projected::Update)),
        ("close_bead", Writes(Projected::Update)),
        ("set_assignee", Writes(Projected::Update)),
        ("set_user_id", Writes(Projected::Update)),
        ("set_files", Writes(Projected::Update)),
        ("set_external_ref", Writes(Projected::Update)),
        ("add_dependency", Writes(Projected::Update)),
        ("add_dependency_typed", Writes(Projected::Update)),
        ("remove_dependency", Writes(Projected::Update)),
        ("add_comment", Writes(Projected::Update)),
        ("update_comment", Writes(Projected::Update)),
        ("delete_comment", Writes(Projected::Whole)),
        ("hard_delete_comment", Writes(Projected::Whole)),
        ("log_event", UnprojectedWrite),
    ];

    let src = include_str!("../../store.rs");
    let trait_body = src
        .split_once("pub trait BeadStore")
        .expect("BeadStore trait present")
        .1;
    let end = trait_body.find("\n}").expect("trait terminates");
    let declared: std::collections::BTreeSet<&str> = trait_body[..end]
        .lines()
        .filter_map(|l| l.trim().strip_prefix("async fn "))
        .filter_map(|l| l.split(['(', '<']).next())
        .collect();

    let classified: std::collections::BTreeSet<&str> =
        CLASSIFIED.iter().map(|(name, _)| *name).collect();

    assert_eq!(
        declared,
        classified,
        "BeadStore methods and their projection classification have diverged.\n  \
         in trait, unclassified: {:?}\n  classified, not in trait: {:?}",
        declared.difference(&classified).collect::<Vec<_>>(),
        classified.difference(&declared).collect::<Vec<_>>(),
    );
    assert_eq!(
        declared.len(),
        34,
        "trait size changed; re-read the classes"
    );
}

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
