//! The canonical-field drift gate (rosary-c735cc, ADR-0021 slice 3).
//!
//! ## What it checks
//!
//! One canonical field set — `Bead`'s own serde fields — against every surface
//! that is supposed to carry it. A field missing from a surface is either a
//! **declared exemption with a reason**, or a build failure.
//!
//! ADR-0021's diagnosis is that the field set is re-typed on ~7 surfaces and
//! nothing notices when one omits a member. Three instances have already been
//! paid for: `acceptance_criteria` dropped on a create path (`rosary-4887d0`),
//! `get_bead` reading fewer fields than `list_beads`, and `derived_from` absent
//! from every export (`rosary-79393f`, 32 beads, total loss). Each was fixed by
//! hand. This is the check that makes the fourth impossible to land quietly.
//!
//! ## Derived from the authority, never a copy
//!
//! The canonical set comes from serializing a `Bead` — the struct is the
//! declaration, so the gate cannot disagree with it about which fields exist.
//! Each surface is likewise interrogated rather than described:
//!
//! | surface | asked by |
//! |---|---|
//! | export contract | calling `bead_to_contract_value` and reading its keys |
//! | store write | serializing a `NewBead` |
//! | MCP `bead_create` / `bead_update` args | reading the assembled `tool_definitions()` |
//!
//! That matters: the first version of `src/parity` scraped source and was green
//! while wrong in four places. A gate that describes a surface must ask the
//! thing that owns it.
//!
//! ## Exemptions are code, not silence
//!
//! Most fields are genuinely absent from some surface for a good reason —
//! `dependency_count` is computed by a COUNT join, `repo` is stamped by the
//! reader. [`EXEMPT`] records each with its reason, so "correctly absent" is
//! distinguishable from "nobody got to it", which is the ambiguity this codebase
//! keeps paying for.

/// A `(field, surface)` pair that is deliberately absent, and why.
#[derive(Debug, Clone, Copy)]
pub struct Exemption {
    pub field: &'static str,
    pub surface: Surface,
    pub reason: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// The exported JSON contract (`bead_to_contract_value`).
    Export,
    /// Inputs accepted when authoring a bead (`NewBead`).
    StoreWrite,
    /// `rsry_bead_create`'s advertised MCP arguments.
    McpCreate,
    /// `rsry_bead_update`'s advertised MCP arguments.
    McpUpdate,
}

impl Surface {
    pub fn name(self) -> &'static str {
        match self {
            Surface::Export => "export contract",
            Surface::StoreWrite => "store write (NewBead)",
            Surface::McpCreate => "MCP rsry_bead_create args",
            Surface::McpUpdate => "MCP rsry_bead_update args",
        }
    }
}

const fn ex(field: &'static str, surface: Surface, reason: &'static str) -> Exemption {
    Exemption {
        field,
        surface,
        reason,
    }
}

