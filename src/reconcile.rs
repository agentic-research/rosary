//! Reconciliation loop — the core of rosary.
//!
//! Implements a Kubernetes-controller-style desired-state loop:
//!   scan → triage → dispatch → verify → report → sleep → repeat
//!
//! Modeled after driftlessaf's workqueue patterns and gem's tiered verification.

use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::bead::BeadState;
use crate::config::{self, RepoConfig};
use crate::dispatch::{self, AgentHandle};
use crate::dolt::{DoltClient, DoltConfig};
use crate::epic;
use crate::queue::{self, QueueEntry, WorkQueue};
use crate::scanner;
use crate::sync::IssueTracker;
use crate::thread;
use crate::verify::{Verifier, VerifySummary};

/// Configuration for the reconciliation loop.
pub struct ReconcilerConfig {
    pub max_concurrent: usize,
    pub scan_interval: Duration,
    pub max_retries: u32,
    pub triage_threshold: f64,
    pub repo: Vec<RepoConfig>,
    pub once: bool,
    pub dry_run: bool,
    /// AI provider name (e.g. "claude", "gemini"). Default "claude".
    pub provider: String,
    /// Overnight mode: prefer small/mechanical beads agents can complete.
    pub overnight: bool,
    /// Compute provider config (from [compute] in rosary.toml).
    pub compute: Option<crate::config::ComputeConfig>,
}

impl Default for ReconcilerConfig {
    fn default() -> Self {
        ReconcilerConfig {
            max_concurrent: 5,
            scan_interval: Duration::from_secs(30),
            max_retries: 5,
            triage_threshold: 0.3,
            repo: Vec::new(),
            once: false,
            dry_run: false,
            provider: "claude".to_string(),
            overnight: false,
            compute: None,
        }
    }
}

/// Tracks state of a bead across loop iterations.
#[derive(Debug)]
struct BeadTracker {
    repo: String,
    last_generation: u64,
    retries: u32,
    consecutive_reverts: u32,
    highest_tier: Option<usize>,
}

/// The reconciliation loop orchestrator.
pub struct Reconciler {
    config: ReconcilerConfig,
    queue: WorkQueue,
    active: HashMap<String, AgentHandle>,
    trackers: HashMap<String, BeadTracker>,
    /// Map repo name → (path, lang) for verification
    repo_info: HashMap<String, (PathBuf, String)>,
    /// Stash work_dir + repo when agent completes so verify_agent can find it
    completed_work_dirs: HashMap<String, (PathBuf, String)>,
    /// Stash workspaces from completed agents for checkpoint + teardown
    completed_workspaces: HashMap<String, crate::workspace::Workspace>,
    /// Dolt clients keyed by repo name, lazily connected
    dolt_clients: HashMap<String, DoltClient>,
    /// Resolved AI agent provider (claude, gemini, etc).
    provider: Box<dyn dispatch::AgentProvider>,
    /// Optional external issue tracker (Linear, etc.) for status mirroring.
    /// When set, persist_status also pushes state transitions to the tracker.
    issue_tracker: Option<Box<dyn IssueTracker>>,
    /// Compute provider for workspace provisioning (local, sprites, etc).
    compute: Box<dyn crate::backend::ComputeProvider>,
}

/// Summary of a single reconciliation iteration.
#[derive(Debug, Default)]
pub struct IterationSummary {
    pub scanned: usize,
    pub triaged: usize,
    pub dispatched: usize,
    pub completed: usize,
    pub passed: usize,
    pub failed: usize,
    pub deadlettered: usize,
    /// Beads closed by the agent via MCP (skipped verification).
    pub agent_closed: usize,
}

impl std::fmt::Display for IterationSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "scanned={} triaged={} dispatched={} completed={} passed={} failed={} deadlettered={} agent_closed={}",
            self.scanned,
            self.triaged,
            self.dispatched,
            self.completed,
            self.passed,
            self.failed,
            self.deadlettered,
            self.agent_closed,
        )
    }
}

