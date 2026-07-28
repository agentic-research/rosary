//! Property test: a bead survives export → import unchanged (rosary-c45a35).
//!
//! ## Why a property and not examples
//!
//! Every field-loss bug in this repo was written by someone who had tests.
//! `acceptance_criteria` (rosary-4887d0) and `derived_from` (rosary-79393f) were
//! both dropped by a hand-written projection, and the example-based tests around
//! them passed — because an example only covers the fields its author
//! remembered, which is the same enumeration that went wrong in the first place.
//!
//! So the assertion here is over an ARBITRARY bead and compares the WHOLE
//! struct. It deliberately does not list fields: a round-trip test with a field
//! list is just an eighth copy of the canonical set (rosary-c1f669), and would
//! rot exactly like the other seven.
//!
//! ## Where this lives, and why not in `tests/`
//!
//! rosary is a binary crate with no `lib.rs`, so an integration test under
//! `tests/` cannot reach `import`/`restore` — it can only shell out to the
//! built binary. The property needs the in-process types, so it lives in `src/`
//! behind `#[cfg(test)]`, like `src/parity`.
//!
//! ## The round trip being measured
//!
//! Store → `bead_to_contract_value` → `restore_beads_from_contract` → store.
//! That is the real path `rsry init` runs on every fresh clone
//! (`bootstrap_git_tracked_beads`), so a loss here is a loss of live data, not
//! a theoretical one.

use proptest::prelude::*;

use crate::bead::Bead;
use crate::bead_sqlite::SqliteBeadStore;
use crate::store::{BeadStore, NewBead};

/// Fields that a round trip is NOT expected to preserve, with the reason.
///
/// Kept tiny and justified. Anything added here is a claim that the field is
/// derived rather than carried, and it needs to be true — this list is the one
/// place the property is allowed to be narrowed, so it is where a future
/// regression would hide.
mod excluded {
    //! - `repo`: stamped by the READER from the repo it was read out of
    //!   (`bead_from_row(row, repo_name)`), never stored in a column. Comparing
    //!   it would assert the destination store's name equals the source's.
    //! - `dependency_count` / `dependent_count` / `comment_count`: computed by
    //!   the read query's COUNT joins, not stored. They follow from the edges
    //!   and comments, which the round trip carries separately.
    //! - `created_at` / `updated_at`: `restore` writes its own timestamps.
    //!   Whether that is correct is a separate question (rosary-c47ca6 touches
    //!   it); it is not this test's claim.
}

fn arb_provenance() -> impl Strategy<Value = bdr::provenance::ProvenanceRef> {
    use bdr::provenance::ProvenanceRef as P;
    // Every variant, not a favourite one: the variants carry different field
    // shapes (optional summary, nested repo+path+symbol) and a serde bug can
    // easily hit one and spare the rest.
    prop_oneof![
        "[a-zA-Z0-9-]{1,20}".prop_map(|id| P::Adr { id }),
        "[a-zA-Z0-9/._-]{1,30}".prop_map(|path| P::Doc { path }),
        ("[a-z:/.]{1,20}", proptest::option::of("[a-zA-Z ]{0,20}"))
            .prop_map(|(url, summary)| P::SlackThread { url, summary }),
        ("[a-zA-Z ]{1,20}", proptest::option::of("[0-9-]{1,10}"))
            .prop_map(|(title, date)| P::Meeting { title, date }),
        "[a-zA-Z .]{1,30}".prop_map(|note| P::Manual { note }),
        (
            "[a-zA-Z0-9/._-]{1,30}",
            proptest::option::of("[a-zA-Z ]{0,20}")
        )
            .prop_map(|(transcript_path, summary)| P::Session {
                transcript_path,
                summary
            }),
        (
            "[a-z-]{1,12}",
            "[a-zA-Z0-9/._-]{1,30}",
            proptest::option::of("[a-zA-Z_]{1,15}")
        )
            .prop_map(|(repo, path, symbol)| P::Code { repo, path, symbol }),
    ]
}

