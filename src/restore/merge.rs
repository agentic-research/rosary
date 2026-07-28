//! Semantic 3-way merge for the git-tracked `.beads/beads.jsonl` export
//! (rosary-f9516f) — the driver behind `merge=beads-jsonl` in `.gitattributes`.
//!
//! `.beads/beads.jsonl` is a *derived, structured* file: one JSON object per
//! line, id-sorted (see [`crate::import::export_beads_contract_jsonl`]). Git's
//! default text merge happens to auto-merge it most of the time — the id sort
//! keeps unrelated beads on distant lines — but a line-level 3-way merge has no
//! idea what a line *means*. It can emit syntactically valid JSONL that is
//! semantically wrong: the same bead id appearing twice (two sides edited it
//! and the hunks didn't overlap textually), or a stale record winning because
//! it happened to be the side git kept. Merging by *record*, not by line, is
//! the only way to hold the file's real invariants: one line per bead id, in id
//! order, each id's record chosen by an actual three-way comparison.
//!
//! ## No silent discards
//!
//! An earlier draft of this driver resolved same-id divergence with
//! last-writer-wins on `updated_at`. That is **wrong here** and was removed:
//! LWW silently destroys one side's real edit, with no record that it ever
//! existed. In a work-tracking store that is data loss wearing a policy hat —
//! the same silent-success failure class this subsystem keeps hitting. The
//! driver now picks a winner only where the 3-way comparison makes the answer
//! *unambiguous*, and is LOUD everywhere else. See [`merge_contract`].
//!
//! ## TRANSITIONAL scaffolding
//!
//! This driver exists because the export is a single flattened projection of
//! the store, so a branch-local bead transition and a global one are
//! indistinguishable inside it — which is what forces a merge decision at all.
//! The intended end state is **dual state** (feature/branch-local bead state
//! kept distinct from the canonical global store) with per-source observations
//! retained and truth *derived* by fold — the machinery `src/observation/`
//! already models (ADR-0010, stuck at R4b / rosary-a66b3a), alongside ADR-0004's
//! dual state machine and ADR-0015's capsules. With dual state plus op-log
//! semantics there is nothing to merge and this driver becomes unnecessary.
//! Tracked as **rosary-610ad8**.

use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Index a batch of contract records by bead id, failing loud on a record with
/// no usable `id`. A silently-dropped bead is exactly the failure mode this
/// subsystem keeps hitting, so an unusable record is an error, never a skip.
/// A duplicate id *within* one side is also an error: that means the file we
/// were handed is already corrupt (most likely by a previous line-wise merge —
/// the very thing this driver exists to prevent), and merging it would launder
/// the corruption into the result.
fn index_by_id(beads: Vec<Value>, side: &str) -> Result<BTreeMap<String, Value>> {
    let mut out = BTreeMap::new();
    for (i, bead) in beads.into_iter().enumerate() {
        let id = bead
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("{side}: record {} has no usable `id` field", i + 1))?
            .to_string();
        if out.insert(id.clone(), bead).is_some() {
            anyhow::bail!("{side}: duplicate bead id `{id}` — input is already corrupt");
        }
    }
    Ok(out)
}

/// Standard git conflict marker size. The driver could take `%L` to honour a
/// caller-configured `conflict-marker-size`, but 7 is what every tool assumes
/// and the merged file is JSONL — a bead record can't contain a leading run of
/// `<` at column 0, so there is nothing for a longer marker to disambiguate.
const MARKER: usize = 7;

/// Outcome of a 3-way JSONL merge: the text to write plus the counts that make
/// the driver's decision auditable on stderr.
#[derive(Debug)]
pub struct MergeOutcome {
    /// The file content to write over `%A`. Id-sorted JSONL, no trailing
    /// newline (matching `bead export --jsonl`, so the next pre-commit export
    /// stages nothing). Contains conflict marker blocks iff `conflicts` is
    /// non-empty.
    pub text: String,
    /// Ids taken from theirs because only THEIR side changed them.
    pub theirs_changed: usize,
    /// Ids taken from ours because only OUR side changed them.
    pub ours_changed: usize,
    /// Ids added on exactly one side (not in the ancestor).
    pub added: usize,
    /// Ids that existed in the ancestor and on exactly one side — i.e. one side
    /// deleted them. See the resurrection policy in [`merge_contract`].
    pub resurrected: usize,
    /// Ids BOTH sides changed, to different values — genuine conflicts. The
    /// driver must exit non-zero when this is non-empty.
    pub conflicts: Vec<String>,
}

impl MergeOutcome {
    /// Did the merge resolve cleanly? Non-zero exit iff this is false.
    pub fn is_clean(&self) -> bool {
        self.conflicts.is_empty()
    }
}

/// Render one conflicting id as a conventional git conflict block.
///
/// Conflict markers make the file unparseable as JSONL — deliberately. A
/// half-merged bead export must never be mistaken for a valid one: a later
/// `merge-jsonl` over this file fails loud in [`index_by_id`], and the
/// pre-commit export regenerates the file from the store once a human has
/// decided. Both sides' full records are preserved verbatim inside the block,
/// so nothing is discarded and either side can be recovered by deleting lines.
fn conflict_block(ours: &str, theirs: &str) -> String {
    let (lt, eq, gt) = ("<".repeat(MARKER), "=".repeat(MARKER), ">".repeat(MARKER));
    format!("{lt} ours\n{ours}\n{eq}\n{theirs}\n{gt} theirs")
}

