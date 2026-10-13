//! Phase 5: Verification, pipeline decisions, and wait_and_verify sub-loop.
//!
//! Sentry span: `reconcile.verify`
//! Attributes: bead.id, action (advance/terminal/retry/deadletter), verify.tier_reached

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;

use crate::dispatch;
use crate::dispatch::provenance::{FailureClass, classify};
use crate::dolt::observations::Verdict;
use crate::pipeline::CompletionAction;
use crate::store::WorkRef;
use crate::verify::VerifySummary;

use super::Reconciler;

/// Result of Phase 5 verification pass.
pub(super) struct VerifyResult {
    pub status_updates: Vec<(String, String, String)>,
    pub phase_advances: Vec<(String, String, String)>,
    pub passed: usize,
    pub failed: usize,
    pub deadlettered: usize,
    /// IDs of beads that deadlettered on THIS (verification) path — distinct
    /// from the count so the targeted-run exit can tell whether the *target*
    /// bead itself deadlettered, not just some sibling (rosary-5361f4). Without
    /// this the exit never fires and a bead that can't pass verify is
    /// re-dispatched forever.
    pub deadlettered_ids: Vec<String>,
    /// Beads already closed by the agent via MCP — verification skipped.
    pub agent_closed: usize,
}

/// Outcome of executing a single CompletionAction.
enum ActionOutcome {
    /// Pipeline advanced to next agent — caller decides whether to dispatch inline or defer.
    Advanced { next_agent: String },
    /// Pipeline complete — PR created, awaiting merge.
    Completed,
    /// Verification or exit failed — bead reopened for retry.
    Retrying,
    /// Max retries exceeded — bead blocked.
    Deadlettered,
}

impl Reconciler {
    // ── Shared decision logic ───────────────────────────────────────────

