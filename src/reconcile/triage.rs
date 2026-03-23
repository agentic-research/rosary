//! Phase 3: Triage — score and filter beads for dispatch.
//!
//! Sentry span: `reconcile.triage`
//! Attributes: beads.scanned, beads.triaged, filter.reason

use std::collections::HashMap;
use std::time::Instant;

use crate::bead::BeadState;
use crate::epic;
use crate::queue::{self, QueueEntry};

use super::Reconciler;

impl Reconciler {
    /// Score open beads, apply filters, enqueue above threshold.
    /// If --bead is set, skip normal triage and only enqueue that bead.
    pub(super) fn triage(
        &mut self,
        beads: &[crate::bead::Bead],
        thread_map: &HashMap<String, String>,
    ) -> usize {
        let target_filter = self.config.target_bead.clone();
        let now = chrono::Utc::now();
        let mut triaged = 0;

        for bead in beads {
            if let Some(ref target) = target_filter {
                if bead.id != *target {
                    continue;
                }
                // Targeted dispatch: log if bead is in non-open state
                if bead.status != "open" {
                    eprintln!(
                        "[triage] targeted bead {target} is '{}', overriding",
                        bead.status
                    );
                }
            } else if bead.state() != BeadState::Open {
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

            // Golden Rule 12: implementation beads need refinement (5-whys)
            // before dispatch. Unrefined beads need a research pass first.
            if bead.needs_refinement() {
                eprintln!(
                    "[refinement] deferring {} — description too short, needs 5-whys (rule 12)",
                    bead.id
                );
                continue;
            }

            // Dependency-aware: hard-filter beads with unresolved deps.
            // Targeted dispatch (--bead) bypasses this — explicit override.
            if bead.is_blocked() && target_filter.is_none() {
                continue;
            }

            // Per-repo coordination: don't dispatch to a repo that
            // already has an active agent.
            let repo_busy = self.active.keys().any(|active_id| {
                self.trackers
                    .get(active_id)
                    .is_some_and(|t| t.repo == bead.repo)
            });
            if repo_busy {
                continue;
            }

            // Thread-aware sequencing: defer if a thread-mate is currently active.
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

            // File overlap: defer if candidate's files conflict with active/queued.
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

            // Self-managed repo preference: boost dogfooding beads.
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
                // UNLESS the bead has pending retries
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
                    triaged += 1;
                }
            }
        }

        triaged
    }
}
