//! The post-commit template: folds a projection staged by commit-msg into
//! HEAD, and does nothing otherwise (rosary-e5bfd3). The end-to-end path with
//! the real binary is `tests/hooks_commit_scoped_publish.rs`.

use super::*;

fn tracked_projection_repo(root: &Path) -> PathBuf {
    init_repo(root);
    seed_commit(root);
    std::fs::create_dir_all(root.join(".beads")).unwrap();
    let jsonl = root.join(".beads/beads.jsonl");
    std::fs::write(&jsonl, "{\"id\":\"x-aaaaaa\"}\n").unwrap();
    assert!(git(root, &["add", ".beads/beads.jsonl"]).status.success());
    assert!(git(root, &["commit", "-q", "-m", "track"]).status.success());
    jsonl
}

fn head(root: &Path) -> String {
    String::from_utf8_lossy(&git(root, &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string()
}

#[test]
fn hooks_run_postcommit_is_a_noop_when_nothing_is_staged() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    tracked_projection_repo(root);
    let before = head(root);
    run(root, "post-commit").unwrap();
    assert_eq!(head(root), before, "nothing staged → HEAD untouched");
}

/// A staged projection is folded into HEAD and ONLY it: another staged path
/// (the `git commit <paths>` leftover) stays staged and out of the commit.
#[test]
fn hooks_run_postcommit_folds_only_the_staged_projection_into_head() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let jsonl = tracked_projection_repo(root);
    let before = head(root);
    std::fs::write(&jsonl, "{\"id\":\"x-aaaaaa\"}\n{\"id\":\"x-bbbbbb\"}\n").unwrap();
    std::fs::write(root.join("other"), "staged but not committed").unwrap();
    assert!(
        git(root, &["add", ".beads/beads.jsonl", "other"])
            .status
            .success()
    );

    run(root, "post-commit").unwrap();

    assert_ne!(head(root), before, "HEAD must be amended");
    let files = git(root, &["show", "--name-only", "--format=", "HEAD"]);
    let files = String::from_utf8_lossy(&files.stdout);
    assert!(files.contains(".beads/beads.jsonl"), "{files}");
    assert!(
        !files.contains("other"),
        "other staged paths must not be swept in: {files}"
    );
    let blob = git(root, &["show", "HEAD:.beads/beads.jsonl"]);
    assert!(String::from_utf8_lossy(&blob.stdout).contains("x-bbbbbb"));
    let staged = git(root, &["diff", "--cached", "--name-only"]);
    assert_eq!(String::from_utf8_lossy(&staged.stdout).trim(), "other");
    // Re-entrant: a second run sees the projection equal to HEAD and stops.
    let amended = head(root);
    run(root, "post-commit").unwrap();
    assert_eq!(head(root), amended);
}
