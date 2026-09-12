//! The pre-commit template (commit-scoped publish moves it to commit-msg: rosary-e5bfd3).

use super::*;

/// Proves `hooks run` genuinely renders and executes the REAL
/// embedded template (not a stub) — exercised via the pre-commit
/// hook's own opt-in-by-tracking guard, which short-circuits to a
/// no-op without needing the `rsry` binary resolvable on PATH (this
/// test process's `cargo test` sandbox does not guarantee that).
#[test]
fn hooks_run_precommit_is_a_noop_when_jsonl_not_tracked() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_repo(root);
    seed_commit(root);
    // No .beads/beads.jsonl tracked — the embedded script's own
    // opt-in guard must make this a clean no-op.
    run(root, "pre-commit").unwrap();
}
