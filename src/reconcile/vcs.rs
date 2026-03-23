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

            eprintln!("[vcs] {repo_name}: {bead_id} → {new_status} (jj change {change_id})");

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

    /// Poll beads in `pr_open` status — close when their PR merges.
    /// Uses `gh pr view` (works locally and in CI). Falls back gracefully
    /// if `gh` isn't available or the PR URL isn't recorded.
    pub(super) async fn poll_pr_merges(&mut self, beads: &[crate::bead::Bead]) -> usize {
        let pr_open_beads: Vec<&crate::bead::Bead> =
            beads.iter().filter(|b| b.status == "pr_open").collect();

        if pr_open_beads.is_empty() {
            return 0;
        }

        let mut closed = 0;
        for bead in &pr_open_beads {
            // Look up PR URL from events log
            let pr_url = if let Some(client) = self.dolt_client(&bead.repo).await {
                client
                    .get_latest_event(&bead.id, "pr_url")
                    .await
                    .ok()
                    .flatten()
            } else {
                None
            };

            let Some(url) = pr_url else {
                continue;
            };

            // Check PR state via `gh pr view`
            let output = std::process::Command::new("gh")
                .args(["pr", "view", &url, "--json", "state", "-q", ".state"])
                .output();

            let merged = match output {
                Ok(o) if o.status.success() => {
                    let state = String::from_utf8_lossy(&o.stdout);
                    state.trim() == "MERGED"
                }
                _ => continue, // gh not available or API error — skip
            };

            if merged {
                eprintln!("[pr-merged] {} — PR merged, closing bead", bead.id);
                self.persist_status(&bead.id, &bead.repo, "closed").await;

                // Now clear pipeline state
                let bead_ref = crate::store::BeadRef {
                    repo: bead.repo.clone(),
                    bead_id: bead.id.clone(),
                };
                self.pipeline.clear_state(&bead_ref).await;
                closed += 1;
            }
        }

        closed
    }
}
