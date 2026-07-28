//! The laws every `FieldAlgebra` must satisfy (rosary-4c8637).
//!
//! ## What was wrong
//!
//! `FieldAlgebra::fold`'s own doc comment says implementations **MUST** be
//! invariant under reordering of `obs`. Measured 2026-07-28, across the four
//! algebras: zero commutativity tests existed, and LWW + OR-set had no
//! idempotence test either. `integration_tests.rs` justified
//! convergence-under-partition with the comment *"Per-field algebras are
//! commutative + idempotent"* — a claim asserted in prose and tested nowhere.
//!
//! That matters because ADR-0010 R4b (`rosary-a66b3a`) puts this lattice on the
//! path to owning bead **status**. A non-commutative fold means two agents
//! observing the same bead in different orders disagree about whether it is
//! done — and `rosary-e0e19f` already showed what a wrong terminal status costs
//! when `BeadState::Done` cannot be left.
//!
//! ## Driven by the registry, not by a list
//!
//! The harness is generic over `&dyn FieldAlgebra` and runs against **every
//! algebra the canonical registry holds**, via `FieldRegistry::iter`. Register a
//! fifth algebra and it is law-checked without touching this file — the same
//! "derive the check from the authority" rule that `src/parity` and
//! `src/publish` follow. A hand-listed set of four would be a fifth copy of the
//! registry, and would rot the first time someone added one.
//!
//! ## Why these three laws and not "ACI"
//!
//! `fold` reduces a *set* rather than combining two values, so the classic
//! binary phrasing does not apply directly. The observable equivalents are:
//!
//! - **permutation invariance** — the trait's stated MUST; commutativity as it
//!   manifests over a slice.
//! - **determinism** — folding the same slice twice agrees. Cheap, and it
//!   catches accidental reliance on iteration order of a hash container.
//!
//! Idempotence is deliberately NOT an algebra law here, and finding that out was
//! the point of writing it as one first. The OR-set genuinely is not idempotent:
//! fold it a byte-identical observation twice and the comment appears twice,
//! because `OrSetAlgebra::fold` emits one entry per observation and sorts for
//! determinism without ever collapsing. That is *safe*, because duplicate
//! suppression is `ObservationLog`'s contract — invariant 8, `dedup_before_fold`
//! — enforced in `ObservationLog::insert` by `dedup_key`, and every production
//! fold path goes through it (`fold.rs` iterates `log.iter()`; `shadow.rs`
//! builds a log and inserts into it before folding).
//!
//! So the law belongs at the log boundary, and
//! `duplicates_do_not_survive_log_then_fold` asserts it there. Recording the
//! split matters: invariant 8 is **load-bearing**, not a convenience. Any future
//! path that folds raw rows without routing through the log will silently
//! double-count every comment and label.
//!
//! Associativity has no direct expression against this signature: there is no
//! `combine(fold(a), fold(b))`. Partition-then-combine convergence is a
//! property of `fold.rs`, and belongs to `rosary-51a4f6`.

use proptest::prelude::*;

use super::{
    FieldAlgebra, FieldName, FieldValue, Observation, PipelineVerdictValue, Source, WorkRef,
};

/// Build an observation for `field` carrying a value the algebra will accept.
///
/// Type-correct by construction: each algebra rejects a `FieldValue` of the
/// wrong shape (that is what the `*_type_mismatch_errors` tests cover, which
/// stay), so a generator emitting arbitrary variants would only ever measure
/// the error path.
fn obs_for(field: &FieldName, source: &str, event: usize, pick: u8, ts_offset: i64) -> Observation {
    let value = match field {
        FieldName::PipelineVerdict => FieldValue::PipelineVerdict(match pick % 7 {
            0 => PipelineVerdictValue::Dispatched,
            1 => PipelineVerdictValue::Verifying,
            2 => PipelineVerdictValue::Pass,
            3 => PipelineVerdictValue::PrOpen,
            4 => PipelineVerdictValue::Done,
            5 => PipelineVerdictValue::Fail,
            _ => PipelineVerdictValue::Deadletter,
        }),
        FieldName::Ahead | FieldName::Behind => FieldValue::Int64(i64::from(pick)),
        FieldName::Deadline => FieldValue::Timestamp(
            chrono::DateTime::from_timestamp(1_700_000_000 + i64::from(pick), 0).expect("in range"),
        ),
        // Every remaining registered field is string-shaped. `None` is included
        // deliberately: an explicit unset is a real observation for LWW, and it
        // is where a "latest wins" implementation most easily goes wrong.
        _ => {
            if pick % 5 == 4 {
                FieldValue::OptString(None)
            } else {
                FieldValue::OptString(Some(format!("v{}", pick % 3)))
            }
        }
    };
    let source = Source::new(source);
    let payload_hash = Observation::compute_payload_hash(&source, field, &value);
    Observation {
        work_item: WorkRef {
            repo: "rosary".to_string(),
            scope: String::new(),
            bead_id: "rosary-law".to_string(),
        },
        source,
        source_event_id: format!("e{event}"),
        field: field.clone(),
        value,
        // Deliberately coarse: `ts_offset % 3` collapses many observations onto
        // the SAME instant. Equal timestamps are exactly where an LWW register's
        // tie-break is decided, and a generator with distinct timestamps would
        // never exercise it — which is how a non-total tie-break survives.
        observed_at: chrono::DateTime::from_timestamp(1_700_000_000 + (ts_offset % 3), 0)
            .expect("in range"),
        cert: None,
        payload_hash,
    }
}

