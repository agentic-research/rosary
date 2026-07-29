//! CLI ↔ MCP surface parity — the declared map, and a ratchet against drift.
//!
//! ## Why
//!
//! Rosary exposes bead operations twice: as CLI verbs (human) and as MCP tools
//! (agent). They are written separately, so they drifted. Measured 2026-07-27
//! against a 51-verb CLI surface and a 41-tool MCP surface: **14 operations on
//! both, 27 MCP-only, 37 CLI-only.** That is not one API with gaps; it is two
//! largely disjoint APIs sharing a binary.
//!
//! Those counts come from the binary describing itself — clap's
//! `CommandFactory` for the CLI, the assembled `tool_definitions()` for MCP —
//! not from scraping source. That distinction is not pedantry: the first
//! version of this gate scraped `main.rs`, and it recorded `coord`, `hooks`,
//! `lattice` and `notes` as leaf verbs when each nests subcommands, so it
//! undercounted the CLI by five and reported a parity picture that was simply
//! false. A gate that describes the surface must ask the thing that owns it.
//!
//! It bit three sessions in a single day: a mache session's `thread_assign`
//! reported success while the query disagreed; an LLO session could not write
//! dependency edges because `link` is MCP-only; and a bead wrongly marked `done`
//! was unrecoverable because `reopen` is CLI-only *and* refuses `done`, while
//! MCP's `update` has no status field at all.
//!
//! ## What this is, and what it deliberately is not
//!
//! It is a **ratchet**, not a fix. Today's 44 gaps (and 20 by-design
//! exemptions) are DECLARED, so the map is executable rather than a note in a
//! doc. What fails the build is a NEW verb
//! appearing on one surface without a decision about the other.
//!
//! It is not a generator. `rosary-08a278`/`rosary-3a5f22` (the capnp tool
//! registry) is what should eventually EMIT both surfaces from one declaration;
//! this table is the input that work needs, and it dies when that lands.
//!
//! ## The two rules that keep it honest
//!
//! 1. **Pairs are matched on OPERATION, not name.** `bead list` and
//!    `rsry_list_beads` are the same operation spelled differently; a
//!    name-based check would report them as two gaps forever.
//! 2. **Every single-surface entry carries a REASON.** An exemption is a
//!    deliberate line of code. Without that the table cannot distinguish
//!    "correctly CLI-only" from "nobody got to it yet" — which is the
//!    ambiguity-by-silence this codebase keeps paying for.

