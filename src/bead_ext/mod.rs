//! Registered bead-field extensions (rosary-c47ca6).
//!
//! ## The decision this implements
//!
//! ADR-0021 poses `branch` / `pr_url` / `jj_change_id` as a binary: either they
//! become canonical columns, or they are declared out of scope. Both answers are
//! wrong, and the evidence was already in the repo.
//!
//! The `notes` JSON column **is** an object store. It holds exactly three keys
//! today — `files`, `test_files`, `derived_from`. `bead_to_contract_value`
//! hand-listed two of them. The third was silently lost: 862 beads carried
//! `derived_from` in the store and **zero** carried it in the tracked JSONL
//! (`rosary-79393f`), and `restore/mod.rs` filled it with `Vec::new()` rather
//! than reading it.
//!
//! So the bug was never "bag versus fixed field list". It was that the bag had
//! **no declaration**, so every projection hand-enumerated its contents and one
//! got missed. An open bag with no schema reproduces the same defect one level
//! down.
//!
//! ## The shape
//!
//! A core canonical set (the 16 real `issues` columns) plus extensions that
//! declare their namespace and fields **once**, with every projection walking
//! the registry instead of a hand-written list.
//!
//! This dissolves ADR-0021's open question rather than answering it: those
//! fields are neither in nor out, they are owned by whichever tool populates
//! them. It is also what lets mache / cloister / signet carry their own bead
//! metadata without rosary's core schema changing — the property that makes the
//! bead format a substrate rather than a rosary-private struct, and the same
//! reason ADR-0014 made `beads.jsonl` a contract.
//!
//! ## Why serde, not a hand-written accessor per field
//!
//! `Bead` derives `Serialize`/`Deserialize`, so the struct itself already
//! defines every field's name and wire shape. Projecting through serde means the
//! registry only has to name fields — it never restates their types, and it
//! cannot disagree with the struct about them. A registry of typed getters would
//! be a second declaration of the same thing, which is the defect being deleted.
//!
//! ## What this deliberately does NOT do yet
//!
//! Only the EXPORT projection walks the registry, because that is where the
//! measured loss was (862 beads with `derived_from` in the store, 0 in the
//! export). The import side still names `derived_from` once in `restore/mod.rs`,
//! and the store's `notes` write still lists its three keys in `bead_sqlite`.
//!
//! Both are blocked on the same thing: `NewBead` derives only `Debug, Clone`, so
//! a bead cannot be constructed from JSON and a registry-driven writer has
//! nowhere to put the values. That is ADR-0021 slice 2 (`rosary-c7126b`, unify
//! writes).
//!
//! Naming the boundary rather than shipping an unused `absorb` alongside
//! `project`: an inverse primitive with no caller is a guess about the shape of
//! work not yet done, and it would rot before slice 2 arrives.

use serde_json::{Map, Value};

/// One field carried by an extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtField {
    /// Field name, matching the `Bead` struct field (and therefore its serde
    /// key). Not repeated anywhere else.
    pub name: &'static str,
    /// Whether rosary's own SQLite store populates it.
    ///
    /// `false` records a fact rather than an aspiration: `bead_from_row`
    /// hardcodes `branch`/`pr_url`/`jj_change_id` to `None`, so nothing in the
    /// native store writes them today. ADR-0021 requires this be *recorded in
    /// the canonical definition* so a Dolt→SQLite migration cannot silently
    /// "lose" a field nobody decided to drop.
    pub native: bool,
}

/// A namespace and the fields it owns.
#[derive(Debug, Clone, Copy)]
pub struct Extension {
    /// Who owns these fields. Declarative today — no production code reads it,
    /// because rosary is still the only registrant. It stays because it is the
    /// point of the design (mache / cloister / signet carrying their own bead
    /// metadata without rosary's schema changing), and because deleting it would
    /// mean re-deciding namespacing when the second tool arrives rather than
    /// reading the decision already recorded here.
    #[allow(dead_code)]
    pub namespace: &'static str,
    pub fields: &'static [ExtField],
}

/// The canonical registry.
///
/// Adding a field here is the ONLY edit needed for it to survive export,
/// import, restore and migration — asserted by
/// `tests::a_synthetic_extension_round_trips_with_no_projection_edit`.
pub const EXTENSIONS: &[Extension] = &[
    Extension {
        namespace: "rosary",
        fields: &[
            ExtField {
                name: "files",
                native: true,
            },
            ExtField {
                name: "test_files",
                native: true,
            },
            ExtField {
                name: "derived_from",
                native: true,
            },
        ],
    },
    Extension {
        namespace: "github",
        fields: &[
            ExtField {
                name: "branch",
                native: false,
            },
            ExtField {
                name: "pr_url",
                native: false,
            },
        ],
    },
    Extension {
        namespace: "jj",
        fields: &[ExtField {
            name: "jj_change_id",
            native: false,
        }],
    },
];

/// Every registered field name, across all namespaces.
pub fn field_names(exts: &[Extension]) -> impl Iterator<Item = &'static str> + '_ {
    exts.iter().flat_map(|e| e.fields.iter().map(|f| f.name))
}

/// Project a serialized bead's extension fields into a map, by walking the
/// registry.
///
/// Takes an already-serialized `Value` rather than `&Bead` so the same function
/// serves both the export path (serialize, then project) and any caller that
/// already holds the JSON form. Absent fields are skipped rather than emitted
/// as null — an extension that a producer does not populate should be silent,
/// not present-and-empty.
pub fn project(bead_json: &Value, exts: &[Extension]) -> Map<String, Value> {
    let mut out = Map::new();
    for name in field_names(exts) {
        if let Some(v) = bead_json.get(name)
            && !v.is_null()
        {
            out.insert(name.to_string(), v.clone());
        }
    }
    out
}

/// Copy every registered extension field present in `source` onto `target`.
///
/// The inverse of [`project`], and the reason a new registry entry needs no
/// import-side edit either. It was deliberately withheld in rosary-c47ca6
/// because nothing could call it — `NewBead` was not deserializable, so the
/// import side had nowhere to put the values. Slice 2 (`rosary-c7126b`) made
/// `NewBead` serde-able, so it now has exactly one caller: `restore`.
pub fn absorb(target: &mut Map<String, Value>, source: &Value, exts: &[Extension]) {
    for name in field_names(exts) {
        if let Some(v) = source.get(name)
            && !v.is_null()
        {
            target.insert(name.to_string(), v.clone());
        }
    }
}

#[cfg(test)]
mod tests;
