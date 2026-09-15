//! The `.beads/beads.jsonl` dirty-plague journey (rosary-3d455a), driven
//! against the REAL binary with the REAL hooks `rsry init` installs — the
//! CLI surface. The MCP stdio and post-merge reconciler surfaces are the
//! sibling `beads_dirty_journey_*.rs` binaries (rosary-e5fcd2); the harness
//! and the shared observations live in `tests/common/journey.rs`.
//!
//! Every existing test observes one piece of this path in isolation:
//! `src/publish/tests.rs` asserts file content and never looks at `git
//! status`; the `hooks_run_*` unit tests drive the hook logic with a stub
//! binary; `tests/init_jsonl_reconciliation.rs` and `tests/close_jsonl_sync.rs`
//! are single-branch, single-writer, and commit with `--no-verify`. None of
//! them creates a second branch, commits through the pre-commit hook after a
//! store write, pushes through the pre-push hook, or asserts a clean tree.
//!
//! This test is the owner's actual day: write a bead on a feature branch,
//! commit code with explicit paths, push, switch back to main, push main. It
//! collects every violation instead of stopping at the first, so one run
//! reports the whole shape of the defect. It is RED by design until the
//! materialization-timing decision in rosary-3d455a (ADR-0024 amendment)
//! lands; it is fix-agnostic — commit-time materialization, main-only
//! regeneration, or a projection outside the working tree all turn it green.
//!
//! MUTATIONS, as measured (rosary-e5fd35, 2026-09-12), each on the branch
//! that carried the whole fix:
//!
//! - restore write-through in `PublishingBeadStore::publish` → observation 1 RED
//! - commit-time publish renders every store bead, not the named ones
//!   → observation 2 RED
//! - pre-push reads the working-tree file instead of the pushed blob → this
//!   journey STAYS GREEN (a store write no longer touches the tree, so tree
//!   equals blob at push time); the discriminator is
//!   `tests/hooks_prepush_ref_blob.rs::a_published_but_uncommitted_record_does_not_satisfy_the_gate`
//! - post-commit never folds the record in → this journey is vacuously green;
//!   the discriminator is `tests/hooks_commit_scoped_publish.rs` (3 of 3 RED)
//!
//! It ran ignored while the amendment was pending; since the wave landed
//! (#495 #497 #498 #496) it runs in `task check` like any other test.

#[path = "common/journey.rs"]
mod cli_journey;
use cli_journey::{Journey, created_id};

#[test]
fn a_bead_written_on_a_feature_branch_does_not_dirty_or_block_the_checkout() {
    let mut j = Journey::new();
    j.seed(&[]);
    j.start_feature();

    // (1) one store write from the CLI; MCP, the HTTP daemon and the
    //     reconciler all go through the same PublishingBeadStore.
    let o = j.rsry(&[
        "bead",
        "create",
        "feature work",
        "--files",
        "a.rs",
        "--acceptance",
        "cargo test feature_work",
    ]);
    j.must("bead create", &o);
    let id = created_id(&o);
    j.observe_write_clean();

    // A second, unrelated bead: the churn a code commit must NOT carry.
    let o = j.rsry(&[
        "bead",
        "create",
        "unrelated note",
        "--files",
        "z.rs",
        "--acceptance",
        "cargo test unrelated",
    ]);
    j.must("second bead create", &o);
    let other = created_id(&o);

    // (2) explicit-path commit through the real pre-commit hook.
    j.observe_explicit_commit(&id, &other);

    // (3) a bead write AFTER the commit, then push through the real pre-push hook.
    let o = j.rsry(&["bead", "comment", "add", &id, "opened PR #1"]);
    j.must("comment add", &o);
    j.observe_push_feature("opened PR #1");

    // (4) switch back to main with no stash; (5) push main is not refused.
    j.observe_checkout_main_and_push();
    j.finish("the CLI");
}
