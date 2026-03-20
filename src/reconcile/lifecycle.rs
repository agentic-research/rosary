//! Bead lifecycle: pass/fail handlers, status persistence, workspace management.

use std::time::Instant;

use crate::verify::VerifySummary;

use super::{BeadTracker, Reconciler};

impl Reconciler {
    /// Update bead status in Dolt and log the transition. Best-effort.
    /// Also mirrors the transition to the external issue tracker (Linear)
    /// if the bead has an external_ref and a tracker is configured.
    pub(crate) async fn persist_status(&mut self, bead_id: &str, repo: &str, status: &str) {
        // 1. Write to Dolt (source of truth) and fetch external_ref
        let has_tracker = self.issue_tracker.is_some();
        let mut external_ref: Option<String> = None;
        if let Some(client) = self.dolt_client(repo).await {
            if let Err(e) = client.update_status(bead_id, status).await {
                eprintln!("[dolt] failed to update {bead_id} to {status}: {e}");
            }
            client
                .log_event(bead_id, "state_change", &format!("→ {status}"))
                .await;
            if has_tracker {
                external_ref = client.get_external_ref(bead_id).await.ok().flatten();
            }
        }

        // 2. Mirror to external issue tracker (best-effort, never blocks)
        // Pass bead status — the tracker handles mapping to its native states.
        if let (Some(tracker), Some(ext_ref)) = (&self.issue_tracker, external_ref) {
            if let Err(e) = tracker.update_status(&ext_ref, status).await {
                eprintln!(
                    "[{}] failed to mirror {bead_id} → {ext_ref}: {e}",
                    tracker.name()
                );
            } else {
                eprintln!(
                    "[{}] mirrored {bead_id} → {ext_ref} ({status})",
                    tracker.name()
                );
            }
        }
    }

    pub(crate) fn on_pass(&mut self, bead_id: &str) {
        println!("[pass] {bead_id}");
        self.queue.clear_backoff(bead_id);
        if let Some(tracker) = self.trackers.get_mut(bead_id) {
            tracker.consecutive_reverts = 0;
        }
        // Cleanup happens after checkpoint (called from iterate)
    }

    /// Checkpoint workspace (jj commit + bookmark) without cleanup.
    ///
    /// Used during phase advancement: the workspace stays alive so the
    /// next pipeline agent reuses the same worktree and its changes.
    pub(crate) async fn checkpoint_workspace(&mut self, bead_id: &str) -> Option<String> {
        let change_id = if let Some(ws) = self.completed_workspaces.remove(bead_id) {
            let message = format!("fix({bead_id}): agent work");
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
    pub(crate) async fn checkpoint_and_cleanup(&mut self, bead_id: &str) -> Option<String> {
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
                let bead_ref = crate::store::BeadRef {
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
            let _ =
                crate::workspace::merge_or_pr(&ws_repo_path, &branch, bead_id, &issue_type).await;
        }

        self.cleanup_workspace(bead_id);
        change_id
    }

    /// Clean up the workspace for a completed bead.
    /// Delegates to workspace.rs cleanup functions to avoid duplication.
    pub(crate) fn cleanup_workspace(&mut self, bead_id: &str) {
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
            // Legacy fallback — try both VCS types
            crate::workspace::cleanup_jj_workspace(std::path::Path::new("."), bead_id);
            crate::workspace::cleanup_git_worktree(std::path::Path::new("."), bead_id);
        }
    }

    /// Handle a verification failure. Returns true if deadlettered.
    pub(crate) fn on_fail(&mut self, bead_id: &str, summary: &VerifySummary) -> bool {
        let tracker = self
            .trackers
            .entry(bead_id.to_string())
            .or_insert(BeadTracker {
                repo: String::new(),
                last_generation: 0,
                retries: 0,
                consecutive_reverts: 0,
                highest_tier: None,
                current_agent: None,
                phase_index: 0,
            });

        // Check for revert (regression from previous best)
        if let (Some(prev), Some(curr)) = (tracker.highest_tier, summary.highest_passing_tier) {
            if curr < prev {
                tracker.consecutive_reverts += 1;
            } else {
                tracker.consecutive_reverts = 0;
            }
        }
        tracker.highest_tier = summary.highest_passing_tier;
        tracker.retries += 1;

        // Stopping conditions
        if tracker.retries >= self.config.max_retries {
            println!("[deadletter] {bead_id}: max retries ({})", tracker.retries);
            return true;
        }
        if tracker.consecutive_reverts >= 3 {
            println!(
                "[deadletter] {bead_id}: {} consecutive reverts",
                tracker.consecutive_reverts
            );
            return true;
        }

        // Schedule retry with backoff
        self.queue
            .record_backoff(bead_id, tracker.retries, Instant::now());
        if let Some((name, _)) = summary.first_failure() {
            println!(
                "[retry] {bead_id}: failed at tier '{name}', retry #{} scheduled",
                tracker.retries
            );
        }

        false
    }

    /// Handle agent exit failure (non-zero exit). Returns true if deadlettered.
    pub(crate) fn on_fail_exit(&mut self, bead_id: &str) -> bool {
        let tracker = self
            .trackers
            .entry(bead_id.to_string())
            .or_insert(BeadTracker {
                repo: String::new(),
                last_generation: 0,
                retries: 0,
                consecutive_reverts: 0,
                highest_tier: None,
                current_agent: None,
                phase_index: 0,
            });
        tracker.retries += 1;

        if tracker.retries >= self.config.max_retries {
            println!(
                "[deadletter] {bead_id}: max retries after exit failure ({})",
                tracker.retries
            );
            return true;
        }

        self.queue
            .record_backoff(bead_id, tracker.retries, Instant::now());
        println!(
            "[retry] {bead_id}: agent exited non-zero, retry #{} scheduled",
            tracker.retries
        );
        self.cleanup_workspace(bead_id);

        false
    }
}
