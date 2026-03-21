//! Reconciliation loop — the core of rosary.
//!
//! Implements a Kubernetes-controller-style desired-state loop:
//!   scan → triage → dispatch → verify → report → sleep → repeat
//!
//! Modeled after driftlessaf's workqueue patterns and gem's tiered verification.

mod completion;
mod helpers;
mod persistence;
mod vcs;
mod workspace_ops;

use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::bead::BeadState;
use crate::config::{self, RepoConfig};
use crate::dispatch::{self, AgentHandle};
use crate::dolt::DoltClient;
use crate::epic;
use crate::pipeline::{CompletionAction, PipelineEngine};
use crate::queue::{self, QueueEntry, WorkQueue};
use crate::scanner;
use crate::store::BeadRef;
use crate::sync::IssueTracker;
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
    /// Target a specific bead — skip triage, only dispatch this one.
    pub target_bead: Option<String>,
    /// Pipeline definitions: issue_type → agent sequence (from config).
    pub pipelines: HashMap<String, Vec<String>>,
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
            target_bead: None,
            pipelines: crate::config::default_pipelines(),
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
    /// Bead's issue type (e.g. "bug", "task"). Captured at dispatch time so
    /// wait_and_verify can determine pipeline advancement without a fresh scan.
    issue_type: String,
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
    /// Schema-driven pipeline engine. Replaces hardcoded agent_pipeline() match.
    /// Uses DispatchStore for persistent pipeline state (survives crashes).
    pipeline: PipelineEngine,
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
            let lang = repo
                .lang
                .clone()
                .unwrap_or_else(|| helpers::detect_language(&path));
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

        // Connect backend (DoltBackend) if config is present.
        // Provides both HierarchyStore and DispatchStore from the same database.
        // Best-effort: features degrade gracefully when unavailable.
        #[allow(clippy::type_complexity)]
        let (hierarchy, dispatch_store): (
            Option<Box<dyn crate::store::HierarchyStore>>,
            Option<Box<dyn crate::store::DispatchStore>>,
        ) = if let Some(ref backend_cfg) = config.backend {
            // Connect twice — sqlx pools are Arc-based so this shares the connection pool.
            let hierarchy = match crate::store_dolt::DoltBackend::connect(backend_cfg).await {
                Ok(backend) => {
                    eprintln!("[reconcile] hierarchy store connected (DoltBackend)");
                    Some(Box::new(backend) as Box<dyn crate::store::HierarchyStore>)
                }
                Err(e) => {
                    eprintln!(
                        "[reconcile] hierarchy store unavailable ({e}), \
                         thread-aware features disabled"
                    );
                    None
                }
            };
            let dispatch = match crate::store_dolt::DoltBackend::connect(backend_cfg).await {
                Ok(backend) => {
                    eprintln!("[reconcile] dispatch store connected (DoltBackend)");
                    Some(Box::new(backend) as Box<dyn crate::store::DispatchStore>)
                }
                Err(e) => {
                    eprintln!("[reconcile] dispatch store unavailable ({e})");
                    None
                }
            };
            (hierarchy, dispatch)
        } else {
            eprintln!("[reconcile] no [backend] config, backend stores disabled");
            (None, None)
        };

        // Build pipeline engine from config + backend store.
        let pipeline = PipelineEngine::new(config.pipelines.clone(), dispatch_store);

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
            pipeline,
        }
    }

    /// Attach an external issue tracker for status mirroring.
    /// When set, every bead state transition also updates the linked Linear issue.
    #[allow(dead_code)] // API surface — called from main.rs when LINEAR_API_KEY is set
    pub fn set_issue_tracker(&mut self, tracker: Box<dyn IssueTracker>) {
        self.issue_tracker = Some(tracker);
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
        // If --bead is set, skip normal triage and only enqueue that bead.
        let target_filter = self.config.target_bead.clone();
        let now = chrono::Utc::now();
        for bead in &beads {
            if let Some(ref target) = target_filter {
                if bead.id != *target {
                    continue;
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
                                    issue_type: bead.issue_type.clone(),
                                });
                        tracker.last_generation = entry.generation;
                        tracker.repo = entry.repo.clone();
                        tracker.current_agent = agent_name;
                        tracker.issue_type = bead.issue_type.clone();

                        // Persist initial pipeline state to backend store
                        let bead_ref = BeadRef {
                            repo: entry.repo.clone(),
                            bead_id: entry.bead_id.clone(),
                        };
                        let pipeline_state =
                            self.pipeline.initial_state(bead_ref, &bead.issue_type);
                        self.pipeline.upsert_state(&pipeline_state).await;

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
        // Uses PipelineEngine.decide() for advance/terminal/retry/deadletter.
        let mut status_updates: Vec<(String, String, String)> = Vec::new();
        let mut phase_advances: Vec<(String, String, String)> = Vec::new();

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
                .unwrap_or_else(|| {
                    // Fallback: look up from scan results
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

            // Determine verification result
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

            // Use pipeline engine for the completion decision
            let action = self.pipeline.decide(
                &issue_type,
                current_agent.as_deref(),
                *exit_success,
                verify_passed,
                retries,
                self.config.max_retries,
            );

            // Update tracker state (on_pass/on_fail) and execute the action
            match action {
                CompletionAction::Advance { ref next_agent, .. } => {
                    summary.passed += 1;
                    self.on_pass(bead_id);
                    self.checkpoint_workspace(bead_id).await;
                    phase_advances.push((bead_id.clone(), repo, next_agent.clone()));
                }
                CompletionAction::Terminal => {
                    summary.passed += 1;
                    self.on_pass(bead_id);
                    self.checkpoint_and_cleanup(bead_id).await;
                    let bead_ref = BeadRef {
                        repo: repo.clone(),
                        bead_id: bead_id.clone(),
                    };
                    self.pipeline.clear_state(&bead_ref).await;
                    status_updates.push((bead_id.clone(), repo, "closed".into()));
                }
                CompletionAction::Retry => {
                    summary.failed += 1;
                    if *exit_success {
                        // Verify failure — use on_fail for tier tracking
                        if let Some(ref vs) = verify_summary {
                            self.on_fail(bead_id, vs);
                        }
                    } else {
                        self.completed_work_dirs.remove(bead_id);
                        self.on_fail_exit(bead_id);
                    }
                    status_updates.push((bead_id.clone(), repo, "open".into()));
                }
                CompletionAction::Deadletter => {
                    summary.failed += 1;
                    summary.deadlettered += 1;
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
                    status_updates.push((bead_id.clone(), repo, "blocked".into()));
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

            // Persist pipeline state to backend store
            let bead_ref = BeadRef {
                repo: repo.clone(),
                bead_id: bead_id.clone(),
            };
            self.pipeline
                .upsert_state(&crate::store::PipelineState {
                    bead_ref,
                    pipeline_phase: (phase + 1) as u8,
                    pipeline_agent: next_agent.clone(),
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

        Ok(summary)
    }

    /// Wait for all active agents to complete, then verify their work.
    ///
    /// This is the "sub-loop" that closes the dispatch cycle: poll agents
    /// every 5 seconds until all finish, run verification, update bead status.
    /// Supports multi-stage pipelines: on phase advance, re-dispatches the
    /// next agent inline and continues the wait loop.
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

            // Verify + pipeline decision for each completed agent
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

                // Determine verification result
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

                        // Write handoff
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

                        // Advance tracker
                        if let Some(tracker) = self.trackers.get_mut(bead_id.as_str()) {
                            tracker.current_agent = Some(next_agent.clone());
                            tracker.phase_index = phase + 1;
                        }

                        // Persist pipeline state
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

                        // Update assignee in Dolt
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

                        // Re-dispatch next agent inline. Build a synthetic Bead
                        // from tracker state for dispatch::spawn().
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
                        // Try to get full bead info from Dolt for a richer prompt
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
                        let bead_ref = BeadRef {
                            repo: repo.clone(),
                            bead_id: bead_id.clone(),
                        };
                        self.pipeline.clear_state(&bead_ref).await;
                        self.persist_status(bead_id, &repo, "closed").await;
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

// merge_or_pr moved to workspace.rs as a shared function

/// Entry point for `rsry run`.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    config_path: &str,
    concurrency: usize,
    interval: u64,
    once: bool,
    dry_run: bool,
    provider: &str,
    overnight: bool,
    target_bead: Option<&str>,
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
        target_bead: target_bead.map(|s| s.to_string()),
        pipelines: cfg.pipelines,
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
mod tests;
