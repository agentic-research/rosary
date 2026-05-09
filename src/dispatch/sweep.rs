//! Orphan-dispatch detection and recovery.
//!
//! A bead is an "orphan dispatch" when:
//! - its state is `BeadState::Dispatched` (i.e. status is `"dispatched"` or
//!   `"in_progress"` — Linear's vocabulary maps to the same state per
//!   `bead.rs:147`)
//! - **and** there is no live agent process working on it: no entry in the
//!   session registry with a live PID, **and** no worktree on disk at
//!   `~/.rsry/worktrees/<repo>/<bead>/`
//!
//! This happens when `rsry_dispatch` stages a worktree + flips status,
//! the caller never spawns the agent (or removes the worktree before
//! spawning), and nothing else cleans up. Without this sweep the bead
//! sits in `Dispatched` forever — invisible-but-harmful state: triage
//! skips it, the dispatcher won't redispatch, the statusline counts it
//! as active. See bead `rosary-67c43d`.
//!
//! The sweep is conservative: it never touches a bead with a live
//! session or an existing worktree. Worst case is a slightly slower
//! `rsry_dispatch` / `rsry_active` (a few `kill(pid, 0)` syscalls + a
//! `Path::exists`).

use crate::bead::BeadState;
use crate::session::{SessionRegistry, is_pid_alive};
use crate::store::BeadStore;

/// Result of a sweep — the bead IDs that were reverted to `open`.
#[derive(Debug, Default, Clone)]
pub struct SweepReport {
    pub reverted: Vec<String>,
}

impl SweepReport {
    /// Used by tests + future MCP `rsry_dispatch_sweep` exposure. Marked
    /// dead-code-allowed because the production callers currently ignore
    /// the report (best-effort cleanup).
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.reverted.is_empty()
    }
}

/// Sweep orphan dispatches in the given repo. Reverts any
/// `BeadState::Dispatched` bead with no live session and no worktree
/// back to `"open"`.
///
/// `repo_path` is the on-disk root of the repo (e.g.
/// `/Users/foo/code/myrepo`); `repo_name` is the basename used to look
/// up worktree locations and session registry entries (e.g. `myrepo`).
pub async fn sweep_orphan_dispatches(
    client: &dyn BeadStore,
    repo_path: &std::path::Path,
    repo_name: &str,
) -> SweepReport {
    let mut report = SweepReport::default();

    let beads = match client.list_beads(repo_name).await {
        Ok(b) => b,
        Err(e) => {
            eprintln!("[sweep] list_beads({repo_name}) failed: {e}");
            return report;
        }
    };

    let registry = SessionRegistry::load().unwrap_or_default();

    for bead in beads {
        if bead.state() != BeadState::Dispatched {
            continue;
        }

        // Live session with a live PID? Leave it alone — there's a real
        // agent at work.
        let has_live_session = registry.active().iter().any(|s| {
            s.bead_id == bead.id && s.repo == repo_name && s.pid.map(is_pid_alive).unwrap_or(false)
        });
        if has_live_session {
            continue;
        }

        // No live session — does a worktree still exist on disk?
        // If so, leave it alone: the agent process may have died but
        // there's unmerged work to recover via rsry_workspace_merge.
        let workspace = crate::workspace::workspace_dir(repo_path, &bead.id);
        if workspace.exists() {
            continue;
        }

        // Orphan confirmed: status is Dispatched, no live PID, no worktree.
        // Revert to open so triage can pick it back up.
        match client.update_status(&bead.id, "open").await {
            Ok(()) => {
                eprintln!(
                    "[sweep] reverted orphan dispatch {} → open (no live session, no worktree)",
                    bead.id
                );
                let _ = client
                    .log_event(
                        &bead.id,
                        "orphan_revert",
                        "no live session + no worktree at sweep time",
                    )
                    .await;
                report.reverted.push(bead.id);
            }
            Err(e) => {
                eprintln!("[sweep] failed to revert {}: {e}", bead.id);
            }
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead_sqlite::SqliteBeadStore;
    use tempfile::TempDir;

    fn test_store_in(dir: &std::path::Path) -> SqliteBeadStore {
        SqliteBeadStore::connect(&dir.join("beads.db")).unwrap()
    }

    #[tokio::test]
    async fn sweep_reverts_dispatched_bead_with_no_session_no_worktree() {
        let tmp = TempDir::new().unwrap();
        let store = test_store_in(tmp.path());
        store
            .create_bead("orphan-1", "T", "", 1, "task")
            .await
            .unwrap();
        store.update_status("orphan-1", "dispatched").await.unwrap();

        let report = sweep_orphan_dispatches(&store, tmp.path(), "no-such-repo-xyz").await;
        // No worktree exists at ~/.rsry/worktrees/no-such-repo-xyz/orphan-1,
        // no session registered → orphan reverted.
        assert_eq!(report.reverted, vec!["orphan-1".to_string()]);

        let bead = store
            .get_bead("orphan-1", "no-such-repo-xyz")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(bead.status, "open");
    }

    #[tokio::test]
    async fn sweep_also_reverts_in_progress_alias() {
        // Linear sync writes "in_progress" — bead.rs:147 maps it to
        // BeadState::Dispatched. The sweep must catch this alias too.
        let tmp = TempDir::new().unwrap();
        let store = test_store_in(tmp.path());
        store
            .create_bead("orphan-2", "T", "", 1, "task")
            .await
            .unwrap();
        store
            .update_status("orphan-2", "in_progress")
            .await
            .unwrap();

        let report = sweep_orphan_dispatches(&store, tmp.path(), "no-such-repo-xyz").await;
        assert_eq!(report.reverted, vec!["orphan-2".to_string()]);
    }

    #[tokio::test]
    async fn sweep_leaves_open_beads_alone() {
        let tmp = TempDir::new().unwrap();
        let store = test_store_in(tmp.path());
        store.create_bead("a", "T", "", 1, "task").await.unwrap();
        // Status is "open" by default; sweep must not touch it.

        let report = sweep_orphan_dispatches(&store, tmp.path(), "no-such-repo-xyz").await;
        assert!(report.is_empty());
    }

    #[tokio::test]
    async fn sweep_leaves_dispatched_with_existing_worktree() {
        let tmp = TempDir::new().unwrap();
        let store = test_store_in(tmp.path());
        store
            .create_bead("guarded", "T", "", 1, "task")
            .await
            .unwrap();
        store.update_status("guarded", "dispatched").await.unwrap();

        // The sweep computes the worktree path from `repo_path` basename
        // (`workspace::workspace_dir`), so pre-create at exactly that path
        // — otherwise the lookup misses and the bead gets falsely reverted.
        let workspace = crate::workspace::workspace_dir(tmp.path(), "guarded");
        std::fs::create_dir_all(&workspace).unwrap();

        let report = sweep_orphan_dispatches(&store, tmp.path(), "sweep-test-guarded-repo").await;
        assert!(
            report.is_empty(),
            "must not revert when worktree still exists — unmerged work might be there",
        );
        let bead = store
            .get_bead("guarded", "sweep-test-guarded-repo")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(bead.status, "dispatched");

        // Cleanup — remove the test worktree dir so re-runs don't accumulate.
        let _ = std::fs::remove_dir_all(&workspace);
    }
}