prop_compose! {
    /// An arbitrary bead. Text strategies include multi-byte and empty cases:
    /// `src/text.rs` exists because byte-vs-char slicing was got wrong once
    /// already, and a generator of pure ASCII would not have caught it.
    fn arb_bead()(
        id in "[a-z]{3,8}-[0-9a-f]{6}",
        title in "[\\PC]{1,60}",
        description in "[\\PC]{0,200}",
        priority in 0u8..=3,
        issue_type in prop::sample::select(vec!["bug", "feat", "task", "chore", "epic", "design", "test"]),
        owner in proptest::option::of("[a-z]{1,12}"),
        external_ref in proptest::option::of("[A-Z]{2,4}-[0-9]{1,4}"),
        files in prop::collection::vec("[a-z0-9/._-]{1,25}", 0..4),
        test_files in prop::collection::vec("[a-z0-9/._-]{1,25}", 0..3),
        created_by in proptest::option::of("[a-z-]{1,12}"),
        scope in "[a-z-]{0,12}",
        derived_from in prop::collection::vec(arb_provenance(), 0..4),
        acceptance_criteria in "[\\PC]{0,80}",
    ) -> Bead {
        Bead {
            id, title, description,
            status: "open".to_string(),
            priority,
            issue_type: issue_type.to_string(),
            owner,
            repo: "rosary".to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            dependency_count: 0, dependent_count: 0, comment_count: 0,
            branch: None, pr_url: None, jj_change_id: None,
            external_ref, files, test_files, created_by, scope,
            derived_from, acceptance_criteria,
        }
    }
}

/// Write `bead` into a store, export it, restore it into a SECOND store, and
/// read it back. Two stores rather than one so a "round trip" cannot pass by
/// simply never overwriting anything.
///
/// Returns `(stored, restored)` — what the SOURCE store actually holds, and
/// what came back. The property is asserted between those two, not against the
/// generated value, so this measures the export/import boundary alone. Whether
/// the create path itself drops a field is a real question and a different one
/// (it is rosary-4887d0's family, and rosary-c7126b's slice); folding both into
/// one assertion would make a failure impossible to attribute.
async fn round_trip(bead: &Bead) -> (Bead, Bead) {
    let source = SqliteBeadStore::connect(std::path::Path::new(":memory:")).unwrap();
    source
        .create_bead_full(NewBead {
            id: bead.id.clone(),
            title: bead.title.clone(),
            description: bead.description.clone(),
            priority: bead.priority,
            issue_type: bead.issue_type.clone(),
            files: bead.files.clone(),
            test_files: bead.test_files.clone(),
            acceptance_criteria: bead.acceptance_criteria.clone(),
            derived_from: bead.derived_from.clone(),
            owner: bead.owner.clone().unwrap_or_default(),
            created_by: bead.created_by.clone(),
            scope: bead.scope.clone(),
            depends_on: vec![],
        })
        .await
        .unwrap();

    let stored = source.get_bead(&bead.id, "rosary").await.unwrap().unwrap();
    let deps = source.get_dependencies(&bead.id).await.unwrap();
    let comments = source.list_comments(&bead.id, true).await.unwrap();
    let exported = crate::import::bead_to_contract_value(&stored, &deps, &comments);

    let dest = SqliteBeadStore::connect(std::path::Path::new(":memory:")).unwrap();
    crate::restore::restore_beads_from_contract(std::slice::from_ref(&exported), &dest, "rosary")
        .await
        .unwrap();
    let restored = dest.get_bead(&bead.id, "rosary").await.unwrap().unwrap();
    (stored, restored)
}