    /// Verify an agent's work and compute the pipeline decision.
    /// Returns (action, verify_summary) for the caller to execute.
    pub(super) fn verify_and_decide(
        &mut self,
        bead_id: &str,
        exit_success: bool,
        beads: &[crate::bead::Bead],
    ) -> (CompletionAction, Option<VerifySummary>) {
        let (_, issue_type, current_agent) = self
            .trackers
            .get(bead_id)
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
                    .find(|b| b.id == bead_id)
                    .map(|b| (b.repo.clone(), b.issue_type.clone(), b.owner.clone()))
                    .unwrap_or_default()
            });

        let retries = self.trackers.get(bead_id).map(|t| t.retries).unwrap_or(0);
        let consecutive_reverts = self
            .trackers
            .get(bead_id)
            .map(|t| t.consecutive_reverts)
            .unwrap_or(0);

        let close_condition = beads
            .iter()
            .find(|b| b.id == bead_id)
            .map(close_condition_text);

        let bead = beads.iter().find(|b| b.id == bead_id);
        let (verify_passed, verify_summary) = if exit_success {
            let vs = self.verify_agent(bead_id, close_condition.as_deref(), bead);
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
            exit_success,
            verify_passed,
            retries,
            self.config.max_retries,
            consecutive_reverts,
        );

        (action, verify_summary)
    }

    /// Execute a CompletionAction — the shared match logic used by both
    /// verify_completed (iterate mode) and wait_and_verify (targeted mode).
    ///
    /// Handles: on_pass/on_fail, workspace checkpoint/cleanup, pipeline state,
    /// handoff writing, tracker updates.
    async fn execute_action(
        &mut self,
        bead_id: &str,
        repo: &str,
        action: &CompletionAction,
        failure_class: FailureClass,
        verify_summary: Option<&VerifySummary>,
        thread_map: &HashMap<String, String>,
    ) -> ActionOutcome {
        // `classify_completion` returns Ok exactly when the run succeeded: a
        // failed exit is never Ok, and a downgraded zero exit is SessionLimit.
        let exit_success = failure_class == FailureClass::Ok;
        // Resolve agent name and phase for the observation.
        let (agent, phase) = self
            .trackers
            .get(bead_id)
            .map(|t| {
                (
                    t.current_agent
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string()),
                    t.phase_index,
                )
            })
            .unwrap_or_else(|| ("unknown".to_string(), 0));

        match action {
            CompletionAction::Advance { next_agent, .. } => {
                self.on_pass(bead_id);
                self.checkpoint_workspace(bead_id).await;
                self.append_observation(
                    bead_id,
                    repo,
                    &agent,
                    phase,
                    Verdict::Pass,
                    "phase passed",
                )
                .await;
                self.write_handoff_and_advance(bead_id, repo, next_agent, thread_map)
                    .await;
                ActionOutcome::Advanced {
                    next_agent: next_agent.clone(),
                }
            }
            CompletionAction::Terminal => {
                self.on_pass(bead_id);
                self.checkpoint_and_cleanup(bead_id).await;
                self.append_observation(
                    bead_id,
                    repo,
                    &agent,
                    phase,
                    Verdict::PrOpen,
                    "pipeline terminal",
                )
                .await;
                self.persist_status(bead_id, repo, crate::bead::BeadState::PrOpen)
                    .await;
                ActionOutcome::Completed
            }
            CompletionAction::Retry => {
                let detail = if exit_success {
                    "verify failed".to_string()
                } else {
                    failed_run_detail(failure_class)
                };
                self.handle_failure(bead_id, exit_success, verify_summary, true);
                self.append_observation(bead_id, repo, &agent, phase, Verdict::Fail, &detail)
                    .await;
                self.persist_status(bead_id, repo, crate::bead::BeadState::Open)
                    .await;
                ActionOutcome::Retrying
            }
            CompletionAction::Deadletter => {
                // "retry budget exhausted" not "max retries": decide() deadletters
                // on EITHER retries >= max_retries OR the consecutive-reverts
                // threshold, and the observation can't tell which fired.
                let detail = if exit_success {
                    "verify failed, retry budget exhausted".to_string()
                } else {
                    format!(
                        "{}, retry budget exhausted",
                        failed_run_detail(failure_class)
                    )
                };
                self.handle_failure(bead_id, exit_success, verify_summary, false);
                self.append_observation(bead_id, repo, &agent, phase, Verdict::Deadletter, &detail)
                    .await;
                self.cleanup_workspace(bead_id);
                let bead_ref = WorkRef {
                    repo: repo.to_string(),
                    scope: self
                        .trackers
                        .get(bead_id)
                        .map(|t| t.scope.clone())
                        .unwrap_or_default(),
                    bead_id: bead_id.to_string(),
                };
                self.pipeline.clear_state(&bead_ref).await;
                self.deadlettered.insert(bead_id.to_string());
                self.persist_status(bead_id, repo, crate::bead::BeadState::DeadLetter)
                    .await;
                ActionOutcome::Deadlettered
            }
        }
    }

    /// Record a failure's bookkeeping — shared between both verify paths.
    /// `schedule_retry` distinguishes Retry (backoff queued) from Deadletter
    /// (terminal — no backoff). The retry/deadletter DECISION itself lives in
    /// `pipeline.decide()` alone (rosary-45b069).
    fn handle_failure(
        &mut self,
        bead_id: &str,
        exit_success: bool,
        verify_summary: Option<&VerifySummary>,
        schedule_retry: bool,
    ) {
        if exit_success {
            if let Some(vs) = verify_summary {
                self.on_fail(bead_id, vs, schedule_retry);
            }
        } else {
            self.completed_work_dirs.remove(bead_id);
            self.on_fail_exit(bead_id, schedule_retry);
        }
    }

    /// Classify *why* a dispatch failed by reading the tail of its stderr log,
    /// so the recorded outcome (`failure:auth`, `failure:skew`, …) is the
    /// diagnosis instead of an absence. Only process-level failures
    /// (`exit_success == false`) are classified — a verify-tier failure exited 0
    /// and carries no spawn/auth signature. Must be called *before*
    /// `execute_action`, which drops `completed_work_dirs` on failure
    /// (`handle_failure` above). Best-effort: an unreadable log yields
    /// [`FailureClass::Unknown`], which qualifies to a bare base outcome.
    /// Classify a completion from everything the run left behind — the stderr
    /// tail AND the stream log tail — regardless of the exit code, so the
    /// recorded outcome (`failure:auth`, `failure:session-limit`, …) is the
    /// diagnosis instead of an absence. Returns `(class, effective_exit_success,
    /// wait_before_retry)`: a session-limit hit exits 0 with a "successful"
    /// result (rosary-1780ea), so its exit is downgraded here and it never
    /// reaches verify; the wait is present only when the notice names a reset
    /// time. Must be called *before* `execute_action`, which drops
    /// `completed_work_dirs` on failure. Best-effort: unreadable logs yield
    /// [`FailureClass::Unknown`] / [`FailureClass::Ok`].
    pub(super) fn classify_completion(
        &self,
        bead_id: &str,
        exit_success: bool,
    ) -> (FailureClass, bool, Option<Duration>) {
        let (stderr, result) = self
            .completed_work_dirs
            .get(bead_id)
            .map(|(work_dir, _repo)| {
                (
                    read_log_tail(work_dir, dispatch::STDERR_LOG_FILENAME),
                    stream_result_text(&read_log_tail(work_dir, dispatch::STREAM_LOG_FILENAME)),
                )
            })
            .unwrap_or_default();
        // The final `result` is the agent's own prose on a good run, so only a
        // result that IS the limit notice counts — never one that mentions it.
        // A failed exit's stderr is the harness talking, not the agent, and
        // keeps the substring classifier.
        let notice = if crate::dispatch::provenance::is_session_limit_notice(&result) {
            Some(result.as_str())
        } else if !exit_success && classify(false, &stderr) == FailureClass::SessionLimit {
            Some(stderr.as_str())
        } else {
            None
        };
        if let Some(text) = notice {
            let class = FailureClass::SessionLimit;
            let wait = crate::dispatch::provenance::session_limit_reset(text, chrono::Local::now());
            eprintln!(
                "[limit] {bead_id}: the seat's session limit was hit — not verifying; {}",
                match wait {
                    Some(w) => format!("next attempt after the reset (~{} min)", w.as_secs() / 60),
                    None => "no reset time in the notice, exponential backoff".to_string(),
                }
            );
            return (class, false, wait);
        }
        // A clean exit that isn't a limit is a success, whatever words its
        // stderr carries (a benign "timeout" warning is not `success:timeout`).
        if exit_success {
            return (FailureClass::Ok, true, None);
        }
        (classify(false, &stderr), false, None)
    }

    /// Write handoff, update tracker, persist pipeline state, set assignee, reopen.
    /// Shared by verify_completed (via execute_action) and wait_and_verify.
    async fn write_handoff_and_advance(
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
            // Read previous handoff for content-linked chain hash.
            // For phase > 0, missing previous handoff means the chain is broken —
            // skip handoff write entirely to avoid creating an unlinked attestation.
            let previous = if phase > 0 {
                match crate::handoff::Handoff::read_from(&ws.work_dir, phase - 1) {
                    Ok(h) => Some(h),
                    Err(e) => {
                        eprintln!(
                            "[handoff] {bead_id}: phase {phase} cannot read previous handoff, \
                             skipping handoff write to preserve chain integrity: {e}"
                        );
                        return;
                    }
                }
            } else {
                None
            };
            let mut handoff = crate::handoff::Handoff::new(
                phase,
                &from_agent,
                Some(next_agent),
                bead_id,
                self.provider.name(),
                &work,
                previous.as_ref(),
            );
            // Always set thread_id (was missing in wait_and_verify before this refactor)
            handoff.thread_id = thread_map.get(bead_id).cloned();
            // APAS L2: emit DSSE envelope only after a successful handoff write.
            match handoff.write_to(&ws.work_dir) {
                Ok(handoff_path) => {
                    self.write_handoff_envelope(bead_id, &handoff_path, phase, &handoff);
                }
                Err(e) => {
                    eprintln!("[handoff] {bead_id}: failed to write phase handoff: {e}");
                }
            }
        }

        if let Some(tracker) = self.trackers.get_mut(bead_id) {
            tracker.current_agent = Some(next_agent.to_string());
            tracker.phase_index = phase + 1;
        }

        let bead_ref = WorkRef {
            repo: repo.to_string(),
            scope: self
                .trackers
                .get(bead_id)
                .map(|t| t.scope.clone())
                .unwrap_or_default(),
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
                eprintln!("[phase] {bead_id} → {next_agent} (phase {})", phase + 1);
            }
        }
        self.persist_status(bead_id, repo, crate::bead::BeadState::Open)
            .await;
    }

    // ── Public entry points ─────────────────────────────────────────────

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
            deadlettered_ids: Vec::new(),
            agent_closed: 0,
        };

        for (bead_id, exit_success) in completed {
            // Classify FIRST, from everything the run left behind, on EVERY path
            // (orchestrator-managed included): a session-limit hit exits 0 with a
            // "successful" result and must never enter verifying (rosary-1780ea).
            // Also before execute_action, which drops the work_dir on failure.
            let (failure_class, exit_success, retry_wait) =
                self.classify_completion(bead_id, *exit_success);

            // Skip orchestrator-managed beads — their completions are handled
            // by orchestrator_tick() which calls on_worker_completed().
            if self.orchestrators.contains_key(bead_id.as_str()) {
                // Feed the completion back to the orchestrator. The orchestrator
                // owns its own retry budget, so a parsed reset time is not applied
                // here — but the downgraded exit keeps a limit hit out of verify.
                let (action, _verify_summary) =
                    self.verify_and_decide(bead_id, exit_success, beads);
                let passed = matches!(
                    action,
                    CompletionAction::Advance { .. } | CompletionAction::Terminal
                );
                if let Some(orch) = self.orchestrators.get_mut(bead_id.as_str()) {
                    orch.on_worker_completed(passed, self.config.max_retries);
                }
                if passed {
                    result.passed += 1;
                } else {
                    result.failed += 1;
                }
                continue;
            }
            let repo = self
                .trackers
                .get(bead_id.as_str())
                .map(|t| t.repo.clone())
                .unwrap_or_else(|| {
                    beads
                        .iter()
                        .find(|b| b.id == *bead_id)
                        .map(|b| b.repo.clone())
                        .unwrap_or_default()
                });

            // Agent-first fast path: the agent already closed the bead via MCP
            // (rsry_bead_close). Skip the full compile/test/lint pipeline — it's
            // wasted compute and would produce noisy rejected-state-transition logs
            // (BeadState validation blocks Closed → Verifying and Closed → PrOpen).
            // Design note: this path trusts the agent's self-close signal, which is
            // a conscious tradeoff against the "pipeline enforces shape" principle.
            // The dev-tier refinement loop (compile/test/lint) is bypassed for
            // self-closing agents. The feature-tier verification (Phase 5.5) still
            // runs on the assembled thread branch, providing a downstream gate.
            if self.is_bead_agent_closed(bead_id, &repo).await {
                self.trackers.remove(bead_id.as_str());
                result.agent_closed += 1;
                continue;
            }

            // Signal "In Review" in Linear while verification tiers run —
            // but only when the agent exited successfully. A crashed agent
            // (exit_success=false) goes straight to retry/deadletter without
            // running any verification, so "verifying" would be misleading.
            if exit_success {
                let (agent, phase) = self
                    .trackers
                    .get(bead_id.as_str())
                    .map(|t| {
                        (
                            t.current_agent
                                .clone()
                                .unwrap_or_else(|| "unknown".to_string()),
                            t.phase_index,
                        )
                    })
                    .unwrap_or_else(|| ("unknown".to_string(), 0));
                self.persist_status(bead_id, &repo, crate::bead::BeadState::Verifying)
                    .await;
                self.append_observation(
                    bead_id,
                    &repo,
                    &agent,
                    phase,
                    Verdict::Verifying,
                    "entering verifying state",
                )
                .await;
                result
                    .status_updates
                    .push((bead_id.clone(), repo.clone(), "verifying".into()));
            }

            let (mut action, verify_summary) = self.verify_and_decide(bead_id, exit_success, beads);

            // Enforceable feedback contract (rosary feedback-contract): a run
            // isn't complete until the agent recorded a native `feedback`
            // run-event. If verification would advance/complete the bead but the
            // agent left no feedback, downgrade to Retry — the retry prompt
            // carries the failure forward so the agent just leaves it (with the
            // fix-forward context it doesn't redo the work). No backend store →
            // fail-open (nothing to enforce against).
            if matches!(
                action,
                CompletionAction::Advance { .. } | CompletionAction::Terminal
            ) && let Some(dispatch_id) = self
                .trackers
                .get(bead_id.as_str())
                .and_then(|t| t.dispatch_id.clone())
            {
                let scope = self
                    .trackers
                    .get(bead_id.as_str())
                    .map(|t| t.scope.clone())
                    .unwrap_or_default();
                let bead_ref = crate::store::WorkRef {
                    repo: repo.clone(),
                    scope,
                    bead_id: bead_id.clone(),
                };
                if !self
                    .pipeline
                    .dispatch_left_feedback(&bead_ref, &dispatch_id)
                    .await
                {
                    eprintln!(
                        "[feedback] {bead_id}: passed verification but left no `feedback` \
                         run-event — job contract requires it; downgrading to retry"
                    );
                    action = CompletionAction::Retry;
                }
            }

            let outcome = self
                .execute_action(
                    bead_id,
                    &repo,
                    &action,
                    failure_class,
                    verify_summary.as_ref(),
                    thread_map,
                )
                .await;

            if let (ActionOutcome::Retrying, Some(wait)) = (&outcome, retry_wait) {
                self.queue
                    .defer_until(&repo, bead_id, std::time::Instant::now() + wait);
            }

            let outcome_str = match &outcome {
                ActionOutcome::Advanced { .. } => "success",
                ActionOutcome::Completed => "success",
                ActionOutcome::Retrying => "failure",
                ActionOutcome::Deadlettered => "deadletter",
            };
            if let Some(dispatch_id) = self
                .trackers
                .get(bead_id.as_str())
                .and_then(|t| t.dispatch_id.as_deref())
            {
                self.pipeline
                    .complete_dispatch(dispatch_id, &failure_class.qualify(outcome_str))
                    .await;
            }

            match outcome {
                ActionOutcome::Advanced { ref next_agent } => {
                    result.passed += 1;
                    result
                        .phase_advances
                        .push((bead_id.clone(), repo, next_agent.clone()));
                }
                ActionOutcome::Completed => {
                    result.passed += 1;
                    result
                        .status_updates
                        .push((bead_id.clone(), repo, "pr_open".into()));
                }
                ActionOutcome::Retrying => {
                    result.failed += 1;
                    result
                        .status_updates
                        .push((bead_id.clone(), repo, "open".into()));
                }
                ActionOutcome::Deadlettered => {
                    result.failed += 1;
                    result.deadlettered += 1;
                    // Record the id so the targeted-run exit sees the TARGET's
                    // own deadletter, not only liveness-sweep deadletters
                    // (rosary-5361f4 — the re-dispatch runaway).
                    result.deadlettered_ids.push(bead_id.clone());
                    result
                        .status_updates
                        .push((bead_id.clone(), repo, "dead_letter".into()));
                }
            }
        }

        result
    }

    /// Wait for all active agents to complete, then verify their work.
    /// Supports multi-stage pipelines: on phase advance, re-dispatches the
    /// next agent inline and continues the wait loop.
    pub(super) async fn wait_and_verify(&mut self) -> Result<WaitOutcome> {
        let mut wait_outcome = WaitOutcome::default();
        let poll_interval = Duration::from_secs(5);
        let timeout = Duration::from_secs(1800);
        let start = std::time::Instant::now();
        let mut last_heartbeat = std::time::Instant::now();

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
                if last_heartbeat.elapsed() >= Duration::from_secs(30) {
                    for (id, handle) in &self.active {
                        let agent = self
                            .trackers
                            .get(id.as_str())
                            .and_then(|t| t.current_agent.as_deref())
                            .unwrap_or("agent");
                        let running_for = (chrono::Utc::now() - handle.started_at).num_seconds();
                        eprintln!("[waiting] {id} ({agent}) running for {running_for}s");
                    }
                    last_heartbeat = std::time::Instant::now();
                }
                tokio::time::sleep(poll_interval).await;
                continue;
            }

            let thread_map: HashMap<String, String> = HashMap::new();
            let mut completed_beads = Vec::new();
            for (bead_id, _) in &completed {
                let repo = self
                    .trackers
                    .get(bead_id.as_str())
                    .map(|t| t.repo.clone())
                    .unwrap_or_default();
                if let Some(client) = self.dolt_client(&repo).await
                    && let Ok(Some(bead)) = client.get_bead(bead_id, &repo).await
                {
                    completed_beads.push(bead);
                }
            }

            for (bead_id, exit_success) in &completed {
                let repo = self
                    .trackers
                    .get(bead_id.as_str())
                    .map(|t| t.repo.clone())
                    .unwrap_or_default();
                let issue_type = self
                    .trackers
                    .get(bead_id.as_str())
                    .map(|t| t.issue_type.clone())
                    .unwrap_or_default();

                // Signal "In Review" in Linear while verification runs —
                // only when the agent exited successfully (same guard as verify_completed).
                // Classify FIRST, from everything the run left behind: a session-limit
                // hit exits 0 with a "successful" result and must never enter
                // verifying (rosary-1780ea). Also before execute_action, which drops
                // the work_dir on failure.
                let (failure_class, exit_success, retry_wait) =
                    self.classify_completion(bead_id, *exit_success);

                if exit_success {
                    let (agent, phase) = self
                        .trackers
                        .get(bead_id.as_str())
                        .map(|t| {
                            (
                                t.current_agent
                                    .clone()
                                    .unwrap_or_else(|| "unknown".to_string()),
                                t.phase_index,
                            )
                        })
                        .unwrap_or_else(|| ("unknown".to_string(), 0));
                    self.persist_status(bead_id, &repo, crate::bead::BeadState::Verifying)
                        .await;
                    self.append_observation(
                        bead_id,
                        &repo,
                        &agent,
                        phase,
                        Verdict::Verifying,
                        "entering verifying state",
                    )
                    .await;
                }

                let (action, verify_summary) =
                    self.verify_and_decide(bead_id, exit_success, &completed_beads);

                let outcome = self
                    .execute_action(
                        bead_id,
                        &repo,
                        &action,
                        failure_class,
                        verify_summary.as_ref(),
                        &thread_map,
                    )
                    .await;

                // Record completion for every dispatch attempt in --once mode.
                if let (ActionOutcome::Retrying, Some(wait)) = (&outcome, retry_wait) {
                    self.queue
                        .defer_until(&repo, bead_id, std::time::Instant::now() + wait);
                }

                let outcome_str = match &outcome {
                    ActionOutcome::Advanced { .. } => "success",
                    ActionOutcome::Completed => "success",
                    ActionOutcome::Retrying => "failure",
                    ActionOutcome::Deadlettered => "deadletter",
                };
                if let Some(dispatch_id) = self
                    .trackers
                    .get(bead_id.as_str())
                    .and_then(|t| t.dispatch_id.as_deref())
                {
                    self.pipeline
                        .complete_dispatch(dispatch_id, &failure_class.qualify(outcome_str))
                        .await;
                }

                // Report terminal outcomes to the run loop: its targeted-mode exit
                // conditions are the target's own deadletter and a completed
                // pipeline, and this path is the only one that sees them in
                // once mode. Before rosary-1780ea nothing flowed back, so a
                // deadlettered target was re-dispatched every scan, forever.
                wait_outcome.record(&outcome, bead_id);

                // wait_and_verify dispatches the next agent inline (unlike verify_completed
                // which defers to the next triage pass).
                if let ActionOutcome::Advanced { ref next_agent } = outcome {
                    let path = self
                        .repo_info
                        .get(&repo)
                        .map(|(p, _)| p.clone())
                        .unwrap_or_default();

                    let phase = self
                        .trackers
                        .get(bead_id.as_str())
                        .map(|t| t.phase_index)
                        .unwrap_or(1);

                    let mut dispatch_bead = crate::bead::Bead {
                        id: bead_id.clone(),
                        title: String::new(),
                        description: String::new(),
                        status: "dispatched".into(),
                        priority: 2,
                        issue_type,
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
                        created_by: None,
                        scope: String::new(),
                        acceptance_criteria: String::new(),
                        derived_from: vec![],
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
                        None,
                        None, // model: verify re-dispatch uses provider default
                    )
                    .await
                    {
                        Ok(handle) => {
                            eprintln!("[dispatch] {bead_id} phase {phase} → {next_agent}");
                            // Record this inline re-dispatch and update tracker.
                            let new_dispatch_id =
                                crate::pipeline::make_dispatch_id(bead_id, handle.started_at);
                            let dispatch_record = crate::store::DispatchRecord {
                                id: new_dispatch_id.clone(),
                                bead_ref: crate::store::WorkRef {
                                    repo: repo.clone(),
                                    scope: self
                                        .trackers
                                        .get(bead_id.as_str())
                                        .map(|t| t.scope.clone())
                                        .unwrap_or_default(),
                                    bead_id: bead_id.clone(),
                                },
                                agent: next_agent.clone(),
                                provider: self.provider.name().to_string(),
                                started_at: handle.started_at,
                                completed_at: None,
                                outcome: None,
                                work_dir: handle.work_dir.display().to_string(),
                                session_id: None,
                                session_ref: handle.session_ref(),
                                workspace_path: handle.workspace_path.clone(),
                                chain_hash: handle.chain_hash.clone(),
                            };
                            self.pipeline.record_dispatch(&dispatch_record).await;
                            if let Some(tracker) = self.trackers.get_mut(bead_id.as_str()) {
                                tracker.dispatch_id = Some(new_dispatch_id);
                            }
                            self.persist_status(bead_id, &repo, crate::bead::BeadState::Dispatched)
                                .await;
                            self.append_observation(
                                bead_id,
                                &repo,
                                next_agent,
                                phase,
                                Verdict::Dispatched,
                                "re-dispatched for next phase",
                            )
                            .await;
                            self.active.insert(bead_id.clone(), handle);
                        }
                        Err(e) => {
                            // Phase was already advanced (write_handoff_and_advance committed
                            // tracker + pipeline state) but the next agent couldn't spawn.
                            // This is unrecoverable without human intervention — the bead is
                            // in an inconsistent state. Block it rather than leave it wedged
                            // at Dispatched with no running agent.
                            eprintln!(
                                "[dispatch] {bead_id}: spawn failed for {next_agent} after \
                                 phase advance — deadlettering: {e}"
                            );
                            self.persist_status(bead_id, &repo, crate::bead::BeadState::DeadLetter)
                                .await;
                            self.append_observation(
                                bead_id,
                                &repo,
                                next_agent,
                                phase,
                                Verdict::Fail,
                                &format!("spawn failed after phase advance: {e}"),
                            )
                            .await;
                            self.trackers.remove(bead_id.as_str());
                            // A deadletter like any other: the targeted run's exit
                            // condition must see it, or it reports deadlettered=0
                            // and exits via the no-active-work path.
                            wait_outcome.record(&ActionOutcome::Deadlettered, bead_id);
                        }
                    }
                }
            }
        }

        Ok(wait_outcome)
    }
}

