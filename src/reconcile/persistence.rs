//! Dolt persistence and status mirroring.
//!
//! Sentry span: `reconcile.persist`

use super::Reconciler;
use crate::dolt::{DoltClient, DoltConfig};
use crate::scanner;

impl Reconciler {
    /// Get or lazily connect a DoltClient for a repo.
    pub(super) async fn dolt_client(&mut self, repo: &str) -> Option<&DoltClient> {
        if self.dolt_clients.contains_key(repo) {
            return self.dolt_clients.get(repo);
        }

        let (path, _) = self.repo_info.get(repo)?;
        let beads_dir = path.join(".beads");
        let config = DoltConfig::from_beads_dir(&beads_dir).ok()?;
        match DoltClient::connect(&config).await {
            Ok(client) => {
                self.dolt_clients.insert(repo.to_string(), client);
                self.dolt_clients.get(repo)
            }
            Err(e) => {
                eprintln!("[dolt] failed to connect for {repo}: {e}");
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
                    println!("[agent-closed] {bead_id} — skipping verification (agent-first)");
                    true
                }
                _ => false,
            }
        } else {
            false
        }
    }

    /// Reset beads stuck at 'dispatched' from a previous run.
    /// On startup, any bead with status=dispatched has no running agent
    /// (the reconciler that dispatched it is dead). Reset to open.
    pub(super) async fn recover_stuck_beads(&mut self) {
        let beads = match scanner::scan_repos(&self.config.repo).await {
            Ok(b) => b,
            Err(_) => return,
        };
        for bead in &beads {
            if bead.status == "dispatched" {
                eprintln!("[recover] resetting stuck bead {} to open", bead.id);
                self.persist_status(&bead.id, &bead.repo, "open").await;
            }
        }
    }
}
