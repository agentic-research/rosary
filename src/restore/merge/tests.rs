use super::*;

/// Minimal contract record — the merge compares whole records structurally, so
/// a stub with a marker field is enough to prove which side's *record* survived.
fn rec(id: &str, marker: &str) -> Value {
    serde_json::json!({
        "schema_version": 1,
        "id": id,
        "title": marker,
        "updated_at": "2026-07-01T00:00:00Z",
    })
}

fn ids(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| {
            serde_json::from_str::<Value>(l).unwrap()["id"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect()
}

/// Read a record's marker out of the merged text. Skips non-JSON lines so it
/// also works on a conflicted result (which carries `<<<<<<<` marker lines).
fn marker(text: &str, id: &str) -> String {
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v["id"] == id)
        .unwrap_or_else(|| panic!("{id} missing from merge result"))["title"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Disjoint bead sets union — the common case (two PRs each filed a new bead).
#[test]
fn disjoint_sides_union() {
    let base = vec![rec("r-aaa", "base")];
    let ours = vec![rec("r-aaa", "base"), rec("r-ccc", "ours")];
    let theirs = vec![rec("r-aaa", "base"), rec("r-bbb", "theirs")];
    let out = merge_contract(base, ours, theirs).unwrap();
    assert!(out.is_clean());
    assert_eq!(ids(&out.text), ["r-aaa", "r-bbb", "r-ccc"]);
    assert_eq!(out.added, 2);
    assert_eq!(out.resurrected, 0);
}

/// Only THEIRS changed the bead relative to the ancestor → theirs wins, cleanly.
/// This is an unambiguous edit, not a conflict.
#[test]
fn only_theirs_changed_takes_theirs() {
    let base = vec![rec("r-aaa", "base")];
    let ours = vec![rec("r-aaa", "base")];
    let theirs = vec![rec("r-aaa", "theirs")];
    let out = merge_contract(base, ours, theirs).unwrap();
    assert!(out.is_clean());
    assert_eq!(marker(&out.text, "r-aaa"), "theirs");
    assert_eq!(out.theirs_changed, 1);
    assert_eq!(out.ours_changed, 0);
}

/// Only OURS changed it → ours wins, cleanly.
#[test]
fn only_ours_changed_takes_ours() {
    let base = vec![rec("r-aaa", "base")];
    let ours = vec![rec("r-aaa", "ours")];
    let theirs = vec![rec("r-aaa", "base")];
    let out = merge_contract(base, ours, theirs).unwrap();
    assert!(out.is_clean());
    assert_eq!(marker(&out.text, "r-aaa"), "ours");
    assert_eq!(out.ours_changed, 1);
}

/// Both sides made the SAME change → clean, no conflict, no duplicate line.
#[test]
fn identical_change_on_both_sides_is_clean() {
    let base = vec![rec("r-aaa", "base")];
    let ours = vec![rec("r-aaa", "same")];
    let theirs = vec![rec("r-aaa", "same")];
    let out = merge_contract(base, ours, theirs).unwrap();
    assert!(out.is_clean());
    assert_eq!(out.text.lines().count(), 1);
    assert_eq!(marker(&out.text, "r-aaa"), "same");
}

/// Both sides changed the same bead DIFFERENTLY → genuine conflict. The driver
/// must NOT pick a winner (that would silently destroy one side's real edit);
/// it emits both records in a conflict block and reports failure.
#[test]
fn both_sides_diverged_conflicts_and_keeps_both() {
    let base = vec![rec("r-aaa", "base")];
    let ours = vec![rec("r-aaa", "ours")];
    let theirs = vec![rec("r-aaa", "theirs")];
    let out = merge_contract(base, ours, theirs).unwrap();
    assert!(!out.is_clean());
    assert_eq!(out.conflicts, ["r-aaa"]);
    // BOTH sides' content survives, verbatim — nothing is discarded.
    assert!(out.text.contains("\"ours\""), "{}", out.text);
    assert!(out.text.contains("\"theirs\""), "{}", out.text);
    assert!(out.text.starts_with("<<<<<<< ours\n"), "{}", out.text);
    assert!(out.text.contains("\n=======\n"), "{}", out.text);
    assert!(out.text.ends_with(">>>>>>> theirs"), "{}", out.text);
}

/// Both sides ADDED the same id with different content (no ancestor record) →
/// also a conflict: there is no basis to prefer either.
#[test]
fn both_sides_added_same_id_differently_conflicts() {
    let ours = vec![rec("r-aaa", "ours")];
    let theirs = vec![rec("r-aaa", "theirs")];
    let out = merge_contract(Vec::new(), ours, theirs).unwrap();
    assert_eq!(out.conflicts, ["r-aaa"]);
}

/// A conflict on one bead must not contaminate the rest of the file — every
/// other id still merges, so the human only resolves what actually diverged.
#[test]
fn conflict_is_isolated_to_the_diverged_id() {
    let base = vec![rec("r-aaa", "base"), rec("r-bbb", "base")];
    let ours = vec![
        rec("r-aaa", "ours"),
        rec("r-bbb", "base"),
        rec("r-ccc", "o"),
    ];
    let theirs = vec![
        rec("r-aaa", "theirs"),
        rec("r-bbb", "base"),
        rec("r-ddd", "t"),
    ];
    let out = merge_contract(base, ours, theirs).unwrap();
    assert_eq!(out.conflicts, ["r-aaa"]);
    assert_eq!(marker(&out.text, "r-bbb"), "base");
    assert_eq!(marker(&out.text, "r-ccc"), "o");
    assert_eq!(marker(&out.text, "r-ddd"), "t");
}

/// Output is id-sorted regardless of input order — the export's diff-stability
/// invariant; if the merge emitted them unsorted the next diff would explode.
#[test]
fn output_is_id_sorted() {
    let ours = vec![rec("r-zzz", "o"), rec("r-mmm", "o")];
    let theirs = vec![rec("r-bbb", "t"), rec("r-nnn", "t")];
    let out = merge_contract(Vec::new(), ours, theirs).unwrap();
    assert_eq!(ids(&out.text), ["r-bbb", "r-mmm", "r-nnn", "r-zzz"]);
}

/// Deletion policy: a bead in the ancestor and on exactly one side is kept
/// (resurrected) and counted, so the choice is visible rather than silent.
#[test]
fn one_sided_deletion_is_resurrected_and_counted() {
    let base = vec![rec("r-aaa", "base"), rec("r-bbb", "base")];
    let ours = vec![rec("r-aaa", "base")];
    let theirs = base.clone();
    let out = merge_contract(base, ours, theirs).unwrap();
    assert!(out.is_clean());
    assert_eq!(ids(&out.text), ["r-aaa", "r-bbb"]);
    assert_eq!(out.resurrected, 1);
}

/// A record with no `id` fails LOUD — never a silent drop.
#[test]
fn record_without_id_errors() {
    let bad = vec![serde_json::json!({"title": "no id here"})];
    let err = merge_contract(Vec::new(), bad, Vec::new()).unwrap_err();
    assert!(err.to_string().contains("no usable `id`"), "{err}");
}

/// A duplicate id within one side means the input is already corrupt (e.g. a
/// prior line-wise merge, or an unresolved conflict block) — refuse rather than
/// launder it into the result.
#[test]
fn duplicate_id_within_one_side_errors() {
    let dup = vec![rec("r-aaa", "one"), rec("r-aaa", "two")];
    let err = merge_contract(Vec::new(), dup, Vec::new()).unwrap_err();
    assert!(err.to_string().contains("duplicate bead id"), "{err}");
}

fn write_jsonl(dir: &std::path::Path, name: &str, recs: &[Value]) -> std::path::PathBuf {
    let p = dir.join(name);
    let body = recs
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&p, body).unwrap();
    p
}

/// The file-level driver entry point overwrites `%A` (ours) with the merged
/// text, as gitattributes(5) requires, and leaves no trailing newline (matching
/// `bead export --jsonl`, so the next pre-commit export stages nothing).
#[test]
fn merge_jsonl_files_overwrites_ours() {
    let dir = tempfile::tempdir().unwrap();
    let base = write_jsonl(dir.path(), "base", &[rec("r-aaa", "base")]);
    let ours = write_jsonl(dir.path(), "ours", &[rec("r-aaa", "base")]);
    let theirs = write_jsonl(
        dir.path(),
        "theirs",
        &[rec("r-aaa", "theirs"), rec("r-bbb", "theirs")],
    );

    let out = merge_jsonl_files(&base, &ours, &theirs).unwrap();
    assert!(out.is_clean());
    assert_eq!(out.theirs_changed, 1);
    let written = std::fs::read_to_string(&ours).unwrap();
    assert_eq!(written, out.text);
    assert!(!written.ends_with('\n'));
    assert_eq!(ids(&written), ["r-aaa", "r-bbb"]);
    assert_eq!(marker(&written, "r-aaa"), "theirs");
}

/// A conflicted merge still WRITES `%A` (git needs a working-tree file to
/// resolve) — with both sides preserved — and reports non-clean.
#[test]
fn merge_jsonl_files_writes_conflict_markers() {
    let dir = tempfile::tempdir().unwrap();
    let base = write_jsonl(dir.path(), "base", &[rec("r-aaa", "base")]);
    let ours = write_jsonl(dir.path(), "ours", &[rec("r-aaa", "ours")]);
    let theirs = write_jsonl(dir.path(), "theirs", &[rec("r-aaa", "theirs")]);
    let out = merge_jsonl_files(&base, &ours, &theirs).unwrap();
    assert!(!out.is_clean());
    let written = std::fs::read_to_string(&ours).unwrap();
    assert!(written.contains("<<<<<<< ours"));
    assert!(written.contains("\"ours\"") && written.contains("\"theirs\""));
}

/// Unparseable input fails loud — the driver returns non-zero and git leaves a
/// conflict, rather than writing a file with a bead silently missing.
#[test]
fn unparseable_input_errors() {
    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("good");
    std::fs::write(&good, "").unwrap();
    let bad = dir.path().join("bad");
    std::fs::write(&bad, "{not json at all\n").unwrap();
    assert!(merge_jsonl_files(&good, &good, &bad).is_err());
}
