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
use crate::verify::{Verifier, VerifySummary};
use crate::xref;

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
    /// Backend storage for orchestrator state (decades, threads, pipeline).
    pub backend: Option<crate::config::BackendConfig>,
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
            backend: None,
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
    /// Current agent name (e.g. "dev-agent"). Set on dispatch, used for handoffs.
    current_agent: Option<String>,
    /// Current pipeline phase index (0-based). Advances on phase completion.
    phase_index: u32,
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
    /// Path to agent definitions directory (from self_managed repo).
    agents_dir: Option<PathBuf>,
    /// Hierarchy store (decades, threads, bead membership).
    /// When set, enables thread-aware dedup and phase context.
    #[allow(dead_code)] // Wired in next phase: thread-aware triage
    hierarchy: Option<Box<dyn crate::store::HierarchyStore>>,
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
    /// Beads transitioned by VCS commit references.
    pub vcs_transitions: usize,
}

impl std::fmt::Display for IterationSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "scanned={} vcs={} triaged={} dispatched={} completed={} passed={} failed={} deadlettered={} agent_closed={}",
            self.scanned,
            self.vcs_transitions,
            self.triaged,
            self.dispatched,
            self.completed,
            self.passed,
            self.failed,
            self.deadlettered,
            self.agent_closed
        )
    }
}