/// Fields deliberately absent from a surface. Every entry is a decision.
pub const EXEMPT: &[Exemption] = &[
    // --- computed, never authored or stored ---
    ex(
        "dependency_count",
        Surface::StoreWrite,
        "computed by the read query's COUNT join, not authored",
    ),
    ex(
        "dependent_count",
        Surface::StoreWrite,
        "computed by the read query's COUNT join, not authored",
    ),
    ex(
        "comment_count",
        Surface::StoreWrite,
        "computed by the read query's COUNT join, not authored",
    ),
    ex(
        "dependency_count",
        Surface::McpCreate,
        "computed; a caller cannot set it",
    ),
    ex(
        "dependent_count",
        Surface::McpCreate,
        "computed; a caller cannot set it",
    ),
    ex(
        "comment_count",
        Surface::McpCreate,
        "computed; a caller cannot set it",
    ),
    ex(
        "dependency_count",
        Surface::McpUpdate,
        "computed; a caller cannot set it",
    ),
    ex(
        "dependent_count",
        Surface::McpUpdate,
        "computed; a caller cannot set it",
    ),
    ex(
        "comment_count",
        Surface::McpUpdate,
        "computed; a caller cannot set it",
    ),
    // --- stamped by the reader from context ---
    ex(
        "repo",
        Surface::StoreWrite,
        "stamped by the reader from the repo it was read out of (bead_from_row)",
    ),
    ex(
        "repo",
        Surface::McpCreate,
        "implied by repo_path; stamping it twice invites disagreement",
    ),
    ex(
        "repo",
        Surface::McpUpdate,
        "implied by repo_path; stamping it twice invites disagreement",
    ),
    // --- assigned by the store, not the author ---
    ex(
        "created_at",
        Surface::StoreWrite,
        "assigned by the store on insert",
    ),
    ex(
        "updated_at",
        Surface::StoreWrite,
        "assigned by the store on every write",
    ),
    ex("created_at", Surface::McpCreate, "assigned by the store"),
    ex("updated_at", Surface::McpCreate, "assigned by the store"),
    ex("created_at", Surface::McpUpdate, "assigned by the store"),
    ex("updated_at", Surface::McpUpdate, "assigned by the store"),
    ex(
        "status",
        Surface::StoreWrite,
        "a create always starts Open; transitions go through update_status so the \
         state machine is not bypassed",
    ),
    ex("status", Surface::McpCreate, "a create always starts Open"),
    // --- not populated by rosary's native store (bead_ext records this) ---
    ex(
        "branch",
        Surface::StoreWrite,
        "github extension; bead_from_row hardcodes None — see bead_ext",
    ),
    ex(
        "pr_url",
        Surface::StoreWrite,
        "github extension; bead_from_row hardcodes None — see bead_ext",
    ),
    ex(
        "jj_change_id",
        Surface::StoreWrite,
        "jj extension; bead_from_row hardcodes None — see bead_ext",
    ),
    ex(
        "branch",
        Surface::McpCreate,
        "github extension, not authored here",
    ),
    ex(
        "pr_url",
        Surface::McpCreate,
        "github extension, not authored here",
    ),
    ex(
        "jj_change_id",
        Surface::McpCreate,
        "jj extension, not authored here",
    ),
    ex(
        "branch",
        Surface::McpUpdate,
        "github extension, not authored here",
    ),
    ex(
        "pr_url",
        Surface::McpUpdate,
        "github extension, not authored here",
    ),
    ex(
        "jj_change_id",
        Surface::McpUpdate,
        "jj extension, not authored here",
    ),
    ex(
        "external_ref",
        Surface::StoreWrite,
        "set by the tracker sync (set_external_ref), not at authoring time",
    ),
    ex(
        "external_ref",
        Surface::McpCreate,
        "set by the tracker sync, not by a caller",
    ),
    ex(
        "external_ref",
        Surface::McpUpdate,
        "set by the tracker sync, not by a caller",
    ),
    ex(
        "derived_from",
        Surface::McpCreate,
        "BDR provenance is attached by decompose/capture, not typed by a caller",
    ),
    ex(
        "derived_from",
        Surface::McpUpdate,
        "BDR provenance is attached by decompose/capture, not typed by a caller",
    ),
    ex(
        "id",
        Surface::McpCreate,
        "minted by the store (generate_bead_id); a caller-supplied id would collide",
    ),
    ex(
        "created_by",
        Surface::McpUpdate,
        "authorship is immutable once set",
    ),
    ex(
        "created_by",
        Surface::McpCreate,
        "derived, not accepted: the handler reads the git config user from \
         repo_path (handlers/mod.rs), so a caller-supplied value would compete \
         with attribution the server can establish itself",
    ),
    ex(
        "status",
        Surface::McpUpdate,
        "corrections go through `rsry_bead_correct`, which demands a reason and \
         records it; a bare status field on update would let an agent rewrite \
         state with no audit trail. This was a GAP (the rosary-e0e19f recovery \
         wall) until that tool landed",
    ),
];

#[cfg(test)]
mod tests;