/// What a targeted (`--once`) wait observed: the terminal outcomes the run
/// loop's exit conditions are keyed on.
#[derive(Debug, Default)]
pub(super) struct WaitOutcome {
    /// Pipelines that reached Terminal (PR open).
    pub passed: usize,
    /// Failed attempts (retrying or deadlettered), as `VerifyResult::failed`
    /// counts them — else a targeted run that failed six times reports 0.
    pub failed: usize,
    /// Beads deadlettered during the wait.
    pub deadlettered_ids: Vec<String>,
}

impl WaitOutcome {
    /// Tally one bead's outcome the way `verify_completed` tallies its own.
    fn record(&mut self, outcome: &ActionOutcome, bead_id: &str) {
        match outcome {
            ActionOutcome::Completed => self.passed += 1,
            ActionOutcome::Retrying => self.failed += 1,
            ActionOutcome::Deadlettered => {
                self.failed += 1;
                self.deadlettered_ids.push(bead_id.to_string());
            }
            _ => {}
        }
    }
}

/// The observation detail for a failed run. A zero exit downgraded to a
/// failure (a session limit) must not be logged as a non-zero process exit,
/// or the event log misdiagnoses the incident.
fn failed_run_detail(class: FailureClass) -> String {
    match class {
        FailureClass::Unknown | FailureClass::Ok => "agent exit non-zero".to_string(),
        FailureClass::SessionLimit => "session limit hit (zero exit downgraded)".to_string(),
        other => format!("agent exit non-zero ({other})"),
    }
}

