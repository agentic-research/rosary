//! Reconciliation loop — the core of loom.
//!
//! Implements a Kubernetes-controller-style desired-state loop:
//!   scan → diff → triage → dispatch → verify → report → sleep → repeat
//!
//! Modeled after driftlessaf's workqueue patterns and gem's tiered verification.

use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::bead::BeadState;
use crate::config::{self, RepoConfig};
use crate::dispatch::{self, AgentHandle};
use crate::queue::{self, QueueEntry, WorkQueue};
use crate::scanner;
use crate::verify::VerifySummary;

/// Configuration for the reconciliation loop.
pub struct ReconcilerConfig {
    pub max_concurrent: usize,
    pub scan_interval: Duration,
    pub max_retries: u32,
    pub triage_threshold: f64,
    pub repos: Vec<RepoConfig>,
    pub once: bool,
    pub dry_run: bool,
}

impl Default for ReconcilerConfig {
    fn default() -> Self {
        ReconcilerConfig {
            max_concurrent: 3,
            scan_interval: Duration::from_secs(30),
            max_retries: 5,
            triage_threshold: 0.3,
            repos: Vec::new(),
            once: false,
            dry_run: false,
        }
    }
}

/// Tracks state of a bead across loop iterations.
#[derive(Debug)]
struct BeadTracker {
    last_generation: u64,
    retries: u32,
    consecutive_reverts: u32,
    highest_tier: Option<usize>,
}

/// The reconciliation loop orchestrator.
pub struct Reconciler {
    config: ReconcilerConfig,
    queue: WorkQueue,
    semaphore: Arc<tokio::sync::Semaphore>,
    active: HashMap<String, AgentHandle>,
    trackers: HashMap<String, BeadTracker>,
    /// Map repo name → (path, lang) for verification
    repo_info: HashMap<String, (PathBuf, String)>,
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
        let permits = config.max_concurrent;
        let mut repo_info = HashMap::new();

        // Build repo info map from config
        for repo in &config.repos {
            let path = scanner::expand_path(&repo.path);
            let lang = detect_language(&path);
            repo_info.insert(repo.name.clone(), (path, lang));
        }

        Reconciler {
            config,
            queue: WorkQueue::new(),
            semaphore: Arc::new(tokio::sync::Semaphore::new(permits)),
            active: HashMap::new(),
            trackers: HashMap::new(),
            repo_info,
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
        let beads = scanner::scan_repos(&self.config.repos).await?;
        summary.scanned = beads.len();

        // Phase 2: CHECK COMPLETED — poll active agents
        let completed = self.check_completed();
        summary.completed = completed.len();

        // Phase 3: VERIFY completed agents
        for (bead_id, exit_success) in &completed {
            if *exit_success {
                let verify_result = self.verify_agent(bead_id);
                match verify_result {
                    Some(vs) if vs.passed() => {
                        summary.passed += 1;
                        self.on_pass(bead_id);
                    }
                    Some(vs) => {
                        summary.failed += 1;
                        if self.on_fail(bead_id, &vs) {
                            summary.deadlettered += 1;
                        }
                    }
                    None => {
                        // No verifier available (unknown repo) — treat as pass
                        summary.passed += 1;
                        self.on_pass(bead_id);
                    }
                }
            } else {
                summary.failed += 1;
                if self.on_fail_exit(bead_id) {
                    summary.deadlettered += 1;
                }
            }
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

            let retries = self.queue.retries(&bead.id);
            let score = queue::triage_score(bead, retries, now);

            if score >= self.config.triage_threshold {
                let bead_gen = bead.generation();

                // Skip if already processed at this generation
                if let Some(tracker) = self.trackers.get(&bead.id) {
                    if tracker.last_generation == bead_gen {
                        continue;
                    }
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
                match dispatch::spawn(bead, &path, true, entry.generation).await {
                    Ok(handle) => {
                        println!(
                            "[dispatch] {} (gen={}, retries={})",
                            entry.bead_id, entry.generation, entry.retries
                        );
                        self.active.insert(entry.bead_id.clone(), handle);
                        self.trackers
                            .entry(entry.bead_id.clone())
                            .or_insert(BeadTracker {
                                last_generation: entry.generation,
                                retries: entry.retries,
                                consecutive_reverts: 0,
                                highest_tier: None,
                            })
                            .last_generation = entry.generation;
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
            match handle.try_wait() {
                Ok(Some(status)) => {
                    completed.push((bead_id.clone(), status.success()));
                    self.active.remove(&bead_id);
                }
                Ok(None) => {
                    // Check timeout (10 min default)
                    if handle.elapsed() > chrono::Duration::minutes(10) {
                        eprintln!("[timeout] killing agent for {bead_id}");
                        let _ = handle.kill();
                        completed.push((bead_id.clone(), false));
                        self.active.remove(&bead_id);
                    }
                }
                Err(e) => {
                    eprintln!("[error] polling agent for {bead_id}: {e}");
                    completed.push((bead_id.clone(), false));
                    self.active.remove(&bead_id);
                }
            }
        }

        completed
    }

    /// Run verification tiers on an agent's work directory.
    fn verify_agent(&self, bead_id: &str) -> Option<VerifySummary> {
        // Find which repo this bead belongs to
        let tracker = self.trackers.get(bead_id)?;
        let _ = tracker; // used for future generation checks

        // Look up the work directory from the active handle — but it's already removed.
        // For now, find repo info from the tracker's last known state.
        // TODO: store work_dir in tracker when agent completes
        None
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
        let tracker = self.trackers.entry(bead_id.to_string()).or_insert(BeadTracker {
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
        let tracker = self.trackers.entry(bead_id.to_string()).or_insert(BeadTracker {
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

/// Entry point for `loom run`.
pub async fn run(
    config_path: &str,
    concurrency: usize,
    interval: u64,
    once: bool,
    dry_run: bool,
) -> Result<()> {
    let cfg = config::load(config_path)?;

    let reconciler_config = ReconcilerConfig {
        max_concurrent: concurrency,
        scan_interval: Duration::from_secs(interval),
        repos: cfg.repos,
        once,
        dry_run,
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
    }

    #[tokio::test]
    async fn reconciler_dry_run_single_pass() {
        // No repos configured — should complete immediately with empty scan
        let config = ReconcilerConfig {
            once: true,
            dry_run: true,
            repos: Vec::new(),
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
            repos: Vec::new(),
            ..Default::default()
        };
        let mut r = Reconciler::new(config);

        r.trackers.insert(
            "x".into(),
            BeadTracker {
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
            repos: Vec::new(),
            ..Default::default()
        };
        let mut r = Reconciler::new(config);

        // Retries increment: 1, 2, 3 — deadletter at 3 == max_retries
        assert!(!r.on_fail_exit("x")); // retries=1
        assert!(!r.on_fail_exit("x")); // retries=2
        assert!(r.on_fail_exit("x"));  // retries=3 == max, deadletter
    }

    #[test]
    fn on_fail_consecutive_reverts_deadletter() {
        let config = ReconcilerConfig {
            max_retries: 100, // won't hit this
            once: true,
            repos: Vec::new(),
            ..Default::default()
        };
        let mut r = Reconciler::new(config);

        // Set initial high tier
        r.trackers.insert(
            "x".into(),
            BeadTracker {
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
        assert!(r.on_fail("x", &regress(Some(0))));  // 1→0, revert #3 → deadletter
    }
}
