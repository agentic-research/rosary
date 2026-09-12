//! The pre-push template (ref-blob comparison lands here: rosary-e5c037).

use super::*;

/// Same opt-in-by-tracking guard on the new pre-push drift gate
/// (rosary-9c0e6c) — proven the same no-binary-needed way as
/// pre-commit's equivalent test above.
#[test]
fn hooks_run_prepush_is_a_noop_when_jsonl_not_tracked() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_repo(root);
    seed_commit(root);
    run(root, "pre-push").unwrap();
}

/// Same worktree guard as pre-commit's rosary-599778 fix, on the new
/// pre-push hook: a linked worktree's on-demand-created empty store
/// must not be compared against the tracked export (which would
/// report every real bead as "missing" and block an unrelated push).
#[test]
fn hooks_run_prepush_is_a_noop_in_a_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("main");
    std::fs::create_dir_all(&main).unwrap();
    init_repo(&main);
    seed_commit(&main);
    std::fs::create_dir_all(main.join(".beads")).unwrap();
    std::fs::write(main.join(".beads/beads.jsonl"), "").unwrap();
    git(&main, &["add", ".beads/beads.jsonl"]);
    git(&main, &["commit", "-q", "-m", "track beads.jsonl"]);

    let wt = tmp.path().join("wt");
    assert!(
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "wt-prepush",
                wt.to_str().unwrap(),
            ],
        )
        .status
        .success()
    );

    run(&wt, "pre-push").unwrap();
}

/// Set up a repo whose tracked export is `tracked`, then run pre-push
/// against a store that would export `live`.
fn prepush_against(tracked: &str, live: &str) -> (tempfile::TempDir, std::process::Output) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    init_repo(&root);
    seed_commit(&root);
    std::fs::create_dir_all(root.join(".beads")).unwrap();
    std::fs::write(root.join(".beads/beads.jsonl"), tracked).unwrap();
    git(&root, &["add", ".beads/beads.jsonl"]);
    git(&root, &["commit", "-q", "-m", "track beads.jsonl"]);

    let rsry = fake_rsry(&root, live);
    let out = run_hook_with_rsry(&root, "pre-push", &rsry);
    (tmp, out)
}

/// The gate's whole purpose, pinned. Every other pre-push test covers
/// a path that deliberately skips the comparison, so without this the
/// check could stop refusing anything and stay green.
#[test]
fn hooks_run_prepush_refuses_the_push_when_the_export_disagrees() {
    let (_tmp, out) = prepush_against("published\n", "published\nstore-only\n");

    assert!(
        !out.status.success(),
        "drift must refuse the push, got {:?}",
        out.status
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("beads.jsonl is stale"),
        "refusal must name the problem, got: {stderr}"
    );
}

/// The other direction: an agreeing store must let the push through.
/// Pairs with the refusal test — together they prove the exit status
/// tracks the comparison rather than being constant.
#[test]
fn hooks_run_prepush_allows_the_push_when_the_export_agrees() {
    let (_tmp, out) = prepush_against("published\n", "published\n");

    assert!(
        out.status.success(),
        "an in-sync store must not block the push, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
