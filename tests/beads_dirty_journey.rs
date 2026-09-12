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
//! Mutation: re-enable write-through in `PublishingBeadStore::publish`, or
//! point pre-push back at the working-tree file, and observations 1/3/4/5 go
//! red again; drop `--published-from` from pre-commit and observation 2 does.
//!
//! It is `#[ignore]`d so `task check` on main stays green while the decision
//! is pending. Run it with `cargo test --test beads_dirty_journey -- --ignored`;
//! that invocation is RED today and is the bead's close condition. The fix
//! removes the `#[ignore]` — a fix that leaves it in place has not landed.

#[path = "common/journey.rs"]
mod cli_journey;
use cli_journey::{Journey, created_id};

#[test]
#[ignore = "rosary-3d455a: RED by design until the ADR-0024 materialization amendment lands; run with -- --ignored"]
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
