use super::*;
use crate::bead_sqlite::SqliteBeadStore;
use serde_json::Value;

/// A repo with a git-tracked `.beads/beads.jsonl` holding `seed_ids`.
///
/// The tracked check really does shell out to git, so this really does init a
/// repo and commit the file. Faking that would test the fake.
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

    fn published(&self) -> Vec<Value> {
        let text = std::fs::read_to_string(self.root.join(".beads/beads.jsonl")).unwrap();
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
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

/// THE REGRESSION. `persist_status` drives `update_status`, which was the
/// loudest unwired path: every dispatch transition landed in the store and
/// never reached the tracked file.
#[tokio::test]
async fn update_status_reaches_the_tracked_projection() {
    let repo = Repo::new(&[]);
    let store = repo.store();
    store
        .create_bead_full(new_bead("r-1", "a bead"))
        .await
        .unwrap();
    assert_eq!(repo.record("r-1").unwrap()["status"], "open");

    store.update_status("r-1", "in_progress").await.unwrap();

    // Assert the projection agrees with the STORE rather than with a literal.
    // The store canonicalises aliases (`in_progress` is stored as `dispatched`),
    // and the invariant that matters is "the file says what the store says" —
    // hardcoding the canonical spelling would make this test a change-detector
    // for the state machine instead of a check on publication.
    let stored = store.get_status("r-1").await.unwrap().expect("bead exists");
    assert_ne!(stored, "open", "the transition actually happened");
    assert_eq!(
        repo.record("r-1").unwrap()["status"],
        serde_json::json!(stored),
        "a status transition must reach the tracked file without the caller opting in"
    );
}

/// Creates broaden the published set by exactly one id.
#[tokio::test]
async fn create_publishes_the_new_bead() {
    let repo = Repo::new(&[]);
    let store = repo.store();
    store.create_bead("r-1", "t", "d", 1, "bug").await.unwrap();
    let rec = repo.record("r-1").expect("created bead is published");
    assert_eq!(rec["title"], "t");
}

/// The rosary-a7ee3a boundary: an UPDATE must never publish an id the owner
/// has not published. Without this, a store-only bead would leak into the
/// tracked record the first time anything touched it.
#[tokio::test]
async fn update_never_adds_an_unpublished_bead() {
    let repo = Repo::new(&["r-published"]);
    // Seed a bead directly in the inner store so it is store-only.
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

    let ids: Vec<_> = repo
        .published()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        ids,
        vec!["r-published"],
        "an update must not broaden the published id set"
    );
}

/// Published-but-absent-locally records survive. A repo whose store was
/// rebuilt must not have the rest of its history blanked by one write.
#[tokio::test]
async fn preserves_published_records_absent_from_the_store() {
    let repo = Repo::new(&["r-gone"]);
    let store = repo.store();
    store
        .create_bead_full(new_bead("r-new", "fresh"))
        .await
        .unwrap();

    let ids: Vec<_> = repo
        .published()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    assert!(ids.contains(&"r-gone".to_string()), "got {ids:?}");
    assert!(ids.contains(&"r-new".to_string()), "got {ids:?}");
}

/// Comments and dependencies are part of the contract, so both must publish.
#[tokio::test]
async fn comments_and_dependencies_publish() {
    let repo = Repo::new(&[]);
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

    let rec = repo.record("r-1").unwrap();
    assert_eq!(
        rec["dependencies"],
        serde_json::json!(["r-2"]),
        "dependency edge must publish"
    );
    let comments = rec["comments"].as_array().expect("comments array");
    assert_eq!(comments.len(), 1, "comment must publish: {rec}");
    assert_eq!(
        comments[0]["text"], "a note",
        "the contract renders a comment as `text` (bead::Comment), not `body`"
    );
}

/// The single-record splice must be field-identical to the whole-file render.
///
/// The fast path renders from `get_bead`; the bounded path renders from
/// `list_all_beads`. They share `bead_read_sql` and `bead_from_row` today, but
/// "today" is exactly the assumption ADR-0021 was written about — so pin it
/// rather than trust it. If the two projections ever diverge, this fails
/// instead of silently writing a lossy record.
#[tokio::test]
async fn upsert_matches_full_refresh_field_for_field() {
    let repo = Repo::new(&[]);
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
    let spliced = repo.record("r-1").expect("spliced record");

    // Now force the bounded whole-file path over the same state.
    crate::jsonl_sync::refresh_tracked_beads_jsonl(
        store.inner.as_ref(),
        &repo.root.file_name().unwrap().to_string_lossy(),
        &repo.root,
    )
    .await
    .unwrap();
    let refreshed = repo.record("r-1").expect("refreshed record");

    assert_eq!(
        spliced, refreshed,
        "single-record splice diverged from the whole-file render"
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
/// The compiler already forces this file to implement all 33 methods (there is
/// no blanket forward). This test guards the *other* half: that the count in
/// the module docs, and the reviewer's mental model, match reality.
#[test]
fn every_trait_method_is_classified() {
    // The wider vocabulary the runtime enum deliberately does not carry: a
    // method can be a projected write, a write the JSONL contract does not
    // represent, or a read. `log_event` is the only UnprojectedWrite —
    // `bead_to_contract_value` renders no event stream, so republishing after
    // one would rewrite 3 MB to produce a byte-identical file.
    //
    // Adding a trait method breaks the build in mod.rs first (there is no
    // blanket forward); this catches the case where someone adds it to the
    // impl but forgets to decide what it means for the projection.
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

    let src = include_str!("../store.rs");
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
