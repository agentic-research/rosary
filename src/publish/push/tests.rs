//! Unit tests for the pre-push gate primitive (rosary-e5c037).

use super::*;

#[test]
fn parse_keeps_updates_and_creates_but_drops_deletes() {
    let input = "refs/heads/a 1111 refs/heads/a 2222\n\
                 refs/heads/new 3333 refs/heads/new 0000000000000000000000000000000000000000\n\
                 (delete) 0000000000000000000000000000000000000000 refs/heads/gone 4444\n\n";
    let refs = parse_prepush_stdin(input).unwrap();
    assert_eq!(
        refs,
        vec![
            PushedRef {
                local_sha: "1111".into(),
                remote_sha: "2222".into()
            },
            PushedRef {
                local_sha: "3333".into(),
                remote_sha: "0000000000000000000000000000000000000000".into()
            },
        ]
    );
}

#[test]
fn parse_rejects_a_malformed_line_rather_than_guessing() {
    assert!(parse_prepush_stdin("refs/heads/a 1111\n").is_err());
}

#[test]
fn named_ids_come_from_brackets_deduplicated_and_sorted() {
    let subjects = vec![
        "[rosary-bbb222] fix: b".to_string(),
        "chore: nothing".to_string(),
        "[rosary-aaa111] feat: a [rosary-bbb222] again".to_string(),
    ];
    assert_eq!(
        named_bead_ids(&subjects),
        vec!["rosary-aaa111", "rosary-bbb222"]
    );
}

#[test]
fn blob_index_keeps_the_exact_line_and_skips_junk() {
    let blob = parse_blob("{\"id\":\"rosary-aaa111\",\"k\":1}\n\nnot json\n{\"no\":\"id\"}\n");
    assert_eq!(blob.records.len(), 1);
    assert_eq!(
        blob.records["rosary-aaa111"],
        "{\"id\":\"rosary-aaa111\",\"k\":1}"
    );
    assert!(blob.duplicates.is_empty());
}

/// Two lines for one id is not an exact projection, whichever comes last.
#[test]
fn blob_index_records_duplicate_ids_instead_of_collapsing_them() {
    let blob =
        parse_blob("{\"id\":\"rosary-dup001\",\"v\":1}\n{\"id\":\"rosary-dup001\",\"v\":2}\n");
    assert_eq!(blob.records.len(), 1);
    assert_eq!(
        blob.duplicates.iter().collect::<Vec<_>>(),
        vec!["rosary-dup001"]
    );
}

/// Ids rosary itself generates carry digits and underscores in the prefix
/// (`sanitize_prefix`: `0day`, `repo_2`); the gate must see them.
#[test]
fn named_ids_accept_digits_and_underscores_in_the_prefix() {
    let subjects = vec![
        "[0day-029928] fix: digits first".to_string(),
        "[repo_2-abc123] feat: underscore".to_string(),
    ];
    assert_eq!(
        named_bead_ids(&subjects),
        vec!["0day-029928", "repo_2-abc123"]
    );
}

/// The comparison against the store: an exact match passes, a byte
/// difference is stale, an absent record is missing, and an id the store
/// does not hold is nobody's problem.
#[tokio::test]
async fn compare_classifies_exact_stale_missing_and_unknown() {
    let store = crate::bead_sqlite::SqliteBeadStore::connect(Path::new(":memory:")).unwrap();
    for id in ["rosary-exact1", "rosary-stale1", "rosary-miss01"] {
        store
            .create_bead_full(crate::store::NewBead {
                id: id.to_string(),
                title: format!("bead {id}"),
                issue_type: "task".to_string(),
                acceptance_criteria: "cargo test".to_string(),
                ..Default::default()
            })
            .await
            .unwrap();
    }
    let exact = store
        .get_bead("rosary-exact1", "rosary")
        .await
        .unwrap()
        .unwrap();
    let exact_line = crate::jsonl_sync::render_bead_line(&store, &exact)
        .await
        .unwrap();
    let stale = store
        .get_bead("rosary-stale1", "rosary")
        .await
        .unwrap()
        .unwrap();
    let stale_line = crate::jsonl_sync::render_bead_line(&store, &stale)
        .await
        .unwrap()
        .replace("bead rosary-stale1", "older title");
    let blob = parse_blob(&format!("{exact_line}\n{stale_line}\n"));

    let ids: Vec<String> = [
        "rosary-exact1",
        "rosary-stale1",
        "rosary-miss01",
        "rosary-unknown",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let d = compare(&store, "rosary", &ids, &blob).await.unwrap();
    assert_eq!(d.missing, vec!["rosary-miss01"]);
    assert_eq!(d.stale, vec!["rosary-stale1"]);
}

/// The refusal is the operator's whole diagnosis: which tip, which ids,
/// in which way, and the exact command that fixes it.
#[test]
fn refusal_names_the_tip_the_ids_and_the_fix() {
    let failures = vec![(
        "abcdef0123456789".to_string(),
        Disagreement {
            missing: vec!["rosary-mis001".into()],
            stale: vec!["rosary-sta001".into()],
        },
    )];
    let msg = refusal_message(&failures);
    assert!(
        msg.contains("abcdef012345: missing rosary-mis001; stale rosary-sta001"),
        "{msg}"
    );
    assert!(
        msg.contains(
            "rsry bead publish rosary-mis001 rosary-sta001 && git commit --amend --no-edit"
        ),
        "{msg}"
    );
}
