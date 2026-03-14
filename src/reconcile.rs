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
use crate::queue::{self, QueueEntry, WorkQueue};
use crate::scanner;
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
}

impl Default for ReconcilerConfig {
    fn default() -> Self {
        ReconcilerConfig {
            max_concurrent: 3,
            scan_interval: Duration::from_secs(30),
            max_retries: 5,
            triage_threshold: 0.3,
            repo: Vec::new(),
            once: false,
            dry_run: false,
            provider: "claude".to_string(),
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
    /// Dolt clients keyed by repo name, lazily connected
    dolt_clients: HashMap<String, DoltClient>,
    /// Resolved AI agent provider (claude, gemini, etc).
    provider: Box<dyn dispatch::AgentProvider>,
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
}

impl std::fmt::Display for IterationSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "scanned={} triaged={} dispatched={} completed={} passed={} failed={} deadlettered={}",
            self.scanned,
            self.triaged,
            self.dispatched,
            self.completed,
            self.passed,
            self.failed,
            self.deadlettered,
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

        Reconciler {
            config,
            queue: WorkQueue::new(),
            active: HashMap::new(),
            trackers: HashMap::new(),
            repo_info,
            completed_work_dirs: HashMap::new(),
            dolt_clients: HashMap::new(),
            provider,
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

        // Phase 3: VERIFY completed agents
        // Collect (bead_id, repo, new_status) for Dolt persistence after processing.
        let mut status_updates: Vec<(String, String, String)> = Vec::new();

        for (bead_id, exit_success) in &completed {
            let repo = self
                .trackers
                .get(bead_id.as_str())
                .map(|t| t.repo.clone())
                .unwrap_or_default();

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

        // Phase 4: TRIAGE — score open beads, enqueue above threshold
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

            // Dedup: skip if too similar to an active or queued bead
            let dominated = beads
                .iter()
                .filter(|other| other.id != bead.id)
                .filter(|other| {
                    self.active.contains_key(&other.id) || self.queue.contains(&other.id)
                })
                .any(|other| scanner::jaccard_similarity(&bead.title, &other.title) > 0.6);
            if dominated {
                eprintln!(
                    "[dedup] skipping {} — similar to active/queued bead",
                    bead.id
                );
                continue;
            }

            let retries = self.queue.retries(&bead.id);
            let score = queue::triage_score(bead, retries, now);

            if score >= self.config.triage_threshold {
                let bead_gen = bead.generation();

                // Skip if already processed at this generation
                if let Some(tracker) = self.trackers.get(&bead.id)
                    && tracker.last_generation == bead_gen
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

        // Phase 5: DISPATCH — dequeue and spawn agents
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
                Ok(Some(status)) => {
                    done = true;
                    success = status.success();
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
                let handle = self.active.remove(&bead_id).unwrap();
                let repo = self
                    .trackers
                    .get(&bead_id)
                    .map(|t| t.repo.clone())
                    .unwrap_or_default();
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
        let timeout = Duration::from_secs(600); // 10 min max
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
    async fn persist_status(&mut self, bead_id: &str, repo: &str, status: &str) {
        if let Some(client) = self.dolt_client(repo).await {
            if let Err(e) = client.update_status(bead_id, status).await {
                eprintln!("[dolt] failed to update {bead_id} to {status}: {e}");
            }
            client
                .log_event(bead_id, "state_change", &format!("→ {status}"))
                .await;
        }
    }

    fn on_pass(&mut self, bead_id: &str) {
        println!("[pass] {bead_id}");
        self.queue.clear_backoff(bead_id);
        if let Some(tracker) = self.trackers.get_mut(bead_id) {
            tracker.consecutive_reverts = 0;
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
) -> Result<()> {
    let cfg = config::load(config_path)?;

    let reconciler_config = ReconcilerConfig {
        max_concurrent: concurrency,
        scan_interval: Duration::from_secs(interval),
        repo: cfg.repo,
        once,
        dry_run,
        provider: provider.to_string(),
        ..Default::default()
    };

    let mut reconciler = Reconciler::new(reconciler_config);
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
        };
        let display = format!("{s}");
        assert!(display.contains("scanned=10"));
        assert!(display.contains("dispatched=2"));
    }

    #[test]
    fn reconciler_config_defaults() {
        let cfg = ReconcilerConfig::default();
        assert_eq!(cfg.max_concurrent, 3);
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
}
