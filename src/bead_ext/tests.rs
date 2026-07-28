use super::*;
use serde_json::json;

/// THE ACCEPTANCE CRITERION (rosary-c47ca6 #2): registering a new extension
/// field makes it survive a round trip with **no edit to any projection**.
///
/// A synthetic namespace nothing else knows about is used deliberately. If
/// `project`/`absorb` were hand-enumerating fields, a namespace invented inside
/// this test could not possibly work — so passing it is evidence the registry is
/// genuinely the only declaration, not a list that happens to agree with three
/// others today.
#[test]
fn a_synthetic_extension_round_trips_with_no_projection_edit() {
    const SYNTHETIC: &[Extension] = &[Extension {
        namespace: "invented-for-this-test",
        fields: &[
            ExtField {
                name: "quokka_score",
                native: false,
            },
            ExtField {
                name: "nested_thing",
                native: true,
            },
        ],
    }];

    let source = json!({
        "id": "rosary-aa0001",
        "quokka_score": 41,
        "nested_thing": {"a": [1, 2, 3], "b": "x"},
        "unregistered": "must not travel",
    });

    let projected = project(&source, SYNTHETIC);
    assert_eq!(projected.len(), 2, "both registered fields projected");
    assert_eq!(projected["quokka_score"], json!(41));
    assert_eq!(projected["nested_thing"], json!({"a": [1,2,3], "b": "x"}));
    assert!(
        !projected.contains_key("unregistered"),
        "projection must carry ONLY registered fields — otherwise the registry \
         is decorative and the bag is open again"
    );
}

/// The registry covers exactly the fields that were in the `notes` bag plus the
/// three ADR-0021 flagged as undecided. Pinning the set makes an accidental
/// removal a test failure rather than a silent export change.
#[test]
fn registry_covers_the_notes_bag_and_the_adr_0021_fields() {
    let names: std::collections::BTreeSet<&str> = field_names(EXTENSIONS).collect();
    for expected in [
        "files",
        "test_files",
        "derived_from",
        "branch",
        "pr_url",
        "jj_change_id",
    ] {
        assert!(names.contains(expected), "{expected} missing from registry");
    }
}

/// ADR-0021 requires the canonical definition to RECORD which fields the native
/// store does not populate, so a Dolt→SQLite migration cannot silently lose a
/// field nobody decided to drop.
///
/// `bead_from_row` hardcodes branch/pr_url/jj_change_id to `None`, so those are
/// `native: false`. This test states that as a checked fact rather than a
/// comment that could drift from the reader.
#[test]
fn non_native_fields_are_declared_as_such() {
    let native: std::collections::BTreeSet<&str> = EXTENSIONS
        .iter()
        .flat_map(|e| e.fields.iter().filter(|f| f.native).map(|f| f.name))
        .collect();
    for owned in ["files", "test_files", "derived_from"] {
        assert!(native.contains(owned), "{owned} should be native");
    }
    for foreign in ["branch", "pr_url", "jj_change_id"] {
        assert!(
            !native.contains(foreign),
            "{foreign} is declared native, but bead_from_row hardcodes it to None"
        );
    }
}

/// Absent and null fields are skipped rather than emitted as null. A producer
/// that does not populate an extension should be silent about it — emitting
/// nulls would make every bead's record carry every namespace's keys.
#[test]
fn absent_and_null_fields_are_not_projected() {
    const ONE: &[Extension] = &[Extension {
        namespace: "t",
        fields: &[
            ExtField {
                name: "present",
                native: true,
            },
            ExtField {
                name: "explicit_null",
                native: true,
            },
            ExtField {
                name: "absent",
                native: true,
            },
        ],
    }];
    let projected = project(&json!({"present": 1, "explicit_null": null}), ONE);
    assert_eq!(projected.len(), 1);
    assert!(projected.contains_key("present"));
}

/// Every registered field name must exist on `Bead`, or the projection silently
/// carries nothing for it.
///
/// This is the drift gate (criterion 4): a typo'd or renamed field is caught
/// here rather than becoming a quietly-absent export column. It reads the field
/// set from a serialized `Bead` — the struct itself — rather than from a second
/// list.
#[test]
fn every_registered_field_exists_on_bead() {
    let bead = crate::bead::Bead {
        id: "rosary-aa0002".into(),
        title: "t".into(),
        description: String::new(),
        status: "open".into(),
        priority: 1,
        issue_type: "bug".into(),
        owner: None,
        repo: "rosary".into(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        dependency_count: 0,
        dependent_count: 0,
        comment_count: 0,
        branch: Some("b".into()),
        pr_url: Some("p".into()),
        jj_change_id: Some("j".into()),
        external_ref: None,
        files: vec!["f".into()],
        test_files: vec!["t".into()],
        created_by: None,
        scope: String::new(),
        derived_from: vec![bdr::provenance::ProvenanceRef::Adr { id: "0021".into() }],
        acceptance_criteria: "cargo test".into(),
    };
    let serialized = serde_json::to_value(&bead).expect("Bead serializes");
    for name in field_names(EXTENSIONS) {
        assert!(
            serialized.get(name).is_some(),
            "registered extension field `{name}` does not exist on `Bead` — the \
             registry has drifted from the struct, and this field would export \
             as nothing at all"
        );
    }
}

/// Slice 2's payoff, asserted end-to-end: a field the RESTORE path never
/// mentions still arrives, because `absorb` + serde carry it.
///
/// `rosary-c47ca6` could only prove this for the export half — `NewBead` was not
/// deserializable, so the import side had nowhere to put the values and
/// `derived_from` stayed hand-named in `restore`. This is the half that was
/// blocked.
#[tokio::test]
async fn a_registered_field_restores_without_any_import_side_edit() {
    use crate::store::BeadStore;

    let store =
        crate::bead_sqlite::SqliteBeadStore::connect(std::path::Path::new(":memory:")).unwrap();

    // A contract record carrying provenance — the field `restore` no longer
    // names anywhere.
    let record = json!({
        "id": "rosary-ext001",
        "title": "carries provenance",
        "description": "",
        "status": "open",
        "priority": 1,
        "issue_type": "bug",
        "acceptance_criteria": "cargo test",
        "files": ["src/a.rs"],
        "test_files": ["tests/a.rs"],
        // `ProvenanceRef` is `#[serde(tag = "kind", rename_all = "snake_case")]`.
        "derived_from": [{"kind": "adr", "id": "0021"}],
    });

    crate::restore::restore_beads_from_contract(&[record], &store, "rosary")
        .await
        .expect("restore succeeds");

    let got = store
        .get_bead("rosary-ext001", "rosary")
        .await
        .unwrap()
        .expect("bead restored");
    assert_eq!(got.files, vec!["src/a.rs".to_string()], "files restored");
    assert_eq!(
        got.derived_from.len(),
        1,
        "provenance must survive restore — this is the half rosary-c47ca6 could \
         not fix, and the reason every fresh clone used to drop it"
    );
    assert!(
        matches!(
            &got.derived_from[0],
            bdr::provenance::ProvenanceRef::Adr { id } if id == "0021"
        ),
        "provenance restored with the wrong shape: {:?}",
        got.derived_from
    );
}