fn close_condition_text(bead: &crate::bead::Bead) -> String {
    match (bead.acceptance_criteria.trim(), bead.description.trim()) {
        ("", description) => description.to_string(),
        (acceptance_criteria, "") => acceptance_criteria.to_string(),
        (acceptance_criteria, description) => format!("{acceptance_criteria}\n{description}"),
    }
}

/// Read up to the last 8 KiB of a workspace's stderr log for failure
/// classification. Best-effort — a missing or unreadable log yields an empty
/// string (which classifies to [`FailureClass::Unknown`]).
fn read_log_tail(work_dir: &std::path::Path, name: &str) -> String {
    use std::io::{Read, Seek, SeekFrom};
    const MAX_TAIL_BYTES: u64 = 8 * 1024;
    // Seek, don't slurp: the stream log is the whole transcript (tool output,
    // file contents) and can be large; only the bounded tail is ever used.
    let Ok(mut file) = std::fs::File::open(work_dir.join(name)) else {
        return String::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if file
        .seek(SeekFrom::Start(len.saturating_sub(MAX_TAIL_BYTES)))
        .is_err()
    {
        return String::new();
    }
    let mut bytes = Vec::with_capacity(len.min(MAX_TAIL_BYTES) as usize);
    if file.read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The text of the stream log's final `{"type":"result"}` record — the
/// harness's own verdict on the run. Only this record is classified: the rest
/// of the stream is the agent's transcript (tool output, file contents, its
/// own prose), and an agent that merely *talks about* a session limit — e.g.
/// one editing this classifier — must not have its successful run downgraded.
/// Empty when the tail holds no parseable result record.
fn stream_result_text(stream_tail: &str) -> String {
    stream_tail
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line.trim()).ok())
        .find(|v| v.get("type").and_then(|t| t.as_str()) == Some("result"))
        .and_then(|v| v.get("result").and_then(|r| r.as_str()).map(str::to_string))
        .unwrap_or_default()
}

