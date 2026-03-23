//! Reconciliation loop — the core of rosary.
//!
//! Implements a Kubernetes-controller-style desired-state loop:
//!   scan → triage → dispatch → verify → report → sleep → repeat
//!
//! Modeled after driftlessaf's workqueue patterns and gem's tiered verification.

mod completion;
mod helpers;
mod persistence;
mod threading;
mod triage;
mod vcs;
pub(crate) mod verify;
mod workspace_ops;

use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::config::{self, RepoConfig};
use crate::dispatch::{self, AgentHandle};
use crate::dolt::DoltClient;
use crate::epic;
use crate::pipeline::PipelineEngine;
use crate::queue::WorkQueue;
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
    /// Maximum pipeline stages per bead. 0 = unlimited.
    pub max_pipeline_depth: usize,
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
            max_pipeline_depth: 0,
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

        let binaries = crate::config::load_global()
            .ok()
            .and_then(|c| c.dispatch.map(|d| d.binaries))
            .unwrap_or_default();
        let provider =
            dispatch::provider_by_name(&config.provider, &binaries).unwrap_or_else(|e| {
                eprintln!("[reconcile] {e}, falling back to claude");
                dispatch::provider_by_name("claude", &binaries).unwrap()
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
        let pipeline = PipelineEngine::new(
            config.pipelines.clone(),
            dispatch_store,
            config.max_pipeline_depth,
        );

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

        // Phase 1.6: PR MERGE POLL — close beads whose PRs have merged
        self.poll_pr_merges(&beads).await;

        // Phase 1.75: CROSS-REPO SYNC — propagate external refs across repos
        let ext_refs = xref::find_external_refs(&beads);
        if !ext_refs.is_empty() {
            xref::sync_external_refs(&ext_refs, &self.dolt_clients, &beads).await;
        }

        // Phase 1.75: AUTO-ASSIGN — set owner on beads without one
        // Uses pipeline engine (config-driven) not dispatch::default_agent (hardcoded).
        for bead in &beads {
            if bead.owner.is_some() || bead.status == "closed" || bead.status == "done" {
                continue;
            }
            let agent = self.pipeline.default_agent(&bead.issue_type);
            if let Some(client) = self.dolt_client(&bead.repo).await {
                if let Err(e) = client.set_assignee(&bead.id, &agent).await {
                    eprintln!("[assign] failed for {}: {e}", bead.id);
                } else {
                    eprintln!(
                        "[assign] {} → {agent} (issue_type={})",
                        bead.id, bead.issue_type
                    );
                }
            }
        }

        // Phase 2: CHECK COMPLETED — poll active agents
        let completed = self.check_completed();
        summary.completed = completed.len();

        // Phase 2.5 + 2.75: AUTO-THREAD + BUILD THREAD MAP
        self.auto_thread(&beads).await;
        let thread_map = self.build_thread_map(&beads).await;

        // Phase 3: TRIAGE — score open beads, enqueue above threshold
        summary.triaged = self.triage(&beads, &thread_map);

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

                // Ensure bead.owner matches pipeline's first agent.
                // Beads created before pipeline changes may have stale owner.
                let mut dispatch_bead = bead.clone();
                let pipeline_agent = self.pipeline.default_agent(&bead.issue_type);
                if dispatch_bead.owner.as_deref() != Some(&pipeline_agent) {
                    dispatch_bead.owner = Some(pipeline_agent.clone());
                    // Update in Dolt so future scans see the correct owner
                    if let Some(client) = self.dolt_client(&entry.repo).await {
                        let _ = client.set_assignee(&bead.id, &pipeline_agent).await;
                    }
                }

                match dispatch::spawn(
                    &dispatch_bead,
                    &path,
                    true,
                    entry.generation,
                    self.provider.as_ref(),
                    self.agents_dir.as_deref(),
                    None, // compute: local subprocess (default)
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

        // Phase 5: VERIFY completed agents + pipeline decisions
        let vr = self.verify_completed(&completed, &beads, &thread_map).await;
        summary.passed += vr.passed;
        summary.failed += vr.failed;
        summary.deadlettered += vr.deadlettered;

        Ok(summary)
    }
}

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
        max_pipeline_depth: cfg.max_pipeline_depth,
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
