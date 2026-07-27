use super::*;

/// A git repo with a tracked file, mirroring the real shape: canonical bead
/// state in the working tree, coordination state in refs.
fn repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path();
    for args in [
        vec!["init", "--quiet"],
        vec!["config", "user.email", "t@example.com"],
        vec!["config", "user.name", "t"],
    ] {
        std::process::Command::new("git")
            .arg("-C")
            .arg(p)
            .args(&args)
            .output()
            .unwrap();
    }
    std::fs::create_dir_all(p.join(".beads")).unwrap();
    std::fs::write(p.join(".beads/beads.jsonl"), "{\"id\":\"x-1\"}\n").unwrap();
    std::process::Command::new("git")
        .arg("-C")
        .arg(p)
        .args(["add", ".beads/beads.jsonl"])
        .output()
        .unwrap();
    std::process::Command::new("git")
        .arg("-C")
        .arg(p)
        .args(["commit", "--quiet", "-m", "seed"])
        .output()
        .unwrap();
    tmp
}

/// THE ADR-0022 EXIT TEST: writing a coordination record must leave the
/// canonical record — and the working tree generally — untouched. This is the
/// whole point; if it fails, coordination traffic is still landing in canonical
/// storage and nothing has been fixed.
#[test]
fn coordination_write_does_not_touch_the_working_tree() {
    let tmp = repo();
    let p = tmp.path();
    let before = std::fs::read(p.join(".beads/beads.jsonl")).unwrap();

    append(
        p,
        "dispatch-abc123",
        r#"{"event":"start","bead":"rosary-fa7167"}"#,
    )
    .unwrap();
    append(p, "dispatch-abc123", r#"{"event":"feedback","ok":true}"#).unwrap();

    let after = std::fs::read(p.join(".beads/beads.jsonl")).unwrap();
    assert_eq!(before, after, "canonical record must be byte-identical");

    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(p)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(
        status.stdout.is_empty(),
        "working tree must be clean, got: {}",
        String::from_utf8_lossy(&status.stdout)
    );
}

#[test]
fn append_then_read_round_trips_in_order() {
    let tmp = repo();
    let p = tmp.path();
    append(p, "d1", r#"{"n":1}"#).unwrap();
    append(p, "d1", r#"{"n":2}"#).unwrap();
    append(p, "d1", r#"{"n":3}"#).unwrap();
    let got = read(p, "d1").unwrap().unwrap();
    assert_eq!(got, "{\"n\":1}\n{\"n\":2}\n{\"n\":3}\n");
}

/// "Never written" and "written empty" must be distinguishable — the same
/// distinction the store/ledger drift kept collapsing.
#[test]
fn unwritten_namespace_is_none_not_empty_string() {
    let tmp = repo();
    assert!(read(tmp.path(), "never-written").unwrap().is_none());
}

#[test]
fn namespaces_are_isolated() {
    let tmp = repo();
    let p = tmp.path();
    append(p, "d1", r#"{"a":1}"#).unwrap();
    append(p, "d2", r#"{"b":2}"#).unwrap();
    assert_eq!(read(p, "d1").unwrap().unwrap(), "{\"a\":1}\n");
    assert_eq!(read(p, "d2").unwrap().unwrap(), "{\"b\":2}\n");
    let mut names = list(p).unwrap();
    names.sort();
    assert_eq!(names, vec!["d1".to_string(), "d2".to_string()]);
}

/// An embedded newline would silently become two records on read. Reject it
/// rather than mangle — a silent split is exactly the class of defect this
/// codebase keeps paying for.
#[test]
fn multiline_and_blank_records_are_rejected() {
    let tmp = repo();
    let p = tmp.path();
    assert!(append(p, "d1", "{\"a\":1}\n{\"b\":2}").is_err());
    assert!(append(p, "d1", "   ").is_err());
    assert!(
        read(p, "d1").unwrap().is_none(),
        "a rejected write must leave nothing behind"
    );
}

#[test]
fn delete_reports_whether_anything_was_removed() {
    let tmp = repo();
    let p = tmp.path();
    append(p, "d1", r#"{"a":1}"#).unwrap();
    assert!(delete(p, "d1").unwrap(), "existing namespace returns true");
    assert!(read(p, "d1").unwrap().is_none());
    assert!(!delete(p, "d1").unwrap(), "second delete reports false");
}

#[test]
fn list_is_empty_before_any_write() {
    let tmp = repo();
    assert!(list(tmp.path()).unwrap().is_empty());
}

/// Coordination refs must not appear as branches — that is the whole reason
/// this tier is invisible to a clone, a PR diff, and GitHub's UI (ADR-0022 Q3).
#[test]
fn coordination_refs_are_not_branches() {
    let tmp = repo();
    let p = tmp.path();
    append(p, "d1", r#"{"a":1}"#).unwrap();
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(p)
        .args(["for-each-ref", "--format=%(refname)", "refs/heads"])
        .output()
        .unwrap();
    let heads = String::from_utf8_lossy(&out.stdout);
    assert!(
        !heads.contains("d1"),
        "coordination ref leaked into refs/heads: {heads}"
    );
}