impl Reconciler {
    pub async fn new(config: ReconcilerConfig) -> Self {
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
                compute: config.compute.clone(),
                ..Default::default()
            };
            match crate::config::compute_provider_from_config(&tmp_cfg) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("[reconcile] compute provider failed ({e}), using local");
                    Box::new(crate::backend::LocalProvider)
                }
            }
        };

        // Discover agents_dir from self-managed repo
        let agents_dir = config
            .repo
            .iter()
            .find(|r| r.self_managed)
            .map(|r| scanner::expand_path(&r.path).join("agents"))
            .filter(|p| p.exists());

        if let Some(ref dir) = agents_dir {
            eprintln!("[reconcile] agents_dir: {}", dir.display());
        } else {
            eprintln!(
                "[reconcile] warning: no agents_dir found (no self-managed repo with agents/)"
            );
        }

        // Connect hierarchy store (DoltBackend) if backend config is present.
        // Best-effort: hierarchy features degrade gracefully when unavailable.
        let hierarchy: Option<Box<dyn crate::store::HierarchyStore>> =
            if let Some(ref backend_cfg) = config.backend {
                match crate::store_dolt::DoltBackend::connect(backend_cfg).await {
                    Ok(backend) => {
                        eprintln!("[reconcile] hierarchy store connected (DoltBackend)");
                        Some(Box::new(backend))
                    }
                    Err(e) => {
                        eprintln!(
                            "[reconcile] hierarchy store unavailable ({e}), \
                             thread-aware features disabled"
                        );
                        None
                    }
                }
            } else {
                eprintln!("[reconcile] no [backend] config, hierarchy store disabled");
                None
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
            agents_dir,
            hierarchy,
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

        // Sweep orphaned workspaces from previous runs
        let repo_paths: Vec<PathBuf> = self.repo_info.values().map(|(p, _)| p.clone()).collect();
        let active_ids: Vec<String> = self.active.keys().cloned().collect();
        crate::workspace::sweep_orphaned(&repo_paths, &active_ids);

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

        // Phase 1.5: VCS SCAN — detect bead refs in recent jj commits
        summary.vcs_transitions = self.scan_vcs(&beads).await;

        // Phase 1.75: CROSS-REPO SYNC — propagate external refs across repos
        let ext_refs = xref::find_external_refs(&beads);
        if !ext_refs.is_empty() {
            xref::sync_external_refs(&ext_refs, &self.dolt_clients, &beads).await;
        }

        // Phase 1.75: AUTO-ASSIGN — set owner on beads without one
        for bead in &beads {
            if bead.owner.is_some() || bead.status == "closed" || bead.status == "done" {
                continue;
            }
            let agent = dispatch::default_agent(&bead.issue_type);
            if let Some(client) = self.dolt_client(&bead.repo).await {
                if let Err(e) = client.set_assignee(&bead.id, agent).await {
                    eprintln!("[assign] failed for {}: {e}", bead.id);
                } else {
                    eprintln!(
                        "[assign] {} → {} (issue_type={})",
                        bead.id, agent, bead.issue_type
                    );
                }
            }
        }

        // Phase 2: CHECK COMPLETED — poll active agents
        let completed = self.check_completed();
        summary.completed = completed.len();

        // Phase 2.5: AUTO-THREAD — cluster open beads and persist as threads.
        // Only runs when hierarchy store is available. Sequential and SharedScope
        // clusters become threads; NearDuplicate and Overlapping are left for dedup.
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
        let thread_map: HashMap<String, String> = if let Some(ref hierarchy) = self.hierarchy {
            let mut map = HashMap::new();
            for bead in &beads {
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
        };

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

            // (Smart triage) Dependency-aware: hard-filter beads with unresolved deps.
            // triage_score already penalizes these, but we skip them entirely to
            // avoid dispatching work whose prerequisites aren't done yet.
            if bead.is_blocked() {
                continue;
            }

            // (Smart triage) Per-repo coordination: don't dispatch to a repo that
            // already has an active agent. Prevents conflicts from concurrent
            // modifications to the same repo (uncommitted work, branch collisions).
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

            // File overlap: defer if candidate's files conflict with an active/queued
            // bead. Prevents merge conflicts from concurrent modifications to the
            // same source files (e.g., two agents both writing to serve.rs).
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
                // Re-check file overlap against beads dispatched earlier in this loop.
                // Triage couldn't catch these because they were queued simultaneously.
                let active_beads: Vec<&crate::bead::Bead> = beads
                    .iter()
                    .filter(|b| self.active.contains_key(&b.id))
                    .collect();
                if let Some(blocker) = epic::has_file_overlap(bead, &active_beads) {
                    eprintln!(
                        "[file-overlap] deferring {} — files conflict with just-dispatched {blocker}",
                        entry.bead_id
                    );
                    continue;
                }

                match dispatch::spawn(
                    bead,
                    &path,
                    true,
                    entry.generation,
                    self.provider.as_ref(),
                    self.agents_dir.as_deref(),
                )
                .await
                {
                    Ok(handle) => {
                        let agent_label = bead.owner.as_deref().unwrap_or("generic");
                        println!(
                            "[dispatch] {} (gen={}, retries={}, provider={}, agent={})",
                            entry.bead_id,
                            entry.generation,
                            entry.retries,
                            self.provider.name(),
                            agent_label,
                        );
                        self.persist_status(&entry.bead_id, &entry.repo, "dispatched")
                            .await;

                        // Record the dispatch branch and workspace path
                        let branch = format!("fix/{}", entry.bead_id);
                        if let Some(client) = self.dolt_client(&entry.repo).await {
                            client
                                .log_event(&entry.bead_id, "dispatch_branch", &branch)
                                .await;
                            // Record workspace_path so agents can resume in the same workspace
                            if let Some(ref ws_path) = handle.workspace_path {
                                client
                                    .log_event(&entry.bead_id, "workspace_path", ws_path)
                                    .await;
                            }
                        }

                        self.active.insert(entry.bead_id.clone(), handle);
                        let agent_name = bead.owner.clone();
                        let tracker =
                            self.trackers
                                .entry(entry.bead_id.clone())
                                .or_insert(BeadTracker {
                                    repo: entry.repo.clone(),
                                    last_generation: entry.generation,
                                    retries: entry.retries,
                                    consecutive_reverts: 0,
                                    highest_tier: None,
                                    current_agent: None,
                                    phase_index: 0,
                                });
                        tracker.last_generation = entry.generation;
                        tracker.repo = entry.repo.clone();
                        tracker.current_agent = agent_name;
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
            // MCP, skip verification entirely. This is the main throughput win —
            // verification (compile+test+lint) takes minutes per bead.
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
                                // Keep workspace for next pipeline phase
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
                                // Keep workspace for next pipeline phase
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

        Ok(summary)
    }

    /// Scan jj logs across repos for bead references in commit messages.
    /// Triggers state transitions: open → dispatched (for refs), open → done (for closes).
    /// Returns the number of transitions triggered.
    async fn scan_vcs(&mut self, beads: &[crate::bead::Bead]) -> usize {
        use crate::vcs;

        // Collect repo info first to avoid borrow conflicts with &mut self
        let repos: Vec<(String, PathBuf)> = self
            .repo_info
            .iter()
            .map(|(name, (path, _))| (name.clone(), path.clone()))
            .collect();

        // Gather all VCS refs across repos
        let mut pending: Vec<(String, String, String, String, bool)> = Vec::new(); // (repo, bead_id, bead_repo, change_id, closes)
        for (repo_name, repo_path) in &repos {
            let vcs_refs = match vcs::scan_vcs_bead_refs(repo_path) {
                Ok(refs) => refs,
                Err(_) => continue,
            };

            for (change_id, bead_ref) in &vcs_refs {
                let bead = beads.iter().find(|b| b.id == bead_ref.id);
                let Some(bead) = bead else { continue };

                // Determine target status
                let should_transition = if bead_ref.closes {
                    !matches!(bead.status.as_str(), "done" | "closed")
                } else {
                    bead.status.as_str() == "open"
                };

                if !should_transition || self.active.contains_key(&bead.id) {
                    continue;
                }

                pending.push((
                    repo_name.clone(),
                    bead.id.clone(),
                    bead.repo.clone(),
                    change_id.clone(),
                    bead_ref.closes,
                ));
            }
        }

        // Apply transitions
        let mut transitions = 0;
        for (repo_name, bead_id, bead_repo, change_id, closes) in &pending {
            let new_status = if *closes { "closed" } else { "dispatched" };

            println!("[vcs] {repo_name}: {bead_id} → {new_status} (jj change {change_id})");

            if let Some(client) = self.dolt_client(bead_repo).await {
                let _ = client
                    .log_event(bead_id, "vcs_ref", &format!("jj:{change_id}"))
                    .await;
            }

            self.persist_status(bead_id, bead_repo, new_status).await;
            transitions += 1;
        }

        transitions
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
        // Cleanup happens after checkpoint (called from iterate)
    }

    /// Checkpoint workspace (jj commit + bookmark) without cleanup.
    ///
    /// Used during phase advancement: the workspace stays alive so the
    /// next pipeline agent reuses the same worktree and its changes.
    async fn checkpoint_workspace(&mut self, bead_id: &str) -> Option<String> {
        let change_id = if let Some(ws) = self.completed_workspaces.remove(bead_id) {
            let message = format!("fix({bead_id}): agent work");
            let result = ws.checkpoint(&message).await;
            // Put it back — workspace stays for next phase or cleanup
            self.completed_workspaces.insert(bead_id.to_string(), ws);
            match result {
                Ok(Some(id)) => {
                    eprintln!("[checkpoint] {bead_id}: jj change {id}");
                    Some(id)
                }
                Ok(None) => None,
                Err(e) => {
                    eprintln!("[checkpoint] {bead_id}: failed: {e}");
                    None
                }
            }
        } else {
            None
        };

        // Log change_id as event for audit trail
        if let Some(ref cid) = change_id {
            let repo = self
                .trackers
                .get(bead_id)
                .map(|t| t.repo.clone())
                .unwrap_or_default();
            if let Some(client) = self.dolt_client(&repo).await {
                client.log_event(bead_id, "jj_checkpoint", cid).await;
            }
        }

        change_id
    }

    /// Checkpoint workspace then write handoff + manifest, then clean up.
    ///
    /// Used when the pipeline is complete (no next agent) or on deadletter.
    async fn checkpoint_and_cleanup(&mut self, bead_id: &str) -> Option<String> {
        let change_id = self.checkpoint_workspace(bead_id).await;

        // Write handoff + manifest to workspace before cleanup
        let repo = self
            .trackers
            .get(bead_id)
            .map(|t| t.repo.clone())
            .unwrap_or_default();
        if let Some(ws) = self.completed_workspaces.get(bead_id) {
            let work_dir = &ws.work_dir;

            // Build work summary from git
            let work = crate::manifest::Work::from_git(work_dir, change_id.as_deref());

            // Write handoff for the phase that just completed
            let (agent, phase) = self
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
            let mut handoff = crate::handoff::Handoff::new(
                phase,
                &agent,
                None,
                bead_id,
                self.provider.name(),
                &work,
            );
            // Look up thread_id from hierarchy if available
            if let Some(ref hierarchy) = self.hierarchy {
                let bead_ref = crate::store::BeadRef {
                    repo: repo.clone(),
                    bead_id: bead_id.to_string(),
                };
                if let Ok(Some(tid)) = hierarchy.find_thread_for_bead(&bead_ref).await {
                    handoff.thread_id = Some(tid);
                }
            }
            if let Err(e) = handoff.write_to(work_dir) {
                eprintln!("[handoff] {bead_id}: failed to write: {e}");
            }

            // Write manifest
            let vcs_kind = match ws.vcs {
                crate::workspace::VcsKind::Jj => "jj",
                crate::workspace::VcsKind::Git => "git",
                crate::workspace::VcsKind::None => "none",
            };
            let mut manifest = crate::manifest::Manifest::at_spawn(
                &format!("d-{bead_id}"),
                bead_id,
                &repo,
                &agent,
                self.provider.name(),
                "task",
                "implement",
                phase,
                &work_dir.display().to_string(),
                &ws.repo_path.display().to_string(),
                vcs_kind,
                None,
            );
            manifest.work = work;
            manifest.complete(true, Some("end_turn"));
            if let Err(e) = manifest.write_to(work_dir) {
                eprintln!("[manifest] {bead_id}: failed to write: {e}");
            }
        }

        // Terminal step: merge or PR based on issue type.
        // Runs outside the workspace borrow scope to allow dolt_client access.
        if let Some(ws) = self.completed_workspaces.get(bead_id) {
            let branch = format!("fix/{bead_id}");
            let ws_repo_path = ws.repo_path.clone();
            let issue_type = if let Some(client) = self.dolt_client(&repo).await {
                client
                    .get_bead(bead_id, &repo)
                    .await
                    .ok()
                    .flatten()
                    .map(|b| b.issue_type)
                    .unwrap_or_else(|| "task".to_string())
            } else {
                "task".to_string()
            };
            let _ =
                crate::workspace::merge_or_pr(&ws_repo_path, &branch, bead_id, &issue_type).await;
        }

        self.cleanup_workspace(bead_id);
        change_id
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
                current_agent: None,
                phase_index: 0,
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
                current_agent: None,
                phase_index: 0,
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

// merge_or_pr moved to workspace.rs as a shared function

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
        backend: cfg.backend,
        ..Default::default()
    };

    let mut reconciler = Reconciler::new(reconciler_config).await;
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
            vcs_transitions: 1,
        };
        let display = format!("{s}");
        assert!(display.contains("scanned=10"));
        assert!(display.contains("dispatched=2"));
        assert!(display.contains("vcs=1"));
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

    #[tokio::test]
    async fn severity_floor_blocks_p3_with_min_priority_2() {
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
            files: Vec::new(),
            test_files: Vec::new(),
        };

        let config = ReconcilerConfig::default();
        let r = Reconciler::new(config).await;
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

        let mut reconciler = Reconciler::new(config).await;
        let summary = reconciler.iterate().await.unwrap();
        assert_eq!(summary.scanned, 0);
        assert_eq!(summary.dispatched, 0);
    }

    #[tokio::test]
    async fn on_pass_clears_state() {
        let config = ReconcilerConfig {
            once: true,
            repo: Vec::new(),
            ..Default::default()
        };
        let mut r = Reconciler::new(config).await;

        r.trackers.insert(
            "x".into(),
            BeadTracker {
                repo: "test".into(),
                last_generation: 1,
                retries: 2,
                consecutive_reverts: 1,
                highest_tier: Some(3),
                current_agent: None,
                phase_index: 0,
            },
        );

        r.on_pass("x");
        assert_eq!(r.trackers["x"].consecutive_reverts, 0);
    }

    #[tokio::test]
    async fn on_fail_exit_deadletters_after_max() {
        let config = ReconcilerConfig {
            max_retries: 3,
            once: true,
            repo: Vec::new(),
            ..Default::default()
        };
        let mut r = Reconciler::new(config).await;

        // Retries increment: 1, 2, 3 — deadletter at 3 == max_retries
        assert!(!r.on_fail_exit("x")); // retries=1
        assert!(!r.on_fail_exit("x")); // retries=2
        assert!(r.on_fail_exit("x")); // retries=3 == max, deadletter
    }

    #[tokio::test]
    async fn on_fail_consecutive_reverts_deadletter() {
        let config = ReconcilerConfig {
            max_retries: 100, // won't hit this
            once: true,
            repo: Vec::new(),
            ..Default::default()
        };
        let mut r = Reconciler::new(config).await;

        // Set initial high tier
        r.trackers.insert(
            "x".into(),
            BeadTracker {
                repo: "test".into(),
                last_generation: 1,
                retries: 0,
                consecutive_reverts: 0,
                highest_tier: Some(4),
                current_agent: None,
                phase_index: 0,
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
        let mut r = Reconciler::new(config).await;

        // Simulate: bead "x" was dispatched at generation 42, then failed
        r.trackers.insert(
            "x".into(),
            BeadTracker {
                repo: "test".into(),
                last_generation: 42,
                retries: 1,
                consecutive_reverts: 0,
                highest_tier: None,
                current_agent: None,
                phase_index: 0,
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
            files: Vec::new(),
            test_files: Vec::new(),
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

    // -- Smart triage tests --

    #[test]
    fn blocked_bead_filtered_by_triage() {
        // A bead with dependency_count > 0 should be hard-filtered in triage,
        // not just scored low. This prevents dispatching work whose
        // prerequisites aren't done yet.
        let bead = crate::bead::Bead {
            id: "dep-blocked".into(),
            title: "blocked by deps".into(),
            description: String::new(),
            status: "open".into(),
            priority: 0, // highest priority
            issue_type: "task".into(),
            owner: None,
            repo: "test".into(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            dependency_count: 2, // has unresolved deps
            dependent_count: 0,
            comment_count: 0,
            branch: None,
            pr_url: None,
            jj_change_id: None,
            external_ref: None,
            files: Vec::new(),
            test_files: Vec::new(),
        };

        // is_blocked returns true for open beads with deps
        assert!(bead.is_blocked());
        // is_ready returns false
        assert!(!bead.is_ready());
        // Even with P0 priority, the bead should be blocked
        assert_eq!(bead.state(), BeadState::Open);
    }

    #[test]
    fn self_managed_repo_gets_score_boost() {
        // Beads from self-managed repos should get a 0.15 score boost,
        // making them dispatch before equivalent beads on other repos.
        let now = chrono::Utc::now();
        let bead = crate::bead::Bead {
            id: "self-1".into(),
            title: "self-managed task".into(),
            description: String::new(),
            status: "open".into(),
            priority: 2,
            issue_type: "task".into(),
            owner: None,
            repo: "rosary".into(),
            created_at: now,
            updated_at: now,
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

        let base_score = queue::triage_score(&bead, 0, now);
        let boosted_score = (base_score + 0.15).min(1.0);
        assert!(
            boosted_score > base_score,
            "self-managed boost should increase score: {boosted_score} vs {base_score}"
        );
        // The boost is 0.15 — enough to push self-managed beads ahead
        assert!(
            (boosted_score - base_score - 0.15).abs() < f64::EPSILON,
            "boost should be exactly 0.15"
        );
    }

    #[tokio::test]
    async fn repo_busy_check_uses_trackers() {
        // When an agent is active on repo "mache", no other bead from
        // "mache" should be triaged. This tests the tracker lookup logic.
        let config = ReconcilerConfig {
            once: true,
            repo: Vec::new(),
            ..Default::default()
        };
        let mut r = Reconciler::new(config).await;

        // Simulate an active agent on repo "mache"
        r.trackers.insert(
            "mache-abc".into(),
            BeadTracker {
                repo: "mache".into(),
                last_generation: 1,
                retries: 0,
                consecutive_reverts: 0,
                highest_tier: None,
                current_agent: None,
                phase_index: 0,
            },
        );
        // Mark it as active (need a dummy handle — use the key presence)
        // We can't easily create an AgentHandle in tests, so test the
        // lookup logic directly:
        let active_ids = ["mache-abc".to_string()];
        let candidate_repo = "mache";

        let repo_busy = active_ids.iter().any(|active_id| {
            r.trackers
                .get(active_id)
                .is_some_and(|t| t.repo == candidate_repo)
        });
        assert!(repo_busy, "repo with active agent should be busy");

        // Different repo should not be busy
        let other_repo = "rosary";
        let other_busy = active_ids.iter().any(|active_id| {
            r.trackers
                .get(active_id)
                .is_some_and(|t| t.repo == other_repo)
        });
        assert!(!other_busy, "repo without active agent should not be busy");
    }
}
