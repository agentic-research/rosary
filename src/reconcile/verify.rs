//! Phase 5: Verification, pipeline decisions, and wait_and_verify sub-loop.
//!
//! Sentry span: `reconcile.verify`
//! Attributes: bead.id, action (advance/terminal/retry/deadletter), verify.tier_reached

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;

use crate::dispatch;
use crate::pipeline::CompletionAction;
use crate::store::BeadRef;

use super::Reconciler;

/// Result of Phase 5 verification pass.
pub(super) struct VerifyResult {
    pub status_updates: Vec<(String, String, String)>,
    pub phase_advances: Vec<(String, String, String)>,
    pub passed: usize,
    pub failed: usize,
    pub deadlettered: usize,
}

impl Reconciler {
    /// Phase 5: verify completed agents and decide pipeline action.
    /// Returns status updates and phase advances for the caller to persist.
    pub(super) async fn verify_completed(
        &mut self,
        completed: &[(String, bool)],
        beads: &[crate::bead::Bead],
        thread_map: &HashMap<String, String>,
    ) -> VerifyResult {
        let mut result = VerifyResult {
            status_updates: Vec::new(),
            phase_advances: Vec::new(),
            passed: 0,
            failed: 0,
            deadlettered: 0,
        };

        for (bead_id, exit_success) in completed {
            let (repo, issue_type, current_agent) = self
                .trackers
                .get(bead_id.as_str())
                .map(|t| {
                    (
                        t.repo.clone(),
                        t.issue_type.clone(),
                        t.current_agent.clone(),
                    )
                })
                .unwrap_or_else(|| {
                    beads
                        .iter()
                        .find(|b| b.id == *bead_id)
                        .map(|b| (b.repo.clone(), b.issue_type.clone(), b.owner.clone()))
                        .unwrap_or_default()
                });

            let retries = self
                .trackers
                .get(bead_id.as_str())
                .map(|t| t.retries)
                .unwrap_or(0);

            let (verify_passed, verify_summary) = if *exit_success {
                let vs = self.verify_agent(bead_id);
                match &vs {
                    Some(v) if v.passed() => (Some(true), vs),
                    Some(_) => (Some(false), vs),
                    None => (None, None),
                }
            } else {
                (Some(false), None)
            };

            let action = self.pipeline.decide(
                &issue_type,
                current_agent.as_deref(),
                *exit_success,
                verify_passed,
                retries,
                self.config.max_retries,
            );

            match action {
                CompletionAction::Advance { ref next_agent, .. } => {
                    result.passed += 1;
                    self.on_pass(bead_id);
                    self.checkpoint_workspace(bead_id).await;
                    result
                        .phase_advances
                        .push((bead_id.clone(), repo, next_agent.clone()));
                }
                CompletionAction::Terminal => {
                    result.passed += 1;
                    self.on_pass(bead_id);
                    // checkpoint_and_cleanup creates the PR — bead moves to
                    // pr_open, NOT closed. Actual close happens when PR merges
                    // (detected by poll_pr_merges in iterate).
                    self.checkpoint_and_cleanup(bead_id).await;
                    result
                        .status_updates
                        .push((bead_id.clone(), repo, "pr_open".into()));
                }
                CompletionAction::Retry => {
                    result.failed += 1;
                    if *exit_success {
                        if let Some(ref vs) = verify_summary {
                            self.on_fail(bead_id, vs);
                        }
                    } else {
                        self.completed_work_dirs.remove(bead_id);
                        self.on_fail_exit(bead_id);
                    }
                    result
                        .status_updates
                        .push((bead_id.clone(), repo, "open".into()));
                }
                CompletionAction::Deadletter => {
                    result.failed += 1;
                    result.deadlettered += 1;
                    if *exit_success {
                        if let Some(ref vs) = verify_summary {
                            self.on_fail(bead_id, vs);
                        }
                    } else {
                        self.completed_work_dirs.remove(bead_id);
                        self.on_fail_exit(bead_id);
                    }
                    self.cleanup_workspace(bead_id);
                    let bead_ref = BeadRef {
                        repo: repo.clone(),
                        bead_id: bead_id.clone(),
                    };
                    self.pipeline.clear_state(&bead_ref).await;
                    result
                        .status_updates
                        .push((bead_id.clone(), repo, "blocked".into()));
                }
            }
        }

        // Persist status transitions
        for (bead_id, repo, status) in &result.status_updates {
            self.persist_status(bead_id, repo, status).await;
        }

        // Phase advancement: write handoff, update owner, advance phase, reopen
        for (bead_id, repo, next_agent) in &result.phase_advances {
            self.advance_phase(bead_id, repo, next_agent, thread_map)
                .await;
        }

        result
    }

