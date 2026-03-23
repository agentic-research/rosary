//! Workspace checkpoint and cleanup during pipeline phase transitions.
//!
//! Sentry spans: `reconcile.workspace.checkpoint`, `reconcile.workspace.cleanup`

use super::Reconciler;
use crate::store::BeadRef;

impl Reconciler {
    /// Checkpoint workspace (jj commit + bookmark) without cleanup.
    ///
    /// Used during phase advancement: the workspace stays alive so the
    /// next pipeline agent reuses the same worktree and its changes.
    pub(super) async fn checkpoint_workspace(&mut self, bead_id: &str) -> Option<String> {
        let change_id = if let Some(ws) = self.completed_workspaces.remove(bead_id) {
            let message = format!("[{bead_id}] fix: agent checkpoint");
            let result = ws.checkpoint(&message).await;
            // Put it back — workspace stays for next phase or cleanup
            self.completed_workspaces.insert(bead_id.to_string(), ws);
            match result {
                Ok(Some(id)) => {
                    eprintln!("[checkpoint] {bead_id}: jj change {id}");
                    Some(id)
                }
                Ok(None) => None,
                Err(e) => {
                    eprintln!("[checkpoint] {bead_id}: failed: {e}");
                    None
                }
            }
        } else {
            None
        };

        // Log change_id as event for audit trail
        if let Some(ref cid) = change_id {
            let repo = self
                .trackers
                .get(bead_id)
                .map(|t| t.repo.clone())
                .unwrap_or_default();
            if let Some(client) = self.dolt_client(&repo).await {
                client.log_event(bead_id, "jj_checkpoint", cid).await;
            }
        }

        change_id
    }

    /// Checkpoint workspace then write handoff + manifest, then clean up.
    ///
    /// Used when the pipeline is complete (no next agent) or on deadletter.
    pub(super) async fn checkpoint_and_cleanup(&mut self, bead_id: &str) -> Option<String> {
        let change_id = self.checkpoint_workspace(bead_id).await;

        // Write handoff + manifest to workspace before cleanup
        let repo = self
            .trackers
            .get(bead_id)
            .map(|t| t.repo.clone())
            .unwrap_or_default();
        if let Some(ws) = self.completed_workspaces.get(bead_id) {
            let work_dir = &ws.work_dir;

            // Build work summary from git
            let work = crate::manifest::Work::from_git(work_dir, change_id.as_deref());

            // Write handoff for the phase that just completed
            let (agent, phase) = self
                .trackers
                .get(bead_id)
                .map(|t| {
                    (
                        t.current_agent
                            .clone()
                            .unwrap_or_else(|| "dev-agent".to_string()),
                        t.phase_index,
                    )
                })
                .unwrap_or_else(|| ("dev-agent".to_string(), 0));
            let mut handoff = crate::handoff::Handoff::new(
                phase,
                &agent,
                None,
                bead_id,
                self.provider.name(),
                &work,
            );
            // Look up thread_id from hierarchy if available
            if let Some(ref hierarchy) = self.hierarchy {
                let bead_ref = BeadRef {
                    repo: repo.clone(),
                    bead_id: bead_id.to_string(),
                };
                if let Ok(Some(tid)) = hierarchy.find_thread_for_bead(&bead_ref).await {
                    handoff.thread_id = Some(tid);
                }
            }
            if let Err(e) = handoff.write_to(work_dir) {
                eprintln!("[handoff] {bead_id}: failed to write: {e}");
            }

            // Write manifest
            let vcs_kind = match ws.vcs {
                crate::workspace::VcsKind::Jj => "jj",
                crate::workspace::VcsKind::Git => "git",
                crate::workspace::VcsKind::None => "none",
            };
            let mut manifest = crate::manifest::Manifest::at_spawn(
                &format!("d-{bead_id}"),
                bead_id,
                &repo,
                &agent,
                self.provider.name(),
                "task",
                "implement",
                phase,
                &work_dir.display().to_string(),
                &ws.repo_path.display().to_string(),
                vcs_kind,
                None,
            );
            manifest.work = work;
            manifest.complete(true, Some("end_turn"));
            if let Err(e) = manifest.write_to(work_dir) {
                eprintln!("[manifest] {bead_id}: failed to write: {e}");
            }
        }

        // Terminal step: merge or PR based on issue type.
        // Runs outside the workspace borrow scope to allow dolt_client access.
        if let Some(ws) = self.completed_workspaces.get(bead_id) {
            let branch = format!("fix/{bead_id}");
            let ws_repo_path = ws.repo_path.clone();
            let issue_type = if let Some(client) = self.dolt_client(&repo).await {
                client
                    .get_bead(bead_id, &repo)
                    .await
                    .ok()
                    .flatten()
                    .map(|b| b.issue_type)
                    .unwrap_or_else(|| "task".to_string())
            } else {
                "task".to_string()
            };
            // Resolve the PR base: thread feature branch if bead belongs to a thread,
            // otherwise default (main). BDR→git: bead PRs into thread branch.
            let base: Option<String> = if let Some(ref hierarchy) = self.hierarchy {
                let bead_ref = BeadRef {
                    repo: repo.clone(),
                    bead_id: bead_id.to_string(),
                };
                if let Ok(Some(thread_id)) = hierarchy.find_thread_for_bead(&bead_ref).await {
                    let decade_id = thread_id.split('/').next().unwrap_or(&thread_id);
                    hierarchy
                        .list_threads(decade_id)
                        .await
                        .ok()
                        .and_then(|threads| {
                            threads
                                .iter()
                                .find(|t| t.id == thread_id)
                                .and_then(|t| t.feature_branch.clone())
                        })
                } else {
                    None
                }
            } else {
                None
            };

            if let Ok(result) = crate::workspace::merge_or_pr_with_base(
                &ws_repo_path,
                &branch,
                bead_id,
                &issue_type,
                base.as_deref(),
            )
            .await
            {
                // Record PR URL on the bead as comment + event (event used by poll_pr_merges)
                if let Some(ref pr_url) = result.pr_url
                    && let Some(client) = self.dolt_client(&repo).await
                {
                    let _ = client
                        .add_comment(bead_id, &format!("PR: {pr_url}"), "rosary")
                        .await;
                    client.log_event(bead_id, "pr_url", pr_url).await;
                }
            }
        }

        self.cleanup_workspace(bead_id);
        change_id
    }

    /// Clean up the workspace for a completed bead.
    /// Delegates to workspace.rs cleanup functions to avoid duplication.
    /// No-op when no workspace is tracked — avoids touching the real filesystem
    /// for unknown beads (safety: prevents deleting worktrees from other reconcilers).
    pub(super) fn cleanup_workspace(&mut self, bead_id: &str) {
        if let Some(ws) = self.completed_workspaces.remove(bead_id) {
            eprintln!(
                "[cleanup] {bead_id} workspace (vcs={:?}, compute={})",
                ws.vcs,
                self.compute.name()
            );
            match ws.vcs {
                crate::workspace::VcsKind::Jj => {
                    crate::workspace::cleanup_jj_workspace(&ws.repo_path, bead_id);
                }
                crate::workspace::VcsKind::Git => {
                    crate::workspace::cleanup_git_worktree(&ws.repo_path, bead_id);
                }
                crate::workspace::VcsKind::None => {}
            }
        } else {
            // No workspace tracked — skip cleanup. The legacy fallback that
            // cleaned up from "." was unsafe: it could delete worktrees
            // belonging to other reconcilers or from previous runs.
            eprintln!("[cleanup] {bead_id}: no workspace tracked, skipping");
        }
    }
}
