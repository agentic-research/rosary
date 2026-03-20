//! Triage logic: scoring, filtering, dedup, file overlap, dependency checks.

use std::collections::HashMap;
use std::time::Instant;

use crate::bead::BeadState;
use crate::epic;
use crate::queue::{self, QueueEntry};

use super::{IterationSummary, Reconciler};

impl Reconciler {
    /// Phase 2.5 + 2.75: Auto-thread open beads and build bead→thread map.
    ///
    /// Clusters open beads and persists Sequential/SharedScope clusters as threads.
    /// Then pre-computes bead→thread mapping for triage (avoids async in triage loop).
    pub(crate) async fn auto_thread_and_build_map(
        &mut self,
        beads: &[crate::bead::Bead],
    ) -> HashMap<String, String> {
        // Phase 2.5: AUTO-THREAD — cluster open beads and persist as threads.
        if let Some(ref hierarchy) = self.hierarchy {
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

                // Generate a thread ID from the first two bead IDs
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

                // Create thread and assign beads
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

        // Phase 2.75: BUILD THREAD MAP — pre-compute bead→thread for triage.
        // Done before triage to avoid async calls inside the triage loop
        // (which would make iterate() non-Send due to AgentHandle borrows).
        if let Some(ref hierarchy) = self.hierarchy {
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
        } else {
            HashMap::new()
        }
    }

    /// Phase 3: TRIAGE — score open beads, enqueue above threshold.
    pub(crate) fn triage(
        &mut self,
        beads: &[crate::bead::Bead],
        thread_map: &HashMap<String, String>,
        summary: &mut IterationSummary,
    ) {
        let now = chrono::Utc::now();
        for bead in beads {
            if bead.state() != BeadState::Open {
                continue;
            }
            if self.active.contains_key(&bead.id) {
                continue;
            }
            if self.queue.is_deadlettered(&bead.id) {
                continue;
            }

            // Severity floor: skip beads below minimum priority level
            if !queue::passes_severity_floor(bead, self.queue.min_priority) {
                continue;
            }

            // Skip epics — they're planning beads, not actionable work
            if bead.issue_type == "epic" {
                continue;
            }

            // (Smart triage) Dependency-aware: hard-filter beads with unresolved deps.
            if bead.is_blocked() {
                continue;
            }

            // (Smart triage) Per-repo coordination: don't dispatch to a repo that
            // already has an active agent.
            let repo_busy = self.active.keys().any(|active_id| {
                self.trackers
                    .get(active_id)
                    .is_some_and(|t| t.repo == bead.repo)
            });
            if repo_busy {
                continue;
            }

            // Thread-aware sequencing: same-thread beads are sequential work,
            // not duplicates. Defer if a thread-mate is currently active.
            if let Some(thread_id) = thread_map.get(&bead.id) {
                let thread_mate_active = self
                    .active
                    .keys()
                    .any(|active_id| thread_map.get(active_id).is_some_and(|at| at == thread_id));
                if thread_mate_active {
                    eprintln!(
                        "[thread] deferring {} — thread-mate active (thread {thread_id})",
                        bead.id
                    );
                    continue;
                }
            }

            // Dedup: skip if semantically dominated by an active or queued bead.
            let active_beads: Vec<&crate::bead::Bead> = beads
                .iter()
                .filter(|other| other.id != bead.id)
                .filter(|other| {
                    self.active.contains_key(&other.id) || self.queue.contains(&other.id)
                })
                .collect();
            if let Some(dominator) = epic::is_dominated_by(bead, &active_beads) {
                eprintln!(
                    "[dedup] skipping {} — too similar to active {dominator}",
                    bead.id
                );
                continue;
            }

            // File overlap: defer if candidate's files conflict with an active/queued bead.
            if let Some(blocker) = epic::has_file_overlap(bead, &active_beads) {
                eprintln!(
                    "[file-overlap] deferring {} — files conflict with active {blocker}",
                    bead.id
                );
                continue;
            }

            let retries = self.queue.retries(&bead.id);
            let mut score = if self.config.overnight {
                queue::triage_score_overnight(bead, retries, now)
            } else {
                queue::triage_score(bead, retries, now)
            };

            // (Smart triage) Self-managed repo preference: boost beads from the
            // rosary repo itself so dogfooding work gets dispatched first.
            if self
                .config
                .repo
                .iter()
                .any(|r| r.name == bead.repo && r.self_managed)
            {
                score = (score + 0.15).min(1.0);
            }

            if score >= self.config.triage_threshold {
                let bead_gen = bead.generation();

                // Skip if already processed at this generation —
                // UNLESS the bead has pending retries (failed dispatch needs re-triage)
                if let Some(tracker) = self.trackers.get(&bead.id)
                    && tracker.last_generation == bead_gen
                    && tracker.retries == 0
                {
                    continue;
                }

                let enqueued = self.queue.enqueue(QueueEntry {
                    bead_id: bead.id.clone(),
                    repo: bead.repo.clone(),
                    score,
                    enqueued_at: Instant::now(),
                    retries,
                    generation: bead_gen,
                });
                if enqueued {
                    summary.triaged += 1;
                }
            }
        }
    }
}
