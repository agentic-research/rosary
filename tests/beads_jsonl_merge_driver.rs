//! End-to-end proof that the `beads-jsonl` git merge driver (rosary-f9516f) is
//! really wired into `git merge` — not just unit-tested in isolation.
//!
//! Each test drives a REAL `git merge` in a temp repo whose config points
//! `merge.beads-jsonl.driver` at the built `rsry` binary, and asserts an
//! outcome git's default text merge provably could not produce. The
//! `control_*` test runs the identical scenario WITHOUT the driver configured
//! and asserts the different (text-merge) result — that contrast is the
//! evidence the driver ran.

use std::path::{Path, PathBuf};
use std::process::Command;

fn rsry() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rsry"))
}

fn git(repo: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("invoking git")
}

fn git_ok(repo: &Path, args: &[&str]) {
    let out = git(repo, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// One JSONL record. Only `id` and the marker field matter to the merge.
fn rec(id: &str, title: &str) -> String {
    serde_json::json!({"schema_version": 1, "id": id, "title": title, "updated_at": "2026-07-01T00:00:00Z"})
        .to_string()
}

fn write_export(repo: &Path, lines: &[String]) {
    std::fs::create_dir_all(repo.join(".beads")).unwrap();
    std::fs::write(repo.join(".beads/beads.jsonl"), lines.join("\n")).unwrap();
}

fn read_export(repo: &Path) -> String {
    std::fs::read_to_string(repo.join(".beads/beads.jsonl")).unwrap()
}

/// Ids in file order — the merge result's *ordering* is what distinguishes a
/// record merge from a text merge.
fn ids_in_order(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .map(|v| v["id"].as_str().unwrap().to_string())
        .collect()
}

/// Fresh repo with `.gitattributes` + a base commit of the export.
/// `with_driver` decides whether the merge driver is configured — the only
/// difference between the real test and its control.
fn setup(with_driver: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git_ok(repo, &["init", "-q", "-b", "main"]);
    git_ok(repo, &["config", "user.email", "test@example.com"]);
    git_ok(repo, &["config", "user.name", "test"]);
    // Never let a global template / user hook interfere.
    git_ok(repo, &["config", "core.hooksPath", "/dev/null"]);
    if with_driver {
        git_ok(
            repo,
            &[
                "config",
                "merge.beads-jsonl.name",
                "rosary bead JSONL export",
            ],
        );
        let driver = format!(
            "\"{}\" bead merge-jsonl \"%O\" \"%A\" \"%B\"",
            rsry().display()
        );
        git_ok(repo, &["config", "merge.beads-jsonl.driver", &driver]);
    }
    std::fs::write(
        repo.join(".gitattributes"),
        ".beads/beads.jsonl merge=beads-jsonl\n",
    )
    .unwrap();

    write_export(repo, &[rec("r-bbb", "base"), rec("r-ddd", "base")]);
    git_ok(repo, &["add", ".gitattributes", ".beads/beads.jsonl"]);
    git_ok(repo, &["commit", "-qm", "base"]);
    dir
}

/// Commit `lines` as the export on a branch forked from `main`.
fn branch_with(repo: &Path, branch: &str, lines: &[String]) {
    git_ok(repo, &["checkout", "-q", "-b", branch, "main"]);
    write_export(repo, lines);
    git_ok(repo, &["add", ".beads/beads.jsonl"]);
    git_ok(repo, &["commit", "-qm", branch]);
}

/// A real `git merge` of two branches that both modified `.beads/beads.jsonl`
/// succeeds (exit 0) and produces the correct UNION — **id-sorted**.
///
/// Each side appends its new bead at the END of the file (an id-unsorted edit,
/// as a hand-edit or an out-of-band writer would produce). A text merge can
/// only ever keep lines where they are; it cannot reorder them. So an
/// id-sorted result is a fingerprint only the record-level driver can leave —
/// see `control_without_driver_text_merge_does_not_sort`.
#[test]
fn git_merge_uses_the_driver_and_produces_a_sorted_union() {
    let dir = setup(true);
    let repo = dir.path();

    branch_with(
        repo,
        "ours",
        &[
            rec("r-bbb", "base"),
            rec("r-ddd", "base"),
            rec("r-aaa", "o"),
        ],
    );
    branch_with(
        repo,
        "theirs",
        &[
            rec("r-bbb", "base"),
            rec("r-ddd", "base"),
            rec("r-ccc", "t"),
        ],
    );

    git_ok(repo, &["checkout", "-q", "ours"]);
    let out = git(repo, &["merge", "--no-edit", "theirs"]);
    assert!(
        out.status.success(),
        "merge should be clean: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let merged = read_export(repo);
    // Union of both sides…
    assert_eq!(
        ids_in_order(&merged),
        ["r-aaa", "r-bbb", "r-ccc", "r-ddd"],
        "merge must union AND re-sort by id: {merged}"
    );
    // …with no marker debris and no duplicated record.
    assert!(!merged.contains("<<<<<<<"), "{merged}");
    assert_eq!(merged.lines().count(), 4, "{merged}");
}

/// Control: the SAME scenario with no driver configured. Git's text merge
/// unions the lines but leaves each appended bead where it was — the result is
/// NOT id-sorted. This is what makes the assertion above proof that the driver
/// ran, rather than proof that git happened to do the right thing.
#[test]
fn control_without_driver_text_merge_does_not_sort() {
    let dir = setup(false);
    let repo = dir.path();

    branch_with(
        repo,
        "ours",
        &[
            rec("r-bbb", "base"),
            rec("r-ddd", "base"),
            rec("r-aaa", "o"),
        ],
    );
    branch_with(
        repo,
        "theirs",
        &[
            rec("r-bbb", "base"),
            rec("r-ddd", "base"),
            rec("r-ccc", "t"),
        ],
    );

    git_ok(repo, &["checkout", "-q", "ours"]);
    let out = git(repo, &["merge", "--no-edit", "theirs"]);
    let merged = read_export(repo);
    let sorted = {
        let mut v = ids_in_order(&merged);
        v.sort();
        v
    };
    // Either git conflicted, or it merged the lines without reordering them.
    // Either way it did NOT produce the driver's id-sorted union.
    assert!(
        !out.status.success() || ids_in_order(&merged) != sorted,
        "text merge unexpectedly produced a sorted union — the distinguishing \
         assertion in the driver test would be vacuous: {merged}"
    );
}

/// Genuine divergence — both branches drove the SAME bead to different states —
/// must fail the merge loudly rather than silently discarding one side's edit.
/// Git must report a conflict, and both records must survive in the worktree.
#[test]
fn git_merge_conflicts_when_both_sides_changed_the_same_bead() {
    let dir = setup(true);
    let repo = dir.path();

    branch_with(repo, "ours", &[rec("r-bbb", "OURS"), rec("r-ddd", "base")]);
    branch_with(
        repo,
        "theirs",
        &[rec("r-bbb", "THEIRS"), rec("r-ddd", "base")],
    );

    git_ok(repo, &["checkout", "-q", "ours"]);
    let out = git(repo, &["merge", "--no-edit", "theirs"]);
    assert!(
        !out.status.success(),
        "a bead changed on both sides must conflict, not silently pick a winner"
    );

    let merged = read_export(repo);
    assert!(merged.contains("<<<<<<< ours"), "{merged}");
    // Neither side's content is discarded.
    assert!(merged.contains("OURS"), "{merged}");
    assert!(merged.contains("THEIRS"), "{merged}");
    // The untouched bead still merged cleanly alongside the conflict.
    assert!(merged.contains("r-ddd"), "{merged}");
}

/// A bead only ONE side changed is an unambiguous edit, not a conflict — the
/// ancestor is what makes the common case quiet.
#[test]
fn git_merge_takes_a_one_sided_edit_cleanly() {
    let dir = setup(true);
    let repo = dir.path();

    // Each side edits a DIFFERENT bead, so each edit is unambiguous.
    branch_with(repo, "ours", &[rec("r-bbb", "base"), rec("r-ddd", "MINE")]);
    branch_with(
        repo,
        "theirs",
        &[rec("r-bbb", "CLOSED"), rec("r-ddd", "base")],
    );

    git_ok(repo, &["checkout", "-q", "ours"]);
    let out = git(repo, &["merge", "--no-edit", "theirs"]);
    assert!(
        out.status.success(),
        "one-sided edits should merge cleanly: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let merged = read_export(repo);
    assert!(merged.contains("CLOSED"), "{merged}");
    assert!(merged.contains("MINE"), "{merged}");
    assert_eq!(merged.lines().count(), 2, "{merged}");
}