#[cfg(test)]
mod stderr_tail_tests {
    use super::*;

    #[test]
    fn tail_of_stderr_log_classifies_the_failure() {
        // The end-to-end seam: a workspace stderr log with a spawn-time auth
        // failure → the classifier reads it and names the cause.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(dispatch::STDERR_LOG_FILENAME),
            "Error: Not logged in. Please run `claude login`.\n",
        )
        .unwrap();

        let tail = read_log_tail(dir.path(), dispatch::STDERR_LOG_FILENAME);
        assert_eq!(classify(false, &tail), FailureClass::Auth);
    }

    #[test]
    fn missing_log_yields_empty_tail_and_unknown() {
        let dir = tempfile::tempdir().unwrap();
        // No .rsry-stderr.log written.
        assert!(read_log_tail(dir.path(), dispatch::STDERR_LOG_FILENAME).is_empty());
        assert_eq!(classify(false, ""), FailureClass::Unknown);
    }
}

#[cfg(test)]
mod bead_keyed_verify_tests {
    use super::*;
    use crate::reconcile::{BeadTracker, ReconcilerConfig};

    fn tracker(agent: &str, issue_type: &str) -> BeadTracker {
        BeadTracker {
            repo: "test-repo".into(),
            last_generation: 0,
            retries: 0,
            consecutive_reverts: 0,
            highest_tier: None,
            current_agent: Some(agent.into()),
            phase_index: 0,
            issue_type: issue_type.into(),
            dispatch_id: None,
            scope: String::new(),
        }
    }