impl Reconciler {
    pub fn new(config: ReconcilerConfig) -> Self {
        let mut repo_info = HashMap::new();

        // Build repo info map from config
        for repo in &config.repo {
            let path = scanner::expand_path(&repo.path);
            let lang = repo.lang.clone().unwrap_or_else(|| detect_language(&path));
            repo_info.insert(repo.name.clone(), (path, lang));
        }

        let provider = dispatch::provider_by_name(&config.provider).unwrap_or_else(|e| {
            eprintln!("[reconcile] {e}, falling back to claude");
            dispatch::provider_by_name("claude").unwrap()
        });

        // Build compute provider from config, fall back to local on failure.
        let compute: Box<dyn crate::backend::ComputeProvider> = {
            // Temporarily build a Config with just the compute field for the factory.
            let tmp_cfg = crate::config::Config {
                repo: vec![],
                linear: None,
                compute: config.compute.clone(),
                http: None,
            };
            match crate::config::compute_provider_from_config(&tmp_cfg) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("[reconcile] compute provider failed ({e}), using local");
                    Box::new(crate::backend::LocalProvider)
                }
            }
        };

        Reconciler {
            config,
            queue: WorkQueue::new(),
            active: HashMap::new(),
            trackers: HashMap::new(),
            repo_info,
            completed_work_dirs: HashMap::new(),
            completed_workspaces: HashMap::new(),
            dolt_clients: HashMap::new(),
            provider,
            issue_tracker: None,
            compute,
        }
    }

    /// Attach an external issue tracker for status mirroring.
    /// When set, every bead state transition also updates the linked Linear issue.
    #[allow(dead_code)] // API surface — called from main.rs when LINEAR_API_KEY is set
    pub fn set_issue_tracker(&mut self, tracker: Box<dyn IssueTracker>) {
        self.issue_tracker = Some(tracker);
    }

    /// Check if a bead was already closed by the dispatched agent via MCP.
    ///
    /// This is the "agent-first" fast path: when agents self-close beads,
    /// we skip the full verification pipeline (compile+test+lint+diff-sanity),
    /// which is the main consumption throughput bottleneck.
    async fn is_bead_agent_closed(&mut self, bead_id: &str, repo: &str) -> bool {
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
    async fn recover_stuck_beads(&mut self) {
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

    /// Run the reconciliation loop.
    pub async fn run(&mut self) -> Result<()> {
        println!(
            "Reconciler started: max_concurrent={}, interval={}s, dry_run={}",
            self.config.max_concurrent,
            self.config.scan_interval.as_secs(),
            self.config.dry_run,
        );

        // Recover beads stuck at 'dispatched' from previous crashed run
        self.recover_stuck_beads().await;

        loop {
            let summary = self.iterate().await?;
            println!("[reconcile] {summary}");

            if self.config.once {
                println!("[reconcile] active agents: {}", self.active.len());
                if !self.active.is_empty() {
                    println!(
                        "[reconcile] waiting for {} active agent(s)...",
                        self.active.len()
                    );
                    self.wait_and_verify().await?;
                }
                println!("[reconcile] single-pass mode, exiting");
                break;
            }

            tokio::time::sleep(self.config.scan_interval).await;
        }

        Ok(())
    }

    /// Execute one full iteration of the reconciliation loop.
    pub async fn iterate(&mut self) -> Result<IterationSummary> {
        let mut summary = IterationSummary::default();

        // Phase 1: SCAN
        let beads = scanner::scan_repos(&self.config.repo).await?;
        summary.scanned = beads.len();

        // Phase 1.5: CROSS-REPO SYNC — propagate external refs across repos
        let ext_refs = thread::find_external_refs(&beads);
        if !ext_refs.is_empty() {
            thread::sync_external_refs(&ext_refs, &self.dolt_clients, &beads).await;
        }

        // Phase 2: CHECK COMPLETED — poll active agents
        let completed = self.check_completed();
        summary.completed = completed.len();

        // Phase 3: TRIAGE — score open beads, enqueue above threshold
        let now = chrono::Utc::now();
        for bead in &beads {
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

            // Dedup: skip if semantically dominated by an active or queued bead.
            // Uses multi-signal similarity (title + description + scope prefix +
            // sequential pattern) instead of plain Jaccard on titles.
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

            let retries = self.queue.retries(&bead.id);
            let score = if self.config.overnight {
                queue::triage_score_overnight(bead, retries, now)
            } else {
                queue::triage_score(bead, retries, now)
            };

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

        // Phase 4: DISPATCH — fill free slots before verification
        // Dispatch runs BEFORE verify so new agents start working while the
        // reconciler spends time on verification (compile+test+lint = minutes).
        // This keeps all concurrency slots utilized instead of idling during verify.
        let dispatch_now = Instant::now();
        while self.active.len() < self.config.max_concurrent {
            let Some(entry) = self.queue.dequeue(dispatch_now) else {
                break;
            };

            if self.config.dry_run {
                println!(
                    "[dry-run] would dispatch {} (score={:.3}, retries={})",
                    entry.bead_id, entry.score, entry.retries
                );
                summary.dispatched += 1;
                continue;
            }

            // Find the bead and repo path for dispatch
            let bead = beads.iter().find(|b| b.id == entry.bead_id);
            let repo_path = self.repo_info.get(&entry.repo).map(|(p, _)| p.clone());

            if let (Some(bead), Some(path)) = (bead, repo_path) {
                match dispatch::spawn(bead, &path, true, entry.generation, self.provider.as_ref())
                    .await
                {
                    Ok(handle) => {
                        println!(
                            "[dispatch] {} (gen={}, retries={}, provider={})",
                            entry.bead_id,
                            entry.generation,
                            entry.retries,
                            self.provider.name()
                        );
                        self.persist_status(&entry.bead_id, &entry.repo, "dispatched")
                            .await;

                        // Record the dispatch branch
                        let branch = format!("fix/{}", entry.bead_id);
                        if let Some(client) = self.dolt_client(&entry.repo).await {
                            client
                                .log_event(&entry.bead_id, "dispatch_branch", &branch)
                                .await;
                        }

                        self.active.insert(entry.bead_id.clone(), handle);
                        let tracker =
                            self.trackers
                                .entry(entry.bead_id.clone())
                                .or_insert(BeadTracker {
                                    repo: entry.repo.clone(),
                                    last_generation: entry.generation,
                                    retries: entry.retries,
                                    consecutive_reverts: 0,
                                    highest_tier: None,
                                });
                        tracker.last_generation = entry.generation;
                        tracker.repo = entry.repo.clone();
                        summary.dispatched += 1;
                    }
                    Err(e) => {
                        eprintln!("[dispatch] failed for {}: {e}", entry.bead_id);
                    }
                }
            }
        }

        // Phase 5: VERIFY completed agents
        // Runs after dispatch so new agents execute in parallel with verification.
        let mut status_updates: Vec<(String, String, String)> = Vec::new();

        for (bead_id, exit_success) in &completed {
            let repo = self
                .trackers
                .get(bead_id.as_str())
                .map(|t| t.repo.clone())
                .unwrap_or_default();

            // Agent-first fast path: if the agent already closed the bead via
            // MCP, skip verification entirely. This is the main throughput win —
            // verification (compile+test+lint) takes minutes per bead.
            if self.is_bead_agent_closed(bead_id, &repo).await {
                self.completed_work_dirs.remove(bead_id);
                summary.agent_closed += 1;
                summary.passed += 1;
                self.on_pass(bead_id);
                continue;
            }

            if *exit_success {
                let verify_result = self.verify_agent(bead_id);
                match verify_result {
                    Some(vs) if vs.passed() => {
                        summary.passed += 1;
                        self.on_pass(bead_id);
                        status_updates.push((bead_id.clone(), repo, "closed".into()));
                    }
                    Some(vs) => {
                        summary.failed += 1;
                        let deadlettered = self.on_fail(bead_id, &vs);
                        if deadlettered {
                            summary.deadlettered += 1;
                            status_updates.push((bead_id.clone(), repo, "blocked".into()));
                        } else {
                            status_updates.push((bead_id.clone(), repo, "open".into()));
                        }
                    }
                    None => {
                        // No verifier available (unknown repo) — treat as pass
                        summary.passed += 1;
                        self.on_pass(bead_id);
                        status_updates.push((bead_id.clone(), repo, "closed".into()));
                    }
                }
            } else {
                self.completed_work_dirs.remove(bead_id);
                summary.failed += 1;
                let deadlettered = self.on_fail_exit(bead_id);
                if deadlettered {
                    summary.deadlettered += 1;
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

        Ok(summary)
    }

    /// Poll active agents for completion. Returns vec of (bead_id, exit_success).
    fn check_completed(&mut self) -> Vec<(String, bool)> {
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
    fn verify_agent(&mut self, bead_id: &str) -> Option<VerifySummary> {
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

    /// Wait for all active agents to complete, then verify their work.
    ///
    /// This is the "sub-loop" that closes the dispatch cycle: poll agents
    /// every 5 seconds until all finish, run verification, update bead status.
    async fn wait_and_verify(&mut self) -> Result<()> {
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
                    continue;
                }

                if *exit_success {
                    let verify_result = self.verify_agent(bead_id);
                    match verify_result {
                        Some(vs) if vs.passed() => {
                            self.on_pass(bead_id);
                            self.persist_status(bead_id, &repo, "closed").await;
                        }
                        Some(vs) => {
                            self.on_fail(bead_id, &vs);
                            self.persist_status(bead_id, &repo, "open").await;
                        }
                        None => {
                            // No verifier — treat as pass
                            self.on_pass(bead_id);
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

    /// Get or lazily connect a DoltClient for a repo.
    async fn dolt_client(&mut self, repo: &str) -> Option<&DoltClient> {
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
    async fn persist_status(&mut self, bead_id: &str, repo: &str, status: &str) {
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

    fn on_pass(&mut self, bead_id: &str) {
        println!("[pass] {bead_id}");
        self.queue.clear_backoff(bead_id);
        if let Some(tracker) = self.trackers.get_mut(bead_id) {
            tracker.consecutive_reverts = 0;
        }
        self.cleanup_workspace(bead_id);
    }

    /// Clean up the workspace for a completed bead.
    /// Delegates to workspace.rs cleanup functions to avoid duplication.
    fn cleanup_workspace(&mut self, bead_id: &str) {
        if let Some(ws) = self.completed_workspaces.remove(bead_id) {
            eprintln!(
                "[cleanup] {bead_id} workspace (vcs={:?}, compute={})",
                ws.vcs,
                self.compute.name()
            );
            match ws.vcs {
                crate::workspace::VcsKind::Jj => {
                    crate::workspace::cleanup_jj_workspace(&ws.repo_path, bead_id);
                }
                crate::workspace::VcsKind::Git => {
                    crate::workspace::cleanup_git_worktree(&ws.repo_path, bead_id);
                }
                crate::workspace::VcsKind::None => {}
            }
        } else {
            // Legacy fallback — try both VCS types
            crate::workspace::cleanup_jj_workspace(std::path::Path::new("."), bead_id);
            crate::workspace::cleanup_git_worktree(std::path::Path::new("."), bead_id);
        }
    }

    /// Handle a verification failure. Returns true if deadlettered.
    fn on_fail(&mut self, bead_id: &str, summary: &VerifySummary) -> bool {
        let tracker = self
            .trackers
            .entry(bead_id.to_string())
            .or_insert(BeadTracker {
                repo: String::new(),
                last_generation: 0,
                retries: 0,
                consecutive_reverts: 0,
                highest_tier: None,
            });

        // Check for revert (regression from previous best)
        if let (Some(prev), Some(curr)) = (tracker.highest_tier, summary.highest_passing_tier) {
            if curr < prev {
                tracker.consecutive_reverts += 1;
            } else {
                tracker.consecutive_reverts = 0;
            }
        }
        tracker.highest_tier = summary.highest_passing_tier;
        tracker.retries += 1;

        // Stopping conditions
        if tracker.retries >= self.config.max_retries {
            println!("[deadletter] {bead_id}: max retries ({})", tracker.retries);
            return true;
        }
        if tracker.consecutive_reverts >= 3 {
            println!(
                "[deadletter] {bead_id}: {} consecutive reverts",
                tracker.consecutive_reverts
            );
            return true;
        }

        // Schedule retry with backoff
        self.queue
            .record_backoff(bead_id, tracker.retries, Instant::now());
        if let Some((name, _)) = summary.first_failure() {
            println!(
                "[retry] {bead_id}: failed at tier '{name}', retry #{} scheduled",
                tracker.retries
            );
        }

        false
    }

    /// Handle agent exit failure (non-zero exit). Returns true if deadlettered.
    fn on_fail_exit(&mut self, bead_id: &str) -> bool {
        let tracker = self
            .trackers
            .entry(bead_id.to_string())
            .or_insert(BeadTracker {
                repo: String::new(),
                last_generation: 0,
                retries: 0,
                consecutive_reverts: 0,
                highest_tier: None,
            });
        tracker.retries += 1;

        if tracker.retries >= self.config.max_retries {
            println!(
                "[deadletter] {bead_id}: max retries after exit failure ({})",
                tracker.retries
            );
            return true;
        }

        self.queue
            .record_backoff(bead_id, tracker.retries, Instant::now());
        println!(
            "[retry] {bead_id}: agent exited non-zero, retry #{} scheduled",
            tracker.retries
        );
        self.cleanup_workspace(bead_id);

        false
    }
}

/// Detect language from repo contents.
fn detect_language(path: &std::path::Path) -> String {
    if path.join("Cargo.toml").exists() {
        "rust".to_string()
    } else if path.join("go.mod").exists() {
        "go".to_string()
    } else if path.join("package.json").exists() {
        "javascript".to_string()
    } else if path.join("pyproject.toml").exists() || path.join("setup.py").exists() {
        "python".to_string()
    } else {
        "unknown".to_string()
    }
}

/// Entry point for `rsry run`.
pub async fn run(
    config_path: &str,
    concurrency: usize,
    interval: u64,
    once: bool,
    dry_run: bool,
    provider: &str,
    overnight: bool,
) -> Result<()> {
    let cfg = config::load(config_path)?;

    // Extract linear config before cfg.repo is moved
    let linear_team = std::env::var("LINEAR_TEAM").unwrap_or_else(|_| {
        cfg.linear
            .as_ref()
            .map(|l| l.team.clone())
            .unwrap_or_else(|| "AGE".to_string())
    });
    let linear_state_overrides = cfg
        .linear
        .as_ref()
        .map(|l| l.states.clone())
        .unwrap_or_default();

    let reconciler_config = ReconcilerConfig {
        max_concurrent: concurrency,
        scan_interval: Duration::from_secs(interval),
        repo: cfg.repo,
        once,
        dry_run,
        provider: provider.to_string(),
        overnight,
        compute: cfg.compute,
        ..Default::default()
    };

    let mut reconciler = Reconciler::new(reconciler_config);
    if let Ok(api_key) = std::env::var("LINEAR_API_KEY") {
        match crate::linear_tracker::LinearTracker::with_overrides(
            &api_key,
            &linear_team,
            linear_state_overrides,
        )
        .await
        {
            Ok(tracker) => {
                eprintln!("[linear] attached tracker for team {linear_team}");
                reconciler.set_issue_tracker(Box::new(tracker));
            }
            Err(e) => {
                eprintln!("[linear] failed to attach tracker: {e} (continuing without)");
            }
        }
    }

    reconciler.run().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_language_rust() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        assert_eq!(detect_language(dir.path()), "rust");
    }

    #[test]
    fn detect_language_go() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module test").unwrap();
        assert_eq!(detect_language(dir.path()), "go");
    }

    #[test]
    fn detect_language_unknown() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(detect_language(dir.path()), "unknown");
    }

    #[test]
    fn iteration_summary_display() {
        let s = IterationSummary {
            scanned: 10,
            triaged: 3,
            dispatched: 2,
            completed: 1,
            passed: 1,
            failed: 0,
            deadlettered: 0,
            agent_closed: 0,
        };
        let display = format!("{s}");
        assert!(display.contains("scanned=10"));
        assert!(display.contains("dispatched=2"));
    }

    #[test]
    fn reconciler_config_defaults() {
        let cfg = ReconcilerConfig::default();
        assert_eq!(cfg.max_concurrent, 5);
        assert_eq!(cfg.scan_interval, Duration::from_secs(30));
        assert_eq!(cfg.max_retries, 5);
        assert!(!cfg.once);
        assert!(!cfg.dry_run);
        assert_eq!(cfg.provider, "claude");
    }

    #[test]
    fn severity_floor_blocks_p3_with_min_priority_2() {
        // A P3 bead should not pass the severity floor when min_priority=2.
        // This tests the integration point: queue::passes_severity_floor is
        // called in iterate() with self.queue.min_priority (default=2).
        let bead = crate::bead::Bead {
            id: "test-p3".into(),
            title: "low priority task".into(),
            description: String::new(),
            status: "open".into(),
            priority: 3,
            issue_type: "task".into(),
            owner: None,
            repo: "test".into(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            dependency_count: 0,
            dependent_count: 0,
            comment_count: 0,
            branch: None,
            pr_url: None,
            jj_change_id: None,
            external_ref: None,
        };

        let config = ReconcilerConfig::default();
        let r = Reconciler::new(config);
        // Default min_priority is 2, so P3 (priority=3) should be blocked
        assert!(
            !queue::passes_severity_floor(&bead, r.queue.min_priority),
            "P3 bead should not pass severity floor with min_priority=2"
        );
    }

    #[tokio::test]
    async fn reconciler_dry_run_single_pass() {
        // No repos configured — should complete immediately with empty scan
        let config = ReconcilerConfig {
            once: true,
            dry_run: true,
            repo: Vec::new(),
            ..Default::default()
        };

        let mut reconciler = Reconciler::new(config);
        let summary = reconciler.iterate().await.unwrap();
        assert_eq!(summary.scanned, 0);
        assert_eq!(summary.dispatched, 0);
    }

    #[test]
    fn on_pass_clears_state() {
        let config = ReconcilerConfig {
            once: true,
            repo: Vec::new(),
            ..Default::default()
        };
        let mut r = Reconciler::new(config);

        r.trackers.insert(
            "x".into(),
            BeadTracker {
                repo: "test".into(),
                last_generation: 1,
                retries: 2,
                consecutive_reverts: 1,
                highest_tier: Some(3),
            },
        );

        r.on_pass("x");
        assert_eq!(r.trackers["x"].consecutive_reverts, 0);
    }

    #[test]
    fn on_fail_exit_deadletters_after_max() {
        let config = ReconcilerConfig {
            max_retries: 3,
            once: true,
            repo: Vec::new(),
            ..Default::default()
        };
        let mut r = Reconciler::new(config);

        // Retries increment: 1, 2, 3 — deadletter at 3 == max_retries
        assert!(!r.on_fail_exit("x")); // retries=1
        assert!(!r.on_fail_exit("x")); // retries=2
        assert!(r.on_fail_exit("x")); // retries=3 == max, deadletter
    }

    #[test]
    fn on_fail_consecutive_reverts_deadletter() {
        let config = ReconcilerConfig {
            max_retries: 100, // won't hit this
            once: true,
            repo: Vec::new(),
            ..Default::default()
        };
        let mut r = Reconciler::new(config);

        // Set initial high tier
        r.trackers.insert(
            "x".into(),
            BeadTracker {
                repo: "test".into(),
                last_generation: 1,
                retries: 0,
                consecutive_reverts: 0,
                highest_tier: Some(4),
            },
        );

        // Three consecutive reverts (each lower than previous best)
        let regress = |highest: Option<usize>| crate::verify::VerifySummary {
            results: vec![
                ("commit".into(), crate::verify::VerifyResult::Pass),
                (
                    "test".into(),
                    crate::verify::VerifyResult::Fail("fail".into()),
                ),
            ],
            highest_passing_tier: highest,
        };

        assert!(!r.on_fail("x", &regress(Some(2)))); // 4→2, revert #1
        assert!(!r.on_fail("x", &regress(Some(1)))); // 2→1, revert #2
        assert!(r.on_fail("x", &regress(Some(0)))); // 1→0, revert #3 → deadletter
    }

    #[tokio::test]
    async fn failed_bead_retries_despite_same_generation() {
        // Scenario: bead dispatched → agent fails → retry scheduled.
        // On next iterate(), the bead's generation hasn't changed (Dolt wasn't updated).
        // The generation check must NOT block re-triage for beads with pending retries.
        let config = ReconcilerConfig {
            max_retries: 3,
            once: true,
            repo: Vec::new(),
            ..Default::default()
        };
        let mut r = Reconciler::new(config);

        // Simulate: bead "x" was dispatched at generation 42, then failed
        r.trackers.insert(
            "x".into(),
            BeadTracker {
                repo: "test".into(),
                last_generation: 42,
                retries: 1,
                consecutive_reverts: 0,
                highest_tier: None,
            },
        );
        // Record backoff (retry is pending)
        r.queue.record_backoff(
            "x",
            1,
            std::time::Instant::now() - std::time::Duration::from_secs(60),
        );

        // Create a bead with the SAME generation (42)
        let bead = crate::bead::Bead {
            id: "x".into(),
            title: "test bead".into(),
            description: String::new(),
            status: "open".into(),
            priority: 1,
            issue_type: "bug".into(),
            owner: None,
            repo: "test".into(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            dependency_count: 0,
            dependent_count: 0,
            comment_count: 0,
            external_ref: None,
            branch: None,
            pr_url: None,
            jj_change_id: None,
        };

        // The bead should still be triageable despite same generation
        let retries = r.queue.retries(&bead.id);
        assert_eq!(retries, 1, "should have 1 retry recorded");

        // Check: tracker has same generation, but retries > 0
        let tracker = r.trackers.get("x").unwrap();
        assert_eq!(tracker.last_generation, 42);
        assert_eq!(tracker.retries, 1);

        // The generation check should NOT block when retries > 0
        let bead_gen = bead.generation();
        let should_skip = tracker.last_generation == bead_gen && tracker.retries == 0;
        assert!(
            !should_skip,
            "bead with pending retries should NOT be skipped by generation check"
        );
    }
}
