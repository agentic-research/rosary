//! Agent verification: polling, verification tiers, wait-and-verify sub-loop.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;

use crate::dispatch;
use crate::verify::Verifier;

use super::{IterationSummary, Reconciler};

impl Reconciler {
    /// Check if a bead was already closed by the dispatched agent via MCP.
    ///
    /// This is the "agent-first" fast path: when agents self-close beads,
    /// we skip the full verification pipeline (compile+test+lint+diff-sanity),
    /// which is the main consumption throughput bottleneck.
    pub(crate) async fn is_bead_agent_closed(&mut self, bead_id: &str, repo: &str) -> bool {
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

    /// Poll active agents for completion. Returns vec of (bead_id, exit_success).
    pub(crate) fn check_completed(&mut self) -> Vec<(String, bool)> {
        let mut completed = Vec::new();

        let bead_ids: Vec<String> = self.active.keys().cloned().collect();
        for bead_id in bead_ids {
            let handle = self.active.get_mut(&bead_id).unwrap();
            let mut done = false;
            let mut success = false;

            match handle.try_wait() {
                Ok(Some(ok)) => {
                    done = true;
                    success = ok;
                }
                Ok(None) => {
                    // Check timeout (10 min default)
                    if handle.elapsed() > chrono::Duration::minutes(10) {
                        eprintln!("[timeout] killing agent for {bead_id}");
                        let _ = handle.kill();
                        done = true;
                    }
                }
                Err(e) => {
                    eprintln!("[error] polling agent for {bead_id}: {e}");
                    done = true;
                }
            }

            if done {
                let mut handle = self.active.remove(&bead_id).unwrap();
                let repo = self
                    .trackers
                    .get(&bead_id)
                    .map(|t| t.repo.clone())
                    .unwrap_or_default();
                // Stash workspace for checkpoint + teardown
                if let Some(ws) = handle.workspace.take() {
                    self.completed_workspaces.insert(bead_id.clone(), ws);
                }
                self.completed_work_dirs
                    .insert(bead_id.clone(), (handle.work_dir, repo));
                completed.push((bead_id, success));
            }
        }

        completed
    }

    /// Run verification tiers on an agent's work directory.
    pub(crate) fn verify_agent(&mut self, bead_id: &str) -> Option<crate::verify::VerifySummary> {
        let (work_dir, repo) = self.completed_work_dirs.remove(bead_id)?;

        // Look up language for this repo
        let lang = self
            .repo_info
            .get(&repo)
            .map(|(_, l)| l.as_str())
            .unwrap_or("unknown");

        let verifier = Verifier::for_language(lang);
        match verifier.run(&work_dir) {
            Ok(summary) => {
                println!(
                    "[verify] {bead_id}: {} (highest_tier={:?})",
                    if summary.passed() { "PASS" } else { "FAIL" },
                    summary.highest_passing_tier,
                );
                Some(summary)
            }
            Err(e) => {
                eprintln!("[verify] {bead_id}: error running verification: {e}");
                None
            }
        }
    }

    /// Phase 5: VERIFY completed agents.
    /// Runs after dispatch so new agents execute in parallel with verification.
    pub(crate) async fn verify_completed(
        &mut self,
        beads: &[crate::bead::Bead],
        thread_map: &HashMap<String, String>,
        completed: Vec<(String, bool)>,
        summary: &mut IterationSummary,
    ) {
        let mut status_updates: Vec<(String, String, String)> = Vec::new();
        let mut phase_advances: Vec<(String, String, String)> = Vec::new();

        for (bead_id, exit_success) in &completed {
            let repo = self
                .trackers
                .get(bead_id.as_str())
                .map(|t| t.repo.clone())
                .unwrap_or_default();

            let bead_info = beads
                .iter()
                .find(|b| b.id == *bead_id)
                .map(|b| (b.issue_type.clone(), b.owner.clone()));

            // Agent-first fast path: if the agent already closed the bead via
            // MCP, skip verification entirely.
            if self.is_bead_agent_closed(bead_id, &repo).await {
                self.completed_work_dirs.remove(bead_id);
                summary.agent_closed += 1;
                summary.passed += 1;
                self.on_pass(bead_id);

                if let Some((ref issue_type, Some(ref current_agent))) = bead_info
                    && let Some(next) = dispatch::next_agent(issue_type, current_agent)
                {
                    // Keep workspace for next pipeline phase
                    self.checkpoint_workspace(bead_id).await;
                    phase_advances.push((bead_id.clone(), repo.clone(), next.to_string()));
                } else {
                    self.checkpoint_and_cleanup(bead_id).await;
                }
                continue;
            }

            if *exit_success {
                let verify_result = self.verify_agent(bead_id);
                match verify_result {
                    Some(vs) if vs.passed() => {
                        summary.passed += 1;
                        self.on_pass(bead_id);

                        if let Some((ref issue_type, Some(ref current_agent))) = bead_info {
                            if let Some(next) = dispatch::next_agent(issue_type, current_agent) {
                                self.checkpoint_workspace(bead_id).await;
                                phase_advances.push((
                                    bead_id.clone(),
                                    repo.clone(),
                                    next.to_string(),
                                ));
                            } else {
                                self.checkpoint_and_cleanup(bead_id).await;
                                status_updates.push((bead_id.clone(), repo, "closed".into()));
                            }
                        } else {
                            self.checkpoint_and_cleanup(bead_id).await;
                            status_updates.push((bead_id.clone(), repo, "closed".into()));
                        }
                    }
                    Some(vs) => {
                        summary.failed += 1;
                        let deadlettered = self.on_fail(bead_id, &vs);
                        if deadlettered {
                            summary.deadlettered += 1;
                            self.cleanup_workspace(bead_id);
                            status_updates.push((bead_id.clone(), repo, "blocked".into()));
                        } else {
                            status_updates.push((bead_id.clone(), repo, "open".into()));
                        }
                    }
                    None => {
                        summary.passed += 1;
                        self.on_pass(bead_id);

                        if let Some((ref issue_type, Some(ref current_agent))) = bead_info {
                            if let Some(next) = dispatch::next_agent(issue_type, current_agent) {
                                self.checkpoint_workspace(bead_id).await;
                                phase_advances.push((
                                    bead_id.clone(),
                                    repo.clone(),
                                    next.to_string(),
                                ));
                            } else {
                                self.checkpoint_and_cleanup(bead_id).await;
                                status_updates.push((bead_id.clone(), repo, "closed".into()));
                            }
                        } else {
                            self.checkpoint_and_cleanup(bead_id).await;
                            status_updates.push((bead_id.clone(), repo, "closed".into()));
                        }
                    }
                }
            } else {
                self.completed_work_dirs.remove(bead_id);
                summary.failed += 1;
                let deadlettered = self.on_fail_exit(bead_id);
                if deadlettered {
                    summary.deadlettered += 1;
                    self.cleanup_workspace(bead_id);
                    status_updates.push((bead_id.clone(), repo, "blocked".into()));
                } else {
                    status_updates.push((bead_id.clone(), repo, "open".into()));
                }
            }
        }

        // Persist state transitions to Dolt (best-effort)
        for (bead_id, repo, status) in &status_updates {
            self.persist_status(bead_id, repo, status).await;
        }

        // Phase advancement: write handoff, update owner, advance phase, reopen
        for (bead_id, repo, next_agent) in &phase_advances {
            // Write handoff to workspace so the next agent has context
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

            if let Some(ws) = self.completed_workspaces.get(bead_id.as_str()) {
                let work = crate::manifest::Work::from_git(&ws.work_dir, None);
                let mut handoff = crate::handoff::Handoff::new(
                    phase,
                    &from_agent,
                    Some(next_agent),
                    bead_id,
                    self.provider.name(),
                    &work,
                );
                handoff.thread_id = thread_map.get(bead_id.as_str()).cloned();
                if let Err(e) = handoff.write_to(&ws.work_dir) {
                    eprintln!("[handoff] {bead_id}: failed to write phase handoff: {e}");
                }
            }

            // Advance tracker state for next phase
            if let Some(tracker) = self.trackers.get_mut(bead_id.as_str()) {
                tracker.current_agent = Some(next_agent.clone());
                tracker.phase_index = phase + 1;
            }

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
    }

    /// Wait for all active agents to complete, then verify their work.
    ///
    /// This is the "sub-loop" that closes the dispatch cycle: poll agents
    /// every 5 seconds until all finish, run verification, update bead status.
    pub(crate) async fn wait_and_verify(&mut self) -> Result<()> {
        let poll_interval = Duration::from_secs(5);
        let timeout = Duration::from_secs(1800); // 30 min max
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

            // Verify + update status for each completed agent
            for (bead_id, exit_success) in &completed {
                let repo = self
                    .trackers
                    .get(bead_id.as_str())
                    .map(|t| t.repo.clone())
                    .unwrap_or_default();

                // Agent-first: skip verification if agent already closed the bead
                if self.is_bead_agent_closed(bead_id, &repo).await {
                    self.completed_work_dirs.remove(bead_id);
                    self.on_pass(bead_id);
                    self.checkpoint_and_cleanup(bead_id).await;
                    continue;
                }

                if *exit_success {
                    let verify_result = self.verify_agent(bead_id);
                    match verify_result {
                        Some(vs) if vs.passed() => {
                            self.on_pass(bead_id);
                            self.checkpoint_and_cleanup(bead_id).await;
                            self.persist_status(bead_id, &repo, "closed").await;
                        }
                        Some(vs) => {
                            self.on_fail(bead_id, &vs);
                            self.persist_status(bead_id, &repo, "open").await;
                        }
                        None => {
                            // No verifier — treat as pass
                            self.on_pass(bead_id);
                            self.checkpoint_and_cleanup(bead_id).await;
                            self.persist_status(bead_id, &repo, "closed").await;
                        }
                    }
                } else {
                    self.completed_work_dirs.remove(bead_id);
                    self.on_fail_exit(bead_id);
                    self.persist_status(bead_id, &repo, "open").await;
                }
            }
        }

        Ok(())
    }
}
