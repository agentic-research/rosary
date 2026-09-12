//! The `.beads/beads.jsonl` dirty-plague journey (rosary-3d455a) from the
//! other surface that caused it: the reconciler. `rsry close-merged --local`
//! is run by the post-merge hook after a `git pull` — on whatever branch the
//! owner happens to be on, including a feature branch — and closes every
//! bead whose squash merge just landed on the remote trunk (rosary-e5fcd2).
//! Same five observations as `beads_dirty_journey.rs`; the store writes are
//! those hook-driven closures, not anything the owner typed.
//!
//! A peer clone plays the rest of the team: it pushes a commit to `feature`
//! (so the owner's pull is a fast-forward) and squash-merge subjects
//! `[<id>] … (#N)` to `main` (what the hook's trunk scan reads).
//!
//! RED by design until rosary-3d455a P1–P4 land (F1 removes the `#[ignore]`).
//! Run with `cargo test --test beads_dirty_journey_reconciler -- --ignored`.

#[path = "common/journey.rs"]
mod reconciler_journey;
use reconciler_journey::Journey;

/// Peer pushes a squash-merge subject for `id` to `main` — the signal
/// `close-merged --local` reads from `refs/remotes/origin/main`.
fn peer_merges_on_main(j: &mut Journey, id: &str, pr: u32) {
    let o = j.peer_git(&["checkout", "-q", "main"]);
    j.must("peer checkout main", &o);
    let subject = format!("[{id}] feat(core): squash-merged (#{pr})");
    let o = j.peer_git(&["commit", "-q", "--allow-empty", "-m", &subject]);
    j.must("peer squash commit", &o);
    let o = j.peer_git(&["push", "-q", "origin", "main"]);
    j.must("peer push main", &o);
}

/// Guard against a vacuous pass: the hook is best-effort (`|| true`, output
/// discarded), so prove the sweep actually closed the bead before observing.
fn assert_closed_by_sweep(j: &mut Journey, id: &str) {
    let out = j.rsry(&["bead", "list", "--status", "done"]);
    let done = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        done.contains(id),
        "post-merge hook did not close {id} via close-merged --local (fixture, not the \
         journey); done beads:\n{done}\ntranscript:\n{}",
        j.transcript.join("\n")
    );
}

#[test]
#[ignore = "rosary-3d455a: RED until P1–P4 land; run with -- --ignored"]
fn a_bead_closed_by_the_post_merge_hook_on_a_feature_branch_does_not_dirty_or_block_the_checkout() {
    let mut j = Journey::new();
    // All three beads exist before the export is tracked: the journey's only
    // store writes are the reconciler's closures.
    let ids = j.seed(&["feature work", "unrelated note", "later work"]);
    let (id, other, third) = (ids[0].clone(), ids[1].clone(), ids[2].clone());
    j.start_feature();

    // The peer: a hook-less clone that advances `feature` by one commit and
    // lands two squash merges on `main`.
    let remote = j.remote.path().to_string_lossy().into_owned();
    let peer = j.peer.to_string_lossy().into_owned();
    let o = j.git(&["clone", "-q", &remote, &peer]);
    j.must("peer clone", &o);
    let o = j.peer_git(&["checkout", "-q", "feature"]);
    j.must("peer checkout feature", &o);
    std::fs::write(j.peer.join("peer.rs"), "fn peer() {}\n").unwrap();
    let o = j.peer_git(&["add", "peer.rs"]);
    j.must("peer add", &o);
    let o = j.peer_git(&[
        "commit",
        "-q",
        "-m",
        "[rosary-000000] feat(peer): peer code",
    ]);
    j.must("peer commit", &o);
    let o = j.peer_git(&["push", "-q", "origin", "feature"]);
    j.must("peer push feature", &o);
    peer_merges_on_main(&mut j, &id, 7);
    peer_merges_on_main(&mut j, &other, 8);

    // (1) the owner, on the feature branch, fast-forward pulls. The post-merge
    //     hook runs `close-merged --local`, which closes `id` and `other`
    //     from the trunk scan — two store writes the owner never typed.
    let o = j.git(&["pull", "-q", "--ff-only"]);
    j.must("fast-forward pull on feature", &o);
    assert_closed_by_sweep(&mut j, &id);
    assert_closed_by_sweep(&mut j, &other);
    j.observe_write_clean();

    // (2) explicit-path commit through the real pre-commit hook. `other`'s
    //     closure is the churn it must not carry.
    j.observe_explicit_commit(&id, &other);

    // (3) a reconciler write AFTER the commit: the peer lands a third squash
    //     merge on main, the owner pulls trunk into the feature branch, the
    //     post-merge hook closes `third`; then push through the real pre-push
    //     hook.
    peer_merges_on_main(&mut j, &third, 9);
    let o = j.git(&["pull", "-q", "--no-rebase", "origin", "main"]);
    j.must("pull main into feature", &o);
    assert_closed_by_sweep(&mut j, &third);
    j.observe_push_feature("close-merged --local: PR #9 merged");

    // (4) switch back to main with no stash; (5) push main is not refused.
    j.observe_checkout_main_and_push();
    j.finish("the post-merge hook (close-merged --local)");
}
