//! VCS scanning — detect bead references in recent commits.
//!
//! Sentry span: `reconcile.vcs_scan`

use std::path::PathBuf;

use super::Reconciler;

impl Reconciler {
    /// Scan jj logs across repos for bead references in commit messages.
    /// Triggers state transitions: open → dispatched (for refs), open → done (for closes).
    /// Returns the number of transitions triggered.
    pub(super) async fn scan_vcs(&mut self, beads: &[crate::bead::Bead]) -> usize {
        use crate::vcs;

        // Collect repo info first to avoid borrow conflicts with &mut self
        let repos: Vec<(String, PathBuf)> = self
            .repo_info
            .iter()
            .map(|(name, (path, _))| (name.clone(), path.clone()))
            .collect();

        // Gather all VCS refs across repos
        let mut pending: Vec<(String, String, String, String, bool)> = Vec::new(); // (repo, bead_id, bead_repo, change_id, closes)
        for (repo_name, repo_path) in &repos {
            let vcs_refs = match vcs::scan_vcs_bead_refs(repo_path) {
                Ok(refs) => refs,
                Err(_) => continue,
            };

            for (change_id, bead_ref) in &vcs_refs {
                let bead = beads.iter().find(|b| b.id == bead_ref.id);
                let Some(bead) = bead else { continue };

                // Determine target status
                let should_transition = if bead_ref.closes {
                    !matches!(bead.status.as_str(), "done" | "closed")
                } else {
                    bead.status.as_str() == "open"
                };

                if !should_transition || self.active.contains_key(&bead.id) {
                    continue;
                }

                pending.push((
                    repo_name.clone(),
                    bead.id.clone(),
                    bead.repo.clone(),
                    change_id.clone(),
                    bead_ref.closes,
                ));
            }
        }

        // Apply transitions
        let mut transitions = 0;
        for (repo_name, bead_id, bead_repo, change_id, closes) in &pending {
            let new_status = if *closes { "closed" } else { "dispatched" };

            println!("[vcs] {repo_name}: {bead_id} → {new_status} (jj change {change_id})");

            if let Some(client) = self.dolt_client(bead_repo).await {
                let _ = client
                    .log_event(bead_id, "vcs_ref", &format!("jj:{change_id}"))
                    .await;
            }

            self.persist_status(bead_id, bead_repo, new_status).await;
            transitions += 1;
        }

        transitions
    }
}