    /// Write handoff, update tracker, persist pipeline state, set assignee, reopen.
    async fn advance_phase(
        &mut self,
        bead_id: &str,
        repo: &str,
        next_agent: &str,
        thread_map: &HashMap<String, String>,
    ) {
        let (from_agent, phase) = self
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

        if let Some(ws) = self.completed_workspaces.get(bead_id) {
            let work = crate::manifest::Work::from_git(&ws.work_dir, None);
            let mut handoff = crate::handoff::Handoff::new(
                phase,
                &from_agent,
                Some(next_agent),
                bead_id,
                self.provider.name(),
                &work,
            );
            handoff.thread_id = thread_map.get(bead_id).cloned();
            if let Err(e) = handoff.write_to(&ws.work_dir) {
                eprintln!("[handoff] {bead_id}: failed to write phase handoff: {e}");
            }
        }

        if let Some(tracker) = self.trackers.get_mut(bead_id) {
            tracker.current_agent = Some(next_agent.to_string());
            tracker.phase_index = phase + 1;
        }

        let bead_ref = BeadRef {
            repo: repo.to_string(),
            bead_id: bead_id.to_string(),
        };
        self.pipeline
            .upsert_state(&crate::store::PipelineState {
                bead_ref,
                pipeline_phase: (phase + 1) as u8,
                pipeline_agent: next_agent.to_string(),
                phase_status: "pending".to_string(),
                retries: 0,
                consecutive_reverts: 0,
                highest_verify_tier: None,
                last_generation: 0,
                backoff_until: None,
            })
            .await;

        if let Some(client) = self.dolt_client(repo).await {
            client
                .log_event(
                    bead_id,
                    "phase_complete",
                    &format!("{from_agent} → {next_agent}"),
                )
                .await;
            if let Err(e) = client.set_assignee(bead_id, next_agent).await {
                eprintln!("[phase] failed to advance {bead_id}: {e}");
            } else {
                println!("[phase] {bead_id} → {next_agent} (phase {})", phase + 1);
            }
        }
        self.persist_status(bead_id, repo, "open").await;
    }