/// Why an operation exists on only one surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Only {
    /// Correct as-is. Process/daemon lifecycle, or something an agent must
    /// never do. Not a gap.
    ByDesign(&'static str),
    /// A real gap: the other surface should have it. Tracked, not accepted.
    Gap(&'static str),
}

/// One operation and how each surface spells it.
#[derive(Debug, Clone, Copy)]
pub struct Op {
    /// CLI form as typed after `rsry`, e.g. `"bead list"`.
    pub cli: Option<&'static str>,
    /// MCP tool name, e.g. `"rsry_list_beads"`.
    pub mcp: Option<&'static str>,
    /// Required whenever exactly one side is present.
    pub only: Option<Only>,
}

const fn both(cli: &'static str, mcp: &'static str) -> Op {
    Op {
        cli: Some(cli),
        mcp: Some(mcp),
        only: None,
    }
}
const fn cli_only(cli: &'static str, why: Only) -> Op {
    Op {
        cli: Some(cli),
        mcp: None,
        only: Some(why),
    }
}
const fn mcp_only(mcp: &'static str, why: Only) -> Op {
    Op {
        cli: None,
        mcp: Some(mcp),
        only: Some(why),
    }
}

use Only::{ByDesign, Gap};

/// The declared surface map. Adding a verb to either surface without adding it
/// here fails `every_surface_verb_is_declared`.
pub const OPS: &[Op] = &[
    // ── on both surfaces ────────────────────────────────────────────────
    both("bead close", "rsry_bead_close"),
    both("bead comment", "rsry_bead_comment"),
    both("bead create", "rsry_bead_create"),
    both("bead import", "rsry_bead_import"),
    both("bead search", "rsry_bead_search"),
    both("decompose", "rsry_decompose"),
    both("dispatch", "rsry_dispatch"),
    both("scan", "rsry_scan"),
    both("status", "rsry_status"),
    both("thread-reparent", "rsry_thread_reparent"),
    // Same operation, different spelling — matched on OPERATION, not name.
    both("bead list", "rsry_list_beads"),
    both("bead review", "rsry_review"),
    both("run", "rsry_run_once"),
    both("enable", "rsry_repo_register"),
    // ── CLI only, correctly ─────────────────────────────────────────────
    cli_only(
        "serve",
        ByDesign("starts the MCP server; an agent cannot start its own transport"),
    ),
    cli_only("start", ByDesign("daemon lifecycle")),
    cli_only("stop", ByDesign("daemon lifecycle")),
    cli_only("logs", ByDesign("daemon lifecycle")),
    cli_only("mcp", ByDesign("alternate transport entry point")),
    cli_only(
        "ipc-call",
        ByDesign("smoke/ops probe against a running server"),
    ),
    cli_only(
        "doctor",
        ByDesign("reports runtime truth about the host; agent-irrelevant"),
    ),
    cli_only(
        "hooks install",
        ByDesign("installs git hooks in the operator's checkout"),
    ),
    cli_only(
        "hooks status",
        ByDesign("reports on the operator's checkout, not on bead state"),
    ),
    cli_only(
        "hooks audit",
        ByDesign(
            "mechanically audits the operator's checkout config (gitignore/backend/drift), not bead state — rosary-b5c8a1",
        ),
    ),
    cli_only(
        "init",
        ByDesign("onboards a repo; precedes any agent having a bead"),
    ),
    cli_only("disable", ByDesign("registry management, operator-only")),
    cli_only(
        "approve",
        ByDesign("the human approval gate — an agent approving itself defeats it"),
    ),
    cli_only("reject", ByDesign("as approve")),
    cli_only(
        "bead merge-jsonl",
        ByDesign("a git merge driver; git invokes it, never a human or agent"),
    ),
    cli_only("bead backup", ByDesign("operator disaster-recovery")),
    cli_only("bead restore", ByDesign("operator disaster-recovery")),
    cli_only(
        "bead migrate",
        ByDesign("backend migration; destructive, operator-gated"),
    ),
    cli_only(
        "migrate",
        ByDesign("orchestrator backend migration; as above"),
    ),
    cli_only(
        "backup",
        ByDesign("orchestrator backup; operator disaster-recovery"),
    ),
    cli_only(
        "notes rotate",
        ByDesign("age recipient rotation; key material is not agent-reachable by design"),
    ),
    // ── CLI only, and that is a GAP ─────────────────────────────────────
    cli_only(
        "bead move",
        Gap(
            "agents cannot relocate a misfiled bead — the operation that fixes what agents get wrong (blocked 15 stranded cloister beads)",
        ),
    ),
    both("bead correct", "rsry_bead_correct"),
    cli_only(
        "bead reopen",
        ByDesign(
            "the state-machine-obeying reopen. It refuses `done` on purpose; \
             correcting a wrongly-recorded status is `bead correct`, which is on \
             BOTH surfaces and demands a reason (rosary-e0e19f)",
        ),
    ),
    cli_only(
        "bead export",
        Gap("agents cannot produce the interop artifact"),
    ),
    cli_only(
        "bead diff",
        Gap("agents cannot inspect what changed in the record"),
    ),
    cli_only(
        "graph",
        Gap("agents cannot render the lattice they navigate"),
    ),
    cli_only(
        "capture",
        Gap("transcript/source -> beads is exactly agent work"),
    ),
    cli_only(
        "plan",
        Gap("Linear ticket -> repo-scoped beads is exactly agent work"),
    ),
    cli_only("sync", Gap("agents cannot reconcile with Linear/GitHub")),
    cli_only("close-merged", Gap("agents cannot run the merged-PR sweep")),
    cli_only("sweep", Gap("agents cannot GC their own merged branches")),
    cli_only(
        "lattice audit",
        Gap("observation-lattice audit is unreachable from MCP"),
    ),
    cli_only(
        "lattice backfill",
        Gap("observation-lattice backfill is unreachable from MCP"),
    ),
    cli_only(
        "pr",
        Gap("agents cannot open a PR through rosary's bead-prefixing wrapper"),
    ),
    cli_only(
        "coord add",
        Gap("the coordination tier (ADR-0022) has no MCP surface yet"),
    ),
    cli_only(
        "coord list",
        Gap("the coordination tier (ADR-0022) has no MCP surface yet"),
    ),
    cli_only(
        "coord rm",
        Gap("the coordination tier (ADR-0022) has no MCP surface yet"),
    ),
    cli_only(
        "coord show",
        Gap("the coordination tier (ADR-0022) has no MCP surface yet"),
    ),
    // ── MCP only, and that is a GAP ─────────────────────────────────────
    mcp_only(
        "rsry_bead_update",
        Gap("a human cannot patch a bead field from the terminal"),
    ),
    mcp_only(
        "rsry_bead_link",
        Gap("a human cannot add a dependency edge — rosary-882154"),
    ),
    mcp_only("rsry_bead_history", Gap("no CLI history view")),
    mcp_only(
        "rsry_bead_comment_list",
        Gap("comment read/edit/delete are MCP-only"),
    ),
    mcp_only("rsry_bead_comment_update", Gap("as comment_list")),
    mcp_only("rsry_bead_comment_delete", Gap("as comment_list")),
    mcp_only("rsry_active", Gap("no CLI view of active work")),
    mcp_only(
        "rsry_expand_ref",
        Gap("no CLI way to expand a CAS ref a human is reading"),
    ),
    mcp_only(
        "rsry_ticket_load",
        Gap("the Linear+GH+bead context loader is agent-only"),
    ),
    mcp_only(
        "rsry_thread_create",
        Gap("thread/decade management is entirely MCP-only — rosary-cac78e"),
    ),
    mcp_only("rsry_thread_list", Gap("as thread_create")),
    mcp_only("rsry_thread_assign", Gap("as thread_create")),
    mcp_only("rsry_decade_create", Gap("as thread_create")),
    mcp_only("rsry_decade_list", Gap("as thread_create")),
    mcp_only(
        "rsry_workspace_create",
        Gap("workspace lifecycle is MCP-only"),
    ),
    mcp_only("rsry_workspace_checkpoint", Gap("as workspace_create")),
    mcp_only("rsry_workspace_cleanup", Gap("as workspace_create")),
    mcp_only("rsry_workspace_merge", Gap("as workspace_create")),
    mcp_only("rsry_pipeline_upsert", Gap("pipeline state is MCP-only")),
    mcp_only("rsry_pipeline_query", Gap("as pipeline_upsert")),
    mcp_only("rsry_dispatch_record", Gap("dispatch history is MCP-only")),
    mcp_only("rsry_dispatch_history", Gap("as dispatch_record")),
    mcp_only(
        "rsry_agent_run_event_record",
        Gap("the feedback substrate has no CLI surface"),
    ),
    mcp_only("rsry_agent_run_events", Gap("as agent_run_event_record")),
    mcp_only(
        "rsry_agent_session_addresses",
        Gap("as agent_run_event_record"),
    ),
    mcp_only(
        "rsry_agent_session_message_record",
        Gap("as agent_run_event_record"),
    ),
    mcp_only(
        "rsry_repo_list",
        Gap("`rsry enable` registers but there is no CLI list"),
    ),
];

#[cfg(test)]
mod tests;
