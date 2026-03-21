//! Auto-thread clustering and thread map building.
//!
//! Sentry span: `reconcile.threading`

use std::collections::HashMap;

use crate::bead::BeadState;
use crate::epic;

use super::Reconciler;

impl Reconciler {
    /// Cluster open beads into threads and persist to hierarchy store.
    /// Only runs when hierarchy store is available. Sequential and SharedScope
    /// clusters become threads; NearDuplicate and Overlapping are left for dedup.
    pub(super) async fn auto_thread(&mut self, beads: &[crate::bead::Bead]) {
        let Some(ref hierarchy) = self.hierarchy else {
            return;
        };

        let open_beads: Vec<&crate::bead::Bead> = beads
            .iter()
            .filter(|b| b.state() == BeadState::Open)
            .collect();
        let owned: Vec<crate::bead::Bead> = open_beads.iter().map(|b| (*b).clone()).collect();
        let clusters = epic::cluster_beads(&owned);

        for cluster in &clusters {
            let should_thread = matches!(
                cluster.relationship,
                epic::ClusterRelationship::Sequential | epic::ClusterRelationship::SharedScope
            );
            if !should_thread || cluster.bead_ids.len() < 2 {
                continue;
            }

            let thread_id = format!("auto/{}-{}", &cluster.bead_ids[0], &cluster.bead_ids[1]);

            // Check if any bead in the cluster already has a thread
            let mut already_threaded = false;
            for bid in &cluster.bead_ids {
                let bead_ref = crate::store::BeadRef {
                    repo: owned
                        .iter()
                        .find(|b| b.id == *bid)
                        .map(|b| b.repo.clone())
                        .unwrap_or_default(),
                    bead_id: bid.clone(),
                };
                if let Ok(Some(_)) = hierarchy.find_thread_for_bead(&bead_ref).await {
                    already_threaded = true;
                    break;
                }
            }
            if already_threaded {
                continue;
            }

            let thread = crate::store::ThreadRecord {
                id: thread_id.clone(),
                name: format!("{:?} cluster", cluster.relationship),
                decade_id: "auto-discovered".to_string(),
                feature_branch: None,
            };
            if let Err(e) = hierarchy.upsert_thread(&thread).await {
                eprintln!("[auto-thread] failed to create thread {thread_id}: {e}");
                continue;
            }
            for bid in &cluster.bead_ids {
                let bead_ref = crate::store::BeadRef {
                    repo: owned
                        .iter()
                        .find(|b| b.id == *bid)
                        .map(|b| b.repo.clone())
                        .unwrap_or_default(),
                    bead_id: bid.clone(),
                };
                let _ = hierarchy.add_bead_to_thread(&thread_id, &bead_ref).await;
            }
            eprintln!(
                "[auto-thread] created thread {thread_id} with {} beads ({:?})",
                cluster.bead_ids.len(),
                cluster.relationship
            );
        }
    }

    /// Pre-compute bead→thread mapping for triage.
    /// Done before triage to avoid async calls inside the triage loop
    /// (which would make iterate() non-Send due to AgentHandle borrows).
    pub(super) async fn build_thread_map(
        &self,
        beads: &[crate::bead::Bead],
    ) -> HashMap<String, String> {
        let Some(ref hierarchy) = self.hierarchy else {
            return HashMap::new();
        };

        let mut map = HashMap::new();
        for bead in beads {
            let bead_ref = crate::store::BeadRef {
                repo: bead.repo.clone(),
                bead_id: bead.id.clone(),
            };
            if let Ok(Some(thread_id)) = hierarchy.find_thread_for_bead(&bead_ref).await {
                map.insert(bead.id.clone(), thread_id);
            }
        }
        map
    }
}
