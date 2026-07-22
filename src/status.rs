//! Single-source status rollup — the ONE aggregation every surface projects
//! from: `rsry status --json` (CLI, which the statusline shells out to) and
//! `rsry_status` (MCP). Before this, the CLI and MCP each hand-rolled their own
//! count (MCP structurally omitted `done` and per-repo breakdown, and scanned
//! open-only), so the two surfaces disagreed. This is ADR-0021's single-source
//! principle (and ADR-0006's unify-MCP/CLI) applied to the aggregation surface:
//! define the buckets once, every surface calls [`status_json`].
//!
//! Bucketing: each bead lands in exactly ONE primary bucket, in priority order
//! **done → blocked → in_progress → open**, so the primary buckets partition
//! the bead set (they sum to `total`, modulo a few non-primary statuses like
//! `pr_open`). `ready` and `dispatchable` are **overlays** — subsets of the
//! open/actionable set counted independently (a ready bead is also in `open`),
//! using the canonical `Bead` predicates so "what is ready/blocked" is defined
//! in one place too.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::bead::Bead;

/// Aggregated bead counts for one scope (global or a single repo).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct StatusCounts {
    pub total: usize,
    /// Exclusive: open AND not blocked/in_progress/terminal.
    pub open: usize,
    pub in_progress: usize,
    pub blocked: usize,
    pub done: usize,
    /// Overlay ⊆ actionable: open + unblocked (rendering-ready).
    pub ready: usize,
    /// Overlay ⊆ ready: safe to fan out (close condition + bounded scope +
    /// refined). A strict subset of `ready` (rosary-d4bb09).
    pub dispatchable: usize,
}

fn is_done(b: &Bead) -> bool {
    b.status == "done" || b.status == "closed"
}

fn is_in_progress(b: &Bead) -> bool {
    b.status == "in_progress" || b.status == "dispatched"
}

/// Fold one bead into `counts` — the single per-bead classification both the
/// global and per-repo rollups (and therefore both CLI and MCP) share.
fn fold(counts: &mut StatusCounts, b: &Bead) {
    counts.total += 1;
    // Exactly one primary bucket, priority order.
    if is_done(b) {
        counts.done += 1;
    } else if b.is_blocked() {
        counts.blocked += 1;
    } else if is_in_progress(b) {
        counts.in_progress += 1;
    } else if b.status == "open" {
        counts.open += 1;
    }
    // Overlays (independent of the primary bucket).
    if b.is_ready() {
        counts.ready += 1;
    }
    if b.is_dispatchable() {
        counts.dispatchable += 1;
    }
}

/// Roll every bead into one global [`StatusCounts`].
pub fn status_rollup(beads: &[Bead]) -> StatusCounts {
    let mut counts = StatusCounts::default();
    for b in beads {
        fold(&mut counts, b);
    }
    counts
}

/// Roll beads into a per-repo map (keyed by `Bead::repo`), each entry the same
/// [`StatusCounts`] shape as the global rollup — so per-repo and global stay
/// consistent by construction (each repo's counts sum to the global).
pub fn status_rollup_by_repo(beads: &[Bead]) -> BTreeMap<String, StatusCounts> {
    let mut per_repo: BTreeMap<String, StatusCounts> = BTreeMap::new();
    for b in beads {
        fold(per_repo.entry(b.repo.clone()).or_default(), b);
    }
    per_repo
}

/// The canonical status JSON both `rsry status --json` and `rsry_status` emit —
/// global counts flattened at the top level plus a `repos` map. One function =
/// the two surfaces cannot drift.
pub fn status_json(beads: &[Bead]) -> serde_json::Value {
    let global = status_rollup(beads);
    let per_repo = status_rollup_by_repo(beads);
    let mut v = serde_json::to_value(&global).expect("StatusCounts serializes");
    v["repos"] = serde_json::to_value(&per_repo).expect("per-repo serializes");
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bead(id: &str, repo: &str, status: &str) -> Bead {
        let mut b = crate::testutil::make_bead(id, "task", repo);
        b.status = status.to_string();
        b
    }

    #[test]
    fn primary_buckets_partition_the_set() {
        let beads = vec![
            bead("a", "r", "open"),
            bead("b", "r", "done"),
            bead("c", "r", "closed"), // closed folds into done
            bead("d", "r", "in_progress"),
        ];
        let c = status_rollup(&beads);
        assert_eq!(c.total, 4);
        assert_eq!(c.done, 2, "done + closed both count as done");
        assert_eq!(c.in_progress, 1);
        assert_eq!(c.open, 1);
        // done + blocked + in_progress + open accounts for every bead here.
        assert_eq!(c.done + c.blocked + c.in_progress + c.open, 4);
    }

    #[test]
    fn per_repo_sums_to_global() {
        let beads = vec![
            bead("a", "rosary", "open"),
            bead("b", "rosary", "done"),
            bead("c", "mache", "open"),
        ];
        let global = status_rollup(&beads);
        let per_repo = status_rollup_by_repo(&beads);
        let summed_total: usize = per_repo.values().map(|c| c.total).sum();
        let summed_open: usize = per_repo.values().map(|c| c.open).sum();
        assert_eq!(summed_total, global.total, "per-repo totals sum to global");
        assert_eq!(summed_open, global.open, "per-repo open sums to global");
        assert_eq!(per_repo["rosary"].total, 2);
        assert_eq!(per_repo["mache"].total, 1);
    }

    #[test]
    fn json_shape_carries_done_and_repos() {
        // The exact fields MCP was missing (done, repos) must be present — this
        // is the drift the single source closes.
        let beads = vec![bead("a", "r", "done"), bead("b", "r", "open")];
        let j = status_json(&beads);
        for key in [
            "total",
            "open",
            "in_progress",
            "blocked",
            "done",
            "ready",
            "dispatchable",
            "repos",
        ] {
            assert!(j.get(key).is_some(), "status JSON must carry `{key}`");
        }
        assert_eq!(j["done"], 1);
        assert!(j["repos"].get("r").is_some());
        assert_eq!(j["repos"]["r"]["done"], 1);
    }
}