    /// rosary-1780ea (2): staging-agent on rosary-e5fcd2 — a bead that WRITES
    /// three test files — logged "skipped (ReadOnly agent)" and went straight
    /// to PR creation; the PR carried an out-of-scope src change only a human
    /// caught. Verification is keyed to the BEAD: when a read-only agent is the
    /// terminal phase of a bead that declares files, its work is verified.
    #[tokio::test]
    async fn readonly_terminal_agent_on_bead_with_test_files_is_verified() {
        let mut r = Reconciler::new(ReconcilerConfig::default()).await;
        // "review" pipelines are [staging-agent] alone — staging is terminal.
        r.trackers
            .insert("e5fcd2".into(), tracker("staging-agent", "review"));
        let work = tempfile::tempdir().unwrap();
        r.completed_work_dirs.insert(
            "e5fcd2".into(),
            (work.path().to_path_buf(), "test-repo".into()),
        );
        let mut bead = crate::testutil::make_bead("e5fcd2", "review", "test-repo");
        bead.test_files = vec!["tests/beads_dirty_journey.rs".into()];

        let summary = r.verify_agent("e5fcd2", None, Some(&bead));
        assert!(
            summary.is_some(),
            "a terminal read-only phase on a bead with test_files must be verified \
             (rosary-1780ea) — verify is keyed to the bead, not the agent"
        );
        // And it is a real verification: an empty work dir has no commit.
        assert!(!summary.unwrap().passed());
    }