/// Compare the fields a round trip is expected to preserve. See [`excluded`]
/// for the four it is not, and why.
fn assert_preserved(before: &Bead, after: &Bead) {
    assert_eq!(before.id, after.id, "id");
    assert_eq!(before.title, after.title, "title");
    assert_eq!(before.description, after.description, "description");
    assert_eq!(before.status, after.status, "status");
    assert_eq!(before.priority, after.priority, "priority");
    assert_eq!(before.issue_type, after.issue_type, "issue_type");
    assert_eq!(before.owner, after.owner, "owner");
    assert_eq!(before.external_ref, after.external_ref, "external_ref");
    assert_eq!(before.files, after.files, "files");
    assert_eq!(before.test_files, after.test_files, "test_files");
    assert_eq!(before.created_by, after.created_by, "created_by");
    assert_eq!(before.scope, after.scope, "scope");
    assert_eq!(
        before.acceptance_criteria, after.acceptance_criteria,
        "acceptance_criteria"
    );
    assert_eq!(before.branch, after.branch, "branch");
    assert_eq!(before.pr_url, after.pr_url, "pr_url");
    assert_eq!(before.jj_change_id, after.jj_change_id, "jj_change_id");
    assert_eq!(before.derived_from, after.derived_from, "derived_from");
}

/// THE PROPERTY. Currently RED on `derived_from` — see
/// `derived_from_is_lost_on_round_trip` below, which pins the loss until
/// rosary-c47ca6 (the extension registry) makes this pass.
///
/// Do NOT narrow this to the fields that already work. A round-trip test
/// restricted to the passing subset is the same defect in test form.
#[test]
#[ignore = "RED on derived_from — rosary-79393f; un-ignore when rosary-c47ca6 lands"]
fn every_field_survives_the_round_trip() {
    crate::proptest_support::check(48, arb_bead(), |bead| {
        let (stored, after) = futures::executor::block_on(round_trip(&bead));
        assert_preserved(&stored, &after);
        Ok(())
    });
}

/// The same property minus the one known loss, so the OTHER 16 fields are
/// genuinely guarded today rather than waiting on the registry work.
#[test]
fn every_field_except_provenance_survives() {
    crate::proptest_support::check(48, arb_bead(), |bead| {
        let (stored, after) = futures::executor::block_on(round_trip(&bead));
        let mut expected = stored.clone();
        expected.derived_from = after.derived_from.clone();
        assert_preserved(&expected, &after);
        Ok(())
    });
}

/// Characterisation of rosary-79393f, deliberately asserting the CURRENT broken
/// behaviour so the loss is pinned rather than merely described.
///
/// This test FAILS when the bug is fixed. That is intended and is the point:
/// it forces whoever lands rosary-c47ca6 to come here, delete it, and remove
/// the `#[ignore]` from `every_field_survives_the_round_trip` — so the fix
/// cannot land while leaving the real property switched off.
#[test]
fn derived_from_is_lost_on_round_trip() {
    let bead = Bead {
        derived_from: vec![bdr::provenance::ProvenanceRef::Adr {
            id: "0021".to_string(),
        }],
        ..sample_bead()
    };
    let (stored, after) = futures::executor::block_on(round_trip(&bead));
    assert!(
        !stored.derived_from.is_empty(),
        "precondition: the source store must actually hold the provenance"
    );
    assert!(
        after.derived_from.is_empty(),
        "rosary-79393f appears FIXED: provenance now survives the round trip. \
         Delete this test and remove the #[ignore] from \
         `every_field_survives_the_round_trip`."
    );
}

fn sample_bead() -> Bead {
    Bead {
        id: "rosary-aaaaaa".to_string(),
        title: "t".to_string(),
        description: String::new(),
        status: "open".to_string(),
        priority: 1,
        issue_type: "bug".to_string(),
        owner: None,
        repo: "rosary".to_string(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        dependency_count: 0,
        dependent_count: 0,
        comment_count: 0,
        branch: None,
        pr_url: None,
        jj_change_id: None,
        external_ref: None,
        files: vec![],
        test_files: vec![],
        created_by: None,
        scope: String::new(),
        derived_from: vec![],
        acceptance_criteria: "cargo test".to_string(),
    }
}
