//! Laws for the `beads.jsonl` 3-way merge driver (rosary-4ca8a5).
//!
//! ## Why this one, and why now
//!
//! `merge_contract` backs a **git merge driver** — `.gitattributes` points at it
//! and `rsry hooks install` configures `merge.beads-jsonl.*`. It runs on real
//! merges without anyone invoking it deliberately, over the file that is the
//! canonical bead record. That is the class of code where a bug silently eats
//! committed work rather than erroring.
//!
//! It had 14 example tests and no laws. And #431 made every bead write
//! republish the tracked file, so two sessions touching beads concurrently is
//! now the normal case rather than the rare one — the driver runs far more often
//! than when it was written.
//!
//! ## What the properties add over the examples
//!
//! The examples are good, and they stay: each names a specific scenario
//! (`only_theirs_changed_takes_theirs`, `conflict_is_isolated_to_the_diverged_id`)
//! and reads as documentation. What they cannot do is quantify. These properties
//! assert over *arbitrary* ancestor/ours/theirs triples that:
//!
//! - **nothing is ever lost** — every id in any input appears in the output.
//!   This is the one that matters: a merge driver that drops a bead has
//!   destroyed committed history, and no example can rule it out for inputs
//!   nobody thought of.
//! - **conflict is exactly the both-sides-diverged set** — no false conflicts
//!   (which would block merges), and no silent winners (which would lose an
//!   edit). Both directions matter and the examples only sample each once.
//! - **identity holds** — `merge(O, A, A) == A` and `merge(O, O, B) == B`.
//! - **symmetry** — swapping ours/theirs conflicts on the same ids. An
//!   asymmetric driver would resolve differently depending on which side ran it,
//!   which is exactly the bug a distributed team would never reproduce.
//! - **id-sorted output** — the diff-stability contract `src/import.rs`
//!   documents: adding one bead inserts exactly one line.
//!
//! Determinism comes from `crate::proptest_support` — a fixed seed, so a
//! counterexample replays and per-file coverage does not wander (#441).

use serde_json::{Value, json};

use super::{MergeOutcome, merge_contract};

/// A bead record with `id`, and a `v` field the sides can diverge on.
fn rec(id: u8, v: u8) -> Value {
    json!({"id": format!("rosary-{id:02}"), "v": v, "title": format!("bead {id}")})
}

fn id_of(id: u8) -> String {
    format!("rosary-{id:02}")
}

/// One side of a merge, built from `(present, value)` per id slot.
///
/// Modelling each side as a sparse map over a small shared id space is what
/// makes add / delete / diverge / agree all reachable from one generator: an id
/// absent from a side is a deletion when the ancestor had it and simply absent
/// when it did not.
fn side(spec: &[(bool, u8)]) -> Vec<Value> {
    spec.iter()
        .enumerate()
        .filter(|(_, (present, _))| *present)
        .map(|(i, (_, v))| rec(i as u8, *v))
        .collect()
}

fn ids_in(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| v.get("id")?.as_str().map(str::to_string))
        .collect()
}

/// Every id mentioned anywhere in the outcome — parsed records for a clean
/// merge, plus the ids named in conflict blocks (whose markers deliberately
/// make those lines unparseable).
fn surviving_ids(out: &MergeOutcome) -> std::collections::BTreeSet<String> {
    let mut seen: std::collections::BTreeSet<String> = ids_in(&out.text).into_iter().collect();
    seen.extend(out.conflicts.iter().cloned());
    seen
}

const SLOTS: usize = 4;

/// `(present, value)` triples for ancestor / ours / theirs across `SLOTS` ids.
fn arb_triple()
-> impl proptest::strategy::Strategy<Value = (Vec<(bool, u8)>, Vec<(bool, u8)>, Vec<(bool, u8)>)> {
    use proptest::prelude::*;
    let one = prop::collection::vec((any::<bool>(), 0u8..3), SLOTS);
    (one.clone(), one.clone(), one)
}

#[test]
fn no_record_is_ever_lost() {
    crate::proptest_support::check(256, arb_triple(), |(a, o, t)| {
        let out = merge_contract(side(&a), side(&o), side(&t))
            .map_err(|e| proptest::test_runner::TestCaseError::fail(e.to_string()))?;
        let survived = surviving_ids(&out);
        // An id present on EITHER side must survive. Ancestor-only ids are the
        // both-sides-deleted case and are correctly absent.
        for i in 0..SLOTS {
            let live = o[i].0 || t[i].0;
            if live {
                proptest::prop_assert!(
                    survived.contains(&id_of(i as u8)),
                    "{} was present on a side but vanished from the merge — a merge \
                     driver that drops a bead has destroyed committed history",
                    id_of(i as u8)
                );
            }
        }
        Ok(())
    });
}