    /// Copilot on #506: `wait_and_verify` ignores `get_bead` errors, so the
    /// bead can be missing here. A terminal read-only phase must then be
    /// verified (fail closed), not waved through as "nothing to verify".
    #[tokio::test]
    async fn readonly_terminal_phase_without_bead_metadata_fails_closed() {
        let mut r = Reconciler::new(ReconcilerConfig::default()).await;
        r.trackers
            .insert("nb-1".into(), tracker("staging-agent", "review"));
        let work = tempfile::tempdir().unwrap();
        r.completed_work_dirs.insert(
            "nb-1".into(),
            (work.path().to_path_buf(), "test-repo".into()),
        );
        let summary = r.verify_agent("nb-1", None, None);
        assert!(
            summary.is_some_and(|s| !s.passed()),
            "missing metadata must verify, and an empty work dir fails"
        );
    }

    #[test]
    fn failed_run_detail_does_not_call_a_session_limit_a_nonzero_exit() {
        assert_eq!(
            failed_run_detail(FailureClass::Unknown),
            "agent exit non-zero"
        );
        let limit = failed_run_detail(FailureClass::SessionLimit);
        assert!(
            limit.contains("session limit") && !limit.contains("non-zero"),
            "{limit}"
        );
        assert_eq!(
            failed_run_detail(FailureClass::Auth),
            "agent exit non-zero (auth)"
        );
    }

    /// The scoping phase of a bug pipeline is read-only AND non-terminal: it
    /// produces comments, not commits, so verifying it would fail every bug
    /// bead at phase 0. The skip survives for that case.
    #[tokio::test]
    async fn readonly_non_terminal_phase_is_still_skipped() {
        let mut r = Reconciler::new(ReconcilerConfig::default()).await;
        r.trackers
            .insert("b-1".into(), tracker("scoping-agent", "bug"));
        let mut bead = crate::testutil::make_bead("b-1", "bug", "test-repo");
        bead.test_files = vec!["tests/x.rs".into()];
        assert!(r.verify_agent("b-1", None, Some(&bead)).is_none());
    }

    /// rosary-1780ea (3): a work dir whose stream log carries the claude
    /// session-limit result classifies as SessionLimit even though the process
    /// exited 0, and the reported exit is downgraded so verify never runs.
    #[tokio::test]
    async fn session_limit_in_stream_log_downgrades_a_zero_exit() {
        let mut r = Reconciler::new(ReconcilerConfig::default()).await;
        let work = tempfile::tempdir().unwrap();
        std::fs::write(
            work.path().join(dispatch::STREAM_LOG_FILENAME),
            "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\
             \"result\":\"You've hit your session limit · resets 3:40pm\"}\n",
        )
        .unwrap();
        r.completed_work_dirs.insert(
            "sl-1".into(),
            (work.path().to_path_buf(), "test-repo".into()),
        );
        let (class, exit_success, wait) = r.classify_completion("sl-1", true);
        assert_eq!(class, FailureClass::SessionLimit);
        assert!(
            !exit_success,
            "a limit hit is a failed attempt, not work to verify"
        );
        assert!(wait.is_some(), "the reset time keys the backoff");
    }