    /// Wait for all active agents to complete, then verify their work.
    /// Supports multi-stage pipelines: on phase advance, re-dispatches the
    /// next agent inline and continues the wait loop.
    pub(super) async fn wait_and_verify(&mut self) -> Result<()> {
        let poll_interval = Duration::from_secs(5);
        let timeout = Duration::from_secs(1800);
        let start = std::time::Instant::now();

        while !self.active.is_empty() {
            if start.elapsed() > timeout {
                eprintln!("[timeout] killing {} remaining agent(s)", self.active.len());
                let ids: Vec<String> = self.active.keys().cloned().collect();
                for id in &ids {
                    if let Some(handle) = self.active.get_mut(id) {
                        let _ = handle.kill();
                    }
                }
            }

            let completed = self.check_completed();
            if completed.is_empty() {
                tokio::time::sleep(poll_interval).await;
                continue;
            }

            for (bead_id, exit_success) in &completed {
                let (repo, issue_type, current_agent) = self
                    .trackers
                    .get(bead_id.as_str())
                    .map(|t| {
                        (
                            t.repo.clone(),
                            t.issue_type.clone(),
                            t.current_agent.clone(),
                        )
                    })
                    .unwrap_or_default();

                let retries = self
                    .trackers
                    .get(bead_id.as_str())
                    .map(|t| t.retries)
                    .unwrap_or(0);

                let (verify_passed, verify_summary) = if *exit_success {
                    let vs = self.verify_agent(bead_id);
                    match &vs {
                        Some(v) if v.passed() => (Some(true), vs),
                        Some(_) => (Some(false), vs),
                        None => (None, None),
                    }
                } else {
                    (Some(false), None)
                };

                let action = self.pipeline.decide(
                    &issue_type,
                    current_agent.as_deref(),
                    *exit_success,
                    verify_passed,
                    retries,
                    self.config.max_retries,
                );

                match action {
                    CompletionAction::Advance { ref next_agent, .. } => {
                        self.on_pass(bead_id);

                        let (from_agent, phase) = self
                            .trackers
                            .get(bead_id.as_str())
                            .map(|t| {
                                (
                                    t.current_agent
                                        .clone()
                                        .unwrap_or_else(|| "dev-agent".to_string()),
                                    t.phase_index,
                                )
                            })
                            .unwrap_or_else(|| ("dev-agent".to_string(), 0));

                        self.checkpoint_workspace(bead_id).await;

                        if let Some(ws) = self.completed_workspaces.get(bead_id.as_str()) {
                            let work = crate::manifest::Work::from_git(&ws.work_dir, None);
                            let handoff = crate::handoff::Handoff::new(
                                phase,
                                &from_agent,
                                Some(next_agent),
                                bead_id,
                                self.provider.name(),
                                &work,
                            );
                            if let Err(e) = handoff.write_to(&ws.work_dir) {
                                eprintln!(
                                    "[handoff] {bead_id}: failed to write phase handoff: {e}"
                                );
                            }
                        }

                        if let Some(tracker) = self.trackers.get_mut(bead_id.as_str()) {
                            tracker.current_agent = Some(next_agent.clone());
                            tracker.phase_index = phase + 1;
                        }

                        let bead_ref = BeadRef {
                            repo: repo.clone(),
                            bead_id: bead_id.clone(),
                        };
                        self.pipeline
                            .upsert_state(&crate::store::PipelineState {
                                bead_ref,
                                pipeline_phase: (phase + 1) as u8,
                                pipeline_agent: next_agent.clone(),
                                phase_status: "executing".to_string(),
                                retries: 0,
                                consecutive_reverts: 0,
                                highest_verify_tier: None,
                                last_generation: 0,
                                backoff_until: None,
                            })
                            .await;

                        if let Some(client) = self.dolt_client(&repo).await {
                            client
                                .log_event(
                                    bead_id,
                                    "phase_complete",
                                    &format!("{from_agent} → {next_agent}"),
                                )
                                .await;
                            if let Err(e) = client.set_assignee(bead_id, next_agent).await {
                                eprintln!("[phase] failed to advance {bead_id}: {e}");
                            } else {
                                println!("[phase] {bead_id} → {next_agent} (phase {})", phase + 1);
                            }
                        }
                        self.persist_status(bead_id, &repo, "open").await;

                        // Re-dispatch next agent inline
                        let path = self
                            .repo_info
                            .get(&repo)
                            .map(|(p, _)| p.clone())
                            .unwrap_or_default();
                        let mut dispatch_bead = crate::bead::Bead {
                            id: bead_id.clone(),
                            title: String::new(),
                            description: String::new(),
                            status: "dispatched".into(),
                            priority: 2,
                            issue_type: issue_type.clone(),
                            owner: Some(next_agent.clone()),
                            repo: repo.clone(),
                            created_at: chrono::Utc::now(),
                            updated_at: chrono::Utc::now(),
                            dependency_count: 0,
                            dependent_count: 0,
                            comment_count: 0,
                            branch: None,
                            pr_url: None,
                            jj_change_id: None,
                            external_ref: None,
                            files: Vec::new(),
                            test_files: Vec::new(),
                        };
                        if let Some(client) = self.dolt_client(&repo).await
                            && let Ok(Some(full)) = client.get_bead(bead_id, &repo).await
                        {
                            dispatch_bead.title = full.title;
                            dispatch_bead.description = full.description;
                            dispatch_bead.files = full.files;
                            dispatch_bead.test_files = full.test_files;
                        }
                        dispatch_bead.owner = Some(next_agent.clone());

                        match dispatch::spawn(
                            &dispatch_bead,
                            &path,
                            true,
                            0,
                            self.provider.as_ref(),
                            self.agents_dir.as_deref(),
                        )
                        .await
                        {
                            Ok(handle) => {
                                println!("[dispatch] {bead_id} phase {} → {next_agent}", phase + 1);
                                self.persist_status(bead_id, &repo, "dispatched").await;
                                self.active.insert(bead_id.clone(), handle);
                            }
                            Err(e) => {
                                eprintln!(
                                    "[dispatch] failed to re-dispatch {bead_id} for {next_agent}: {e}"
                                );
                            }
                        }
                    }
                    CompletionAction::Terminal => {
                        self.on_pass(bead_id);
                        self.checkpoint_and_cleanup(bead_id).await;
                        // Don't close — wait for PR merge
                        self.persist_status(bead_id, &repo, "pr_open").await;
                    }
                    CompletionAction::Retry => {
                        if *exit_success {
                            if let Some(ref vs) = verify_summary {
                                self.on_fail(bead_id, vs);
                            }
                        } else {
                            self.completed_work_dirs.remove(bead_id);
                            self.on_fail_exit(bead_id);
                        }
                        self.persist_status(bead_id, &repo, "open").await;
                    }
                    CompletionAction::Deadletter => {
                        if *exit_success {
                            if let Some(ref vs) = verify_summary {
                                self.on_fail(bead_id, vs);
                            }
                        } else {
                            self.completed_work_dirs.remove(bead_id);
                            self.on_fail_exit(bead_id);
                        }
                        self.cleanup_workspace(bead_id);
                        let bead_ref = BeadRef {
                            repo: repo.clone(),
                            bead_id: bead_id.clone(),
                        };
                        self.pipeline.clear_state(&bead_ref).await;
                        self.persist_status(bead_id, &repo, "blocked").await;
                    }
                }
            }
        }

        Ok(())
    }
}