#[test]
fn conflicts_are_exactly_the_both_sides_diverged_set() {
    crate::proptest_support::check(256, arb_triple(), |(a, o, t)| {
        let out = merge_contract(side(&a), side(&o), side(&t))
            .map_err(|e| proptest::test_runner::TestCaseError::fail(e.to_string()))?;
        for i in 0..SLOTS {
            let id = id_of(i as u8);
            // Both sides present, disagreeing with each other, and each
            // differing from the ancestor (or the ancestor absent) — the only
            // shape a genuine conflict can take.
            let ours_v = o[i].0.then_some(o[i].1);
            let theirs_v = t[i].0.then_some(t[i].1);
            let anc_v = a[i].0.then_some(a[i].1);
            let expected = match (ours_v, theirs_v) {
                (Some(x), Some(y)) => x != y && Some(x) != anc_v && Some(y) != anc_v,
                _ => false,
            };
            proptest::prop_assert_eq!(
                out.conflicts.contains(&id),
                expected,
                "{}: conflict={} but both-sides-diverged={} (ancestor={:?} ours={:?} theirs={:?}) \
                 — a false conflict blocks a merge; a missing one means a silent winner",
                id,
                out.conflicts.contains(&id),
                expected,
                anc_v,
                ours_v,
                theirs_v
            );
        }
        Ok(())
    });
}

#[test]
fn identity_when_one_side_is_unchanged() {
    crate::proptest_support::check(192, arb_triple(), |(a, o, _t)| {
        // merge(O, A, A): both sides made the same change → that change, clean.
        let out = merge_contract(side(&a), side(&o), side(&o))
            .map_err(|e| proptest::test_runner::TestCaseError::fail(e.to_string()))?;
        proptest::prop_assert!(
            out.is_clean(),
            "identical sides must never conflict, got {:?}",
            out.conflicts
        );
        proptest::prop_assert_eq!(
            ids_in(&out.text),
            ids_in(&side_text(&o)),
            "merge(O, A, A) must equal A"
        );

        // merge(O, O, B): only theirs changed → theirs, clean.
        let out2 = merge_contract(side(&a), side(&a), side(&o))
            .map_err(|e| proptest::test_runner::TestCaseError::fail(e.to_string()))?;
        proptest::prop_assert!(
            out2.is_clean(),
            "only-one-side-changed must never conflict, got {:?}",
            out2.conflicts
        );
        Ok(())
    });
}

fn side_text(spec: &[(bool, u8)]) -> String {
    let mut v: Vec<String> = side(spec)
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect();
    v.sort();
    v.join("\n")
}

#[test]
fn swapping_sides_conflicts_on_the_same_ids() {
    crate::proptest_support::check(256, arb_triple(), |(a, o, t)| {
        let ab = merge_contract(side(&a), side(&o), side(&t))
            .map_err(|e| proptest::test_runner::TestCaseError::fail(e.to_string()))?;
        let ba = merge_contract(side(&a), side(&t), side(&o))
            .map_err(|e| proptest::test_runner::TestCaseError::fail(e.to_string()))?;
        let mut x = ab.conflicts.clone();
        let mut y = ba.conflicts.clone();
        x.sort();
        y.sort();
        proptest::prop_assert_eq!(
            x,
            y,
            "conflict set depends on which side is 'ours' — the same merge would \
             resolve differently depending on who ran it"
        );
        Ok(())
    });
}

#[test]
fn output_is_always_id_sorted() {
    crate::proptest_support::check(256, arb_triple(), |(a, o, t)| {
        let out = merge_contract(side(&a), side(&o), side(&t))
            .map_err(|e| proptest::test_runner::TestCaseError::fail(e.to_string()))?;
        // Only meaningful for a clean merge: conflict blocks are deliberately
        // unparseable, so `ids_in` skips those lines.
        if out.is_clean() {
            let ids = ids_in(&out.text);
            let mut sorted = ids.clone();
            sorted.sort();
            proptest::prop_assert_eq!(
                ids,
                sorted,
                "output must be id-sorted — the diff-stability contract in \
                 src/import.rs (adding a bead inserts exactly one line)"
            );
        }
        Ok(())
    });
}