    /// A successful run whose TRANSCRIPT mentions a session limit (an agent
    /// editing this very classifier, or writing docs about limits) is not a
    /// limit hit: only the final `result` record is the harness's verdict.
    #[tokio::test]
    async fn session_limit_text_in_transcript_does_not_downgrade_a_success() {
        let mut r = Reconciler::new(ReconcilerConfig::default()).await;
        let work = tempfile::tempdir().unwrap();
        std::fs::write(
            work.path().join(dispatch::STREAM_LOG_FILENAME),
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\
             \"text\":\"Matching \\\"You've hit your session limit\\\" in provenance.rs\"}]}}\n\
             {\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\",\
             \"content\":\"if t.contains(\\\"session limit\\\") || t.contains(\\\"usage limit\\\")\"}]}}\n\
             {\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\
             \"result\":\"Implemented the classifier change; tests pass.\"}\n",
        )
        .unwrap();
        r.completed_work_dirs.insert(
            "ok-1".into(),
            (work.path().to_path_buf(), "test-repo".into()),
        );
        let (class, exit_success, wait) = r.classify_completion("ok-1", true);
        assert_eq!(class, FailureClass::Ok);
        assert!(exit_success, "a real success must reach verify");
        assert!(wait.is_none());
    }

    /// Work dir with the given stderr tail and final `result` text.
    fn completion_fixture(
        r: &mut Reconciler,
        id: &str,
        stderr: &str,
        result: &str,
    ) -> tempfile::TempDir {
        let work = tempfile::tempdir().unwrap();
        std::fs::write(work.path().join(dispatch::STDERR_LOG_FILENAME), stderr).unwrap();
        let record = serde_json::json!({"type": "result", "subtype": "success", "result": result});
        std::fs::write(
            work.path().join(dispatch::STREAM_LOG_FILENAME),
            format!("{record}\n"),
        )
        .unwrap();
        r.completed_work_dirs
            .insert(id.into(), (work.path().to_path_buf(), "test-repo".into()));
        work
    }

    /// The final `result` is the agent's own summary on a good run. One that
    /// MENTIONS limits (this repo's own session-limit work) is a success, and
    /// so is a clean exit whose stderr carries a benign "timeout" warning: a
    /// zero exit is only ever overturned by the limit notice itself.
    #[tokio::test]
    async fn a_success_whose_summary_mentions_limits_still_reaches_verify() {
        let mut r = Reconciler::new(ReconcilerConfig::default()).await;
        let _w = completion_fixture(
            &mut r,
            "ok-2",
            "warning: request timeout raised to 30s\n",
            "Implemented session limit handling: a usage limit notice now backs off until the reset.",
        );
        assert_eq!(
            r.classify_completion("ok-2", true),
            (FailureClass::Ok, true, None)
        );
    }

    /// A failed exit keeps the stderr classifier: the harness's own limit or
    /// timeout message is the diagnosis.
    #[tokio::test]
    async fn a_failed_exit_is_classified_from_stderr() {
        let mut r = Reconciler::new(ReconcilerConfig::default()).await;
        let _a = completion_fixture(&mut r, "f-1", "Claude usage limit reached.\n", "");
        assert_eq!(
            r.classify_completion("f-1", false).0,
            FailureClass::SessionLimit
        );
        let _b = completion_fixture(&mut r, "f-2", "error: request timed out\n", "");
        assert_eq!(
            r.classify_completion("f-2", false),
            (FailureClass::Timeout, false, None)
        );
    }

    #[test]
    fn session_limit_notice_is_anchored_not_a_substring() {
        use crate::dispatch::provenance::is_session_limit_notice;
        assert!(is_session_limit_notice(
            "You've hit your session limit · resets 3:40pm"
        ));
        assert!(is_session_limit_notice(
            "  Claude usage limit reached. Your limit will reset at 4pm"
        ));
        assert!(!is_session_limit_notice(
            "Fixed: \"You've hit your session limit\" now backs off"
        ));
        assert!(!is_session_limit_notice(
            "Implemented session limit handling"
        ));
        assert!(!is_session_limit_notice(""));
    }

    /// The e2e shape: five retries then a deadletter is six failures, not the
    /// "failed=0" a targeted run reported when `WaitOutcome` carried no count.
    #[test]
    fn wait_outcome_counts_every_failed_attempt() {
        let mut w = WaitOutcome::default();
        for _ in 0..5 {
            w.record(&ActionOutcome::Retrying, "t-1");
        }
        w.record(&ActionOutcome::Deadlettered, "t-1");
        assert_eq!((w.passed, w.failed), (0, 6));
        assert_eq!(w.deadlettered_ids, vec!["t-1".to_string()]);
    }

    #[test]
    fn stream_result_text_takes_the_last_result_record_only() {
        let tail = "{\"type\":\"assistant\",\"text\":\"session limit\"}\n\
                    not json at all\n\
                    {\"type\":\"result\",\"result\":\"done\"}\n";
        assert_eq!(stream_result_text(tail), "done");
        assert_eq!(stream_result_text("{\"type\":\"assistant\"}\n"), "");
        assert_eq!(stream_result_text(""), "");
    }
}
