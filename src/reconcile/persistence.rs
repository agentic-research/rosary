//! Bead persistence and status mirroring.
//!
//! Sentry span: `reconcile.persist`

use super::Reconciler;
use crate::scanner;
use crate::store::BeadStore;

impl Reconciler {
    /// Get or lazily connect a BeadStore for a repo.
    pub(super) async fn dolt_client(&mut self, repo: &str) -> Option<&dyn BeadStore> {
        if self.dolt_clients.contains_key(repo) {
            return self.dolt_clients.get(repo).map(|b| b.as_ref());
        }

        let (path, _) = self.repo_info.get(repo)?;
        let beads_dir = path.join(".beads");
        match crate::bead_sqlite::connect_bead_store(&beads_dir).await {
            Ok(store) => {
                self.dolt_clients.insert(repo.to_string(), store);
                self.dolt_clients.get(repo).map(|b| b.as_ref())
            }
            Err(e) => {
                eprintln!("[bead] failed to connect for {repo}: {e}");
                None
            }
        }
    }

    /// Update bead status in Dolt and log the transition. Best-effort.
    /// Also mirrors the transition to the external issue tracker (Linear)
    /// if the bead has an external_ref and a tracker is configured.
    pub(super) async fn persist_status(&mut self, bead_id: &str, repo: &str, status: &str) {
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

    /// Check if a bead was already closed by the dispatched agent via MCP.
    ///
    /// This is the "agent-first" fast path: when agents self-close beads,
    /// we skip the full verification pipeline (compile+test+lint+diff-sanity),
    /// which is the main consumption throughput bottleneck.
    #[allow(dead_code)] // Used when agents have bead_close permission
    pub(super) async fn is_bead_agent_closed(&mut self, bead_id: &str, repo: &str) -> bool {
        if let Some(client) = self.dolt_client(repo).await {
            match client.get_status(bead_id).await {
                Ok(Some(ref status)) if status == "closed" || status == "done" => {
                    eprintln!("[agent-closed] {bead_id} — skipping verification (agent-first)");
                    true
                }
                _ => false,
            }
        } else {
            false
        }
    }

    /// Append an observation for a bead (CRDT-lattice dual-write).
    ///
    /// This runs alongside persist_status — both the mutable cell AND the
    /// append-only observation are written. Once we validate the lattice
    /// produces identical status, persist_status can be removed.
    pub(super) async fn append_observation(
        &mut self,
        bead_id: &str,
        repo: &str,
        agent: &str,
        phase: u32,
        verdict: crate::dolt::observations::Verdict,
        detail: &str,
    ) {
        if let Some(client) = self.dolt_client(repo).await {
            let event_detail =
                format!("phase={phase} agent={agent} verdict={verdict:?} detail={detail}");
            client
                .log_event(bead_id, "observation", &event_detail)
                .await;
        }
    }

    /// Reset beads stuck at 'dispatched' from a previous run.
    /// On startup, any bead with status=dispatched has no running agent
    /// (the reconciler that dispatched it is dead). Reset to open.
    ///
    /// Also restores pipeline state: if a bead had progressed to phase 2
    /// before the crash, the tracker is rebuilt from persistent PipelineState
    /// so it resumes at the correct phase (not phase 0).
    pub(super) async fn recover_stuck_beads(&mut self) {
        // Load persistent pipeline state so we can restore phase progress.
        let active_pipelines = self.pipeline.list_active().await;
        let pipeline_map: std::collections::HashMap<String, crate::store::PipelineState> =
            active_pipelines
                .into_iter()
                .map(|ps| (ps.bead_ref.bead_id.clone(), ps))
                .collect();

        if !pipeline_map.is_empty() {
            eprintln!(
                "[recover] found {} active pipeline states from previous run",
                pipeline_map.len()
            );
        }

        let beads = match scanner::scan_repos(&self.config.repo).await {
            Ok(b) => b,
            Err(_) => return,
        };
        for bead in &beads {
            if bead.status == "dispatched" {
                // Restore tracker from persistent pipeline state if available.
                // This preserves phase progress across crashes — without it,
                // a bead at phase 2 (staging) would restart at phase 0 (scoping).
                if let Some(ps) = pipeline_map.get(&bead.id) {
                    eprintln!(
                        "[recover] restoring {} to phase {} ({})",
                        bead.id, ps.pipeline_phase, ps.pipeline_agent
                    );
                    self.trackers.insert(
                        bead.id.clone(),
                        super::BeadTracker {
                            repo: bead.repo.clone(),
                            last_generation: 0,
                            retries: ps.retries,
                            consecutive_reverts: ps.consecutive_reverts,
                            highest_tier: ps.highest_verify_tier.map(|t| t as usize),
                            current_agent: Some(ps.pipeline_agent.clone()),
                            phase_index: ps.pipeline_phase as u32,
                            issue_type: bead.issue_type.clone(),
                            dispatch_id: None,
                        },
                    );
                }

                eprintln!("[recover] resetting stuck bead {} to open", bead.id);
                self.persist_status(&bead.id, &bead.repo, "open").await;
            }
        }
    }
}