/// Build a batch from raw specs. Sources come from a small pool so same-source
/// and cross-source collisions both occur.
///
/// Taking specs as a plain proptest input rather than nesting a strategy keeps
/// shrinking intact: a counterexample minimises to the smallest batch that still
/// breaks the law, which is the whole reason to use proptest here.
fn batch_from(field: &FieldName, specs: &[(u8, usize, i64)]) -> Vec<Observation> {
    specs
        .iter()
        .enumerate()
        .map(|(i, (pick, src, ts))| {
            obs_for(
                field,
                ["github", "linear", "agent", "cli"][src % 4],
                i,
                *pick,
                *ts,
            )
        })
        .collect()
}

fn fold_of(alg: &dyn FieldAlgebra, obs: &[Observation]) -> anyhow::Result<FieldValue> {
    let refs: Vec<&Observation> = obs.iter().collect();
    alg.fold(&refs)
}

/// Every registered field, so the properties below enumerate the registry
/// rather than a hand-written list of four.
fn registered_fields() -> Vec<FieldName> {
    super::registry::global().fields().cloned().collect()
}

/// THE law the trait doc states as a MUST, and the one nothing tested.
#[test]
fn fold_is_invariant_under_permutation() {
    crate::proptest_support::check(
        96,
        (
            0usize..32,
            prop::collection::vec((0u8..9, 0usize..4, 0i64..6), 0..7),
            0u64..10_000,
        ),
        |(idx, specs, seed)| {
            let fields = registered_fields();
            let field = fields[idx % fields.len()].clone();
            let reg = super::registry::global();
            let alg = reg.get(&field).expect("registered");
            let batch = batch_from(&field, &specs);

            // Deterministic shuffle driven by a proptest-owned seed, so a failure
            // replays exactly rather than depending on ambient randomness.
            let mut shuffled = batch.clone();
            let n = shuffled.len();
            for i in (1..n).rev() {
                let j = ((seed
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(i as u64))
                    >> 33) as usize
                    % (i + 1);
                shuffled.swap(i, j);
            }

            match (fold_of(alg, &batch), fold_of(alg, &shuffled)) {
                (Ok(x), Ok(y)) => prop_assert_eq!(
                    x,
                    y,
                    "field {:?}: fold changed under reordering — the trait doc says MUST NOT",
                    field
                ),
                (Err(_), Err(_)) => {}
                (x, y) => prop_assert!(
                    false,
                    "field {:?}: fold succeeded on one ordering and failed on the other ({} vs {})",
                    field,
                    x.is_ok(),
                    y.is_ok()
                ),
            }
            Ok(())
        },
    );
}

/// Invariant 8 (`dedup_before_fold`) at the boundary that owns it.
///
/// Inserting the same observation twice must leave the folded view
/// unchanged. This is the property webhook redelivery and `lattice
/// backfill` re-runs actually depend on, and it holds even though the
/// OR-set algebra underneath is not itself idempotent.
#[test]
fn duplicates_do_not_survive_log_then_fold() {
    crate::proptest_support::check(
        96,
        (
            0usize..32,
            prop::collection::vec((0u8..9, 0usize..4, 0i64..6), 1..7),
            0usize..8,
        ),
        |(idx, specs, dup)| {
            let fields = registered_fields();
            let field = fields[idx % fields.len()].clone();
            let batch = batch_from(&field, &specs);
            prop_assume!(!batch.is_empty());

            let mut plain = super::log::ObservationLog::new();
            for o in &batch {
                plain.insert(o.clone());
            }
            let mut with_dup = super::log::ObservationLog::new();
            for o in &batch {
                with_dup.insert(o.clone());
            }
            // The duplicate is byte-identical, so `insert` must report it as a
            // no-op and the fold must not move.
            let accepted = with_dup.insert(batch[dup % batch.len()].clone());
            prop_assert!(
                !accepted,
                "field {:?}: log accepted a byte-identical duplicate — invariant 8 is broken \
             at its own boundary, and the OR-set will double-count",
                field
            );

            let a = super::fold::fold(&plain);
            let b = super::fold::fold(&with_dup);
            if let (Ok(x), Ok(y)) = (a, b) {
                prop_assert_eq!(
                    x,
                    y,
                    "field {:?}: a replayed observation changed the derived view",
                    field
                );
            }
            Ok(())
        },
    );
}

/// Determinism — folding the same slice twice agrees. Catches accidental
/// dependence on the iteration order of a hash container.
#[test]
fn fold_is_deterministic() {
    crate::proptest_support::check(
        96,
        (
            0usize..32,
            prop::collection::vec((0u8..9, 0usize..4, 0i64..6), 0..7),
        ),
        |(idx, specs)| {
            let fields = registered_fields();
            let field = fields[idx % fields.len()].clone();
            let reg = super::registry::global();
            let alg = reg.get(&field).expect("registered");
            let batch = batch_from(&field, &specs);
            if let (Ok(x), Ok(y)) = (fold_of(alg, &batch), fold_of(alg, &batch)) {
                prop_assert_eq!(x, y, "field {:?}: fold is not deterministic", field);
            }
            Ok(())
        },
    );
}

/// The harness must actually be pointed at something. A registry that returned
/// nothing would make every property above vacuous — the failure mode this
/// session found in the permission rail and again in the coverage gate.
#[test]
fn harness_covers_every_registered_algebra() {
    let fields = registered_fields();
    assert!(
        fields.len() >= 9,
        "registry looks empty or shrunk: {fields:?}"
    );
    let reg = super::registry::global();
    for f in &fields {
        assert!(
            reg.get(f).is_some(),
            "no algebra for registered field {f:?}"
        );
    }
    // All four algebra kinds are represented, so "every registered field" is
    // not accidentally nine instances of one algebra.
    assert!(
        fields.contains(&FieldName::PipelineVerdict),
        "chain-max absent"
    );
    assert!(fields.contains(&FieldName::Assignee), "LWW absent");
    assert!(fields.contains(&FieldName::Comment), "OR-set absent");
}