/// 3-way merge of the bead contract JSONL. `ours` (`%A`) is what the caller
/// overwrites with the result.
///
/// Per id, the standard three-way decision — a winner is chosen only where the
/// answer is *unambiguous*, and there are no silent discards:
///
/// | ancestor | ours | theirs | result |
/// | --- | --- | --- | --- |
/// | any | X | X | X (both sides agree — clean) |
/// | any | X | absent | X (present on one side only — clean; see deletions) |
/// | A | A | B | **B** — only theirs changed it |
/// | A | B | A | **B** — only ours changed it |
/// | A | B | C | **CONFLICT** — both changed it, differently |
/// | absent | B | C | **CONFLICT** — both added the same id, differently |
///
/// Using the ancestor is what keeps this quiet: a bead only one branch touched
/// merges cleanly, which is the overwhelmingly common case (each branch files
/// or closes its own beads). A conflict means two branches really did drive the
/// same bead to different states — and then the driver refuses to guess,
/// emitting BOTH records inside a conflict block and reporting failure so git
/// leaves the resolution to a human. A loud conflict beats a silent discard.
///
/// **Deletion policy — a bead present on exactly one side is KEPT (resurrected
/// if the ancestor had it).** This is a deliberate choice, not an oversight: in
/// this system beads are *closed*, not deleted — there is no delete primitive
/// on the CLI surface, and the export is regenerated from the store on every
/// commit, so a bead's absence on one side is far more likely to mean "that
/// branch's store simply predates it / never had it" (a stale or partially
/// restored `.beads/beads.db`) than "a human deliberately destroyed it". Losing
/// a real bead is unrecoverable from the export; resurrecting a genuinely
/// deleted one is a one-line follow-up commit. Resurrections are counted and
/// reported so the choice is visible rather than silent.
pub fn merge_contract(
    ancestor: Vec<Value>,
    ours: Vec<Value>,
    theirs: Vec<Value>,
) -> Result<MergeOutcome> {
    let ancestor = index_by_id(ancestor, "ancestor")?;
    let ours = index_by_id(ours, "ours")?;
    let theirs = index_by_id(theirs, "theirs")?;

    let mut out = MergeOutcome {
        text: String::new(),
        theirs_changed: 0,
        ours_changed: 0,
        added: 0,
        resurrected: 0,
        conflicts: Vec::new(),
    };

    // BTreeSet over the union of ids ⇒ the output is id-sorted by construction,
    // which is the export's diff-stability invariant (see
    // export_beads_contract_jsonl).
    let all: BTreeSet<&String> = ours.keys().chain(theirs.keys()).collect();
    let mut blocks: Vec<String> = Vec::with_capacity(all.len());

    for id in all {
        let base = ancestor.get(id);
        let block = match (ours.get(id), theirs.get(id)) {
            (Some(o), Some(t)) if o == t => serde_json::to_string(o)?,
            (Some(o), Some(t)) => {
                // Both sides have it and they differ. Ask the ancestor which
                // side actually changed it.
                match base {
                    Some(a) if a == o => {
                        out.theirs_changed += 1;
                        serde_json::to_string(t)?
                    }
                    Some(a) if a == t => {
                        out.ours_changed += 1;
                        serde_json::to_string(o)?
                    }
                    _ => {
                        // Both changed it (or both added it), differently.
                        // Refuse to pick — keep both, fail the merge.
                        out.conflicts.push(id.clone());
                        conflict_block(&serde_json::to_string(o)?, &serde_json::to_string(t)?)
                    }
                }
            }
            (Some(o), None) => {
                if base.is_some() {
                    out.resurrected += 1;
                } else {
                    out.added += 1;
                }
                serde_json::to_string(o)?
            }
            (None, Some(t)) => {
                if base.is_some() {
                    out.resurrected += 1;
                } else {
                    out.added += 1;
                }
                serde_json::to_string(t)?
            }
            // `all` is built from the two side maps, so at least one is Some.
            (None, None) => unreachable!("id came from ours or theirs"),
        };
        blocks.push(block);
    }

    out.text = blocks.join("\n");
    Ok(out)
}

/// The `merge.beads-jsonl.driver` entry point: read the three versions git
/// handed us, merge by record, and overwrite `ours` (`%A`) with the result —
/// "The merge driver is expected to leave the result of the merge in the file
/// named with %A by overwriting it" (gitattributes(5)).
///
/// `%A` is written in BOTH outcomes: on a clean merge it is the merged JSONL,
/// on a conflicted one it carries conflict blocks for the diverged ids (and the
/// caller must exit non-zero — see [`MergeOutcome::is_clean`]).
///
/// Returns `Err` (→ exit 1, reported as a conflict by git) if any input is
/// unparseable, leaving `%A` untouched. That is deliberately loud: better git
/// stops than that it commits a file with a bead silently missing from it.
pub fn merge_jsonl_files(
    ancestor: &std::path::Path,
    ours: &std::path::Path,
    theirs: &std::path::Path,
) -> Result<MergeOutcome> {
    let read = |p: &std::path::Path, side: &str| -> Result<Vec<Value>> {
        super::read_beads_jsonl(Some(p.to_string_lossy().into_owned()))
            .with_context(|| format!("reading {side} version ({})", p.display()))
    };
    let outcome = merge_contract(
        read(ancestor, "ancestor")?,
        read(ours, "ours")?,
        read(theirs, "theirs")?,
    )?;
    std::fs::write(ours, &outcome.text)
        .with_context(|| format!("writing merge result to {}", ours.display()))?;
    Ok(outcome)
}

#[cfg(test)]
mod laws;
#[cfg(test)]
mod tests;
