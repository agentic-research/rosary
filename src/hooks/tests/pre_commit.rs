//! The pre-commit template: it no longer renders the projection — that moved
//! to commit-msg, scoped to the beads the subject names (rosary-e5bfd3).

use super::*;

/// Proves `hooks run` genuinely renders and executes the REAL embedded
/// template (not a stub) — the opt-in-by-tracking guard short-circuits to a
/// no-op without needing the `rsry` binary resolvable on PATH.
#[test]
fn hooks_run_precommit_is_a_noop_when_jsonl_not_tracked() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_repo(root);
    seed_commit(root);
    run(root, "pre-commit").unwrap();
}

/// With the projection tracked AND a resolvable rsry, pre-commit still writes
/// nothing: the render lives in commit-msg now. `fake_rsry` would overwrite
/// the file on any `-o <path>` export call, so an unchanged file proves the
/// template never asked for one (mutation: restore the export block → RED).
#[test]
fn hooks_run_precommit_never_renders_the_tracked_projection() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_repo(root);
    seed_commit(root);
    std::fs::create_dir_all(root.join(".beads")).unwrap();
    let jsonl = root.join(".beads/beads.jsonl");
    std::fs::write(&jsonl, "{\"id\":\"x-aaaaaa\"}\n").unwrap();
    assert!(git(root, &["add", ".beads/beads.jsonl"]).status.success());
    assert!(git(root, &["commit", "-q", "-m", "track"]).status.success());
    let fake = fake_rsry(root, "{\"id\":\"swept-in\"}\n");
    let out = run_hook_with_rsry(root, "pre-commit", &fake);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(&jsonl).unwrap(),
        "{\"id\":\"x-aaaaaa\"}\n",
        "pre-commit must not touch the projection"
    );
    let staged = git(root, &["diff", "--cached", "--name-only"]);
    assert!(
        String::from_utf8_lossy(&staged.stdout).trim().is_empty(),
        "pre-commit must stage nothing"
    );
}
