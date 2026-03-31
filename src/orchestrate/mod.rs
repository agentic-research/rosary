//! Feature Orchestrator — per-bead lifecycle coordinator.
//!
//! Sits between the grand orchestrator (reconciler) and worker agents.
//! Each `FeatureOrchestrator` owns one bead's full lifecycle:
//!
//! 1. **Research fan-out** — spawn parallel read-only workers
//! 2. **Synthesis** — read worker output, build contextualized prompts
//! 3. **Implementation** — dispatch dev-agent with synthesized context
//! 4. **Verification** — staging-agent review, prod-agent safety check
//! 5. **Course correction** — mid-flight messaging, session continuation
//!
//! Inspired by Claude Code's coordinator mode and agent teams architecture.
//! Key principle: "never delegate understanding" — the orchestrator reads
//! worker output and synthesizes before dispatching the next phase.
//!
//! ## Architecture
//!
//! ```text
//! Grand Orchestrator (reconciler)
//!   ├─ FeatureOrchestrator A (bead X)
//!   │    ├─ research workers (parallel, ReadOnly)
//!   │    ├─ synthesis step (reads output, builds prompt)
//!   │    ├─ dev-agent (Implement)
//!   │    ├─ staging-agent (ReadOnly, adversarial)
//!   │    └─ mailbox (mid-flight communication)
//!   ├─ FeatureOrchestrator B (bead Y)
//!   │    └─ ...
//!   └─ ...
//! ```
//!
//! Enabled via `[orchestration] mode = "hierarchical"` in config.
//! Default `mode = "flat"` preserves existing reconciler behavior.

pub mod fanout;
pub mod mailbox;
pub mod plan_gate;
pub mod synthesis;
pub mod transcript;

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::dispatch::AgentHandle;
use crate::handoff::Handoff;
use crate::store::BeadRef;

// ── Orchestrator State Machine ──────────────────────────────────

/// Current state of a feature orchestrator's lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OrchestratorState {
    /// Waiting to start — just created, no workers spawned yet.
    Idle,

    /// Parallel research phase — multiple read-only workers exploring
    /// the codebase to gather context before implementation.
    ResearchFanOut {
        /// Number of workers spawned.
        worker_count: usize,
        /// Number completed so far.
        completed: usize,
        /// Summaries from completed workers.
        findings: Vec<ResearchFinding>,
    },

    /// Synthesizing research findings into an implementation prompt.
    /// This is the "never delegate understanding" step — the orchestrator
    /// reads all worker output and builds a contextualized prompt.
    Synthesizing,

    /// Waiting for plan approval before implementation.
    /// Worker proposed a plan; orchestrator validates scope/risk.
    PlanApproval { plan: WorkerPlan },

    /// A worker agent is currently executing (dev, staging, prod, etc.).
    AwaitingWorker { agent: String, phase: u32 },

    /// Pipeline completed successfully — bead ready to close.
    Terminal,

    /// Pipeline failed — max retries exceeded or unrecoverable error.
    Failed { retries: u32, reason: String },
}

/// A finding from a research worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchFinding {
    /// Which research query this answers.
    pub query_index: usize,
    /// Human-readable summary of findings.
    pub summary: String,
    /// Key files discovered.
    pub key_files: Vec<String>,
    /// Issues or concerns flagged.
    pub issues: Vec<String>,
}

/// A worker's proposed implementation plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerPlan {
    pub steps: Vec<PlanStep>,
    pub estimated_files: Vec<String>,
    pub risk_assessment: String,
}

/// A single step in a worker's plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    pub description: String,
    pub files: Vec<String>,
    pub risk: PlanStepRisk,
}

/// Risk level for a plan step.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStepRisk {
    Low,
    Medium,
    High,
}

// ── Continue vs Spawn Decision ──────────────────────────────────

/// Decision about whether to continue an existing worker session
/// or spawn a fresh one for the next phase.
///
/// Modeled after Claude Code's explicit decision matrix:
/// - High context overlap → continue (reuse session)
/// - Low context overlap → spawn fresh
/// - Correcting a failure → continue (has error context)
/// - Verification after impl → spawn fresh (needs fresh eyes)
#[derive(Debug, Clone, Copy)]
pub enum SessionDecision {
    /// Continue the existing session (pass session_id to --resume).
    Continue,
    /// Spawn a fresh agent (no prior context).
    SpawnFresh,
}

// ── Feature Orchestrator ────────────────────────────────────────

/// Per-bead lifecycle coordinator.
///
/// Runs as an async tokio task, managing one bead through its full
/// pipeline: research → synthesis → implementation → verification.
///
/// Unlike the flat reconciler which fire-and-forgets agents, the
/// feature orchestrator can:
/// - Fan out parallel research workers
/// - Synthesize between phases (read worker output, build prompt)
/// - Send mid-flight messages via mailbox
/// - Decide continue-vs-spawn for session reuse
/// - Apply plan-mode approval gates
pub struct FeatureOrchestrator {
    /// The bead this orchestrator owns.
    pub bead_ref: BeadRef,
    /// Issue type (captured at creation for pipeline lookup).
    pub issue_type: String,
    /// Agent sequence from PipelineEngine.
    pub pipeline: Vec<String>,
    /// Current phase index into the pipeline.
    pub current_phase: usize,
    /// Orchestrator lifecycle state.
    pub state: OrchestratorState,
    /// Workspace directory for this bead.
    pub work_dir: PathBuf,
    /// Mailbox for mid-flight communication.
    pub mailbox: mailbox::Mailbox,
    /// Handle to the currently-running worker (if any).
    pub worker_handle: Option<AgentHandle>,
    /// Handoff chain from previous phases.
    pub handoff_chain: Vec<Handoff>,
    /// Transcript excerpts from previous workers (fork-style context).
    pub transcript_cache: Vec<transcript::TranscriptEntry>,
    /// Last worker's session ID (for continue-vs-spawn decision).
    pub last_session_id: Option<String>,
    /// Retry count for the current phase.
    pub retries: u32,
    /// When this orchestrator was created.
    pub created_at: DateTime<Utc>,
    /// Configuration for orchestrator behavior.
    pub config: OrchestratorBehavior,
}

/// Per-orchestrator behavior flags (from global OrchestrationConfig).
#[derive(Debug, Clone)]
pub struct OrchestratorBehavior {
    /// Enable synthesis LLM call between phases.
    pub synthesis: bool,
    /// Enable parallel research fan-out for scoping phase.
    pub fan_out: bool,
    /// Enable plan-mode approval gate before implementation.
    pub plan_gate: bool,
    /// Max parallel research workers.
    pub max_research_workers: usize,
    /// Pass transcript excerpts (fork-style) instead of just handoff JSON.
    pub fork_context: bool,
}

impl Default for OrchestratorBehavior {
    fn default() -> Self {
        Self {
            synthesis: true,
            fan_out: false,
            plan_gate: false,
            max_research_workers: 3,
            fork_context: true,
        }
    }
}

impl FeatureOrchestrator {
    /// Decide whether to continue an existing worker session or spawn fresh.
    ///
    /// Based on Claude Code's decision matrix:
    /// - Research explored the files to edit → Continue
    /// - Research was broad, implementation narrow → Spawn fresh
    /// - Correcting a failure → Continue (has error context)
    /// - Verification after implementation → Spawn fresh (fresh eyes)
    /// - Wrong approach entirely → Spawn fresh (avoid anchoring)
    pub fn session_decision(&self, next_agent: &str) -> SessionDecision {
        // No previous session → must spawn fresh
        if self.last_session_id.is_none() {
            return SessionDecision::SpawnFresh;
        }

        let current_agent = self
            .pipeline
            .get(self.current_phase)
            .map(|s| s.as_str())
            .unwrap_or("");

        match (current_agent, next_agent) {
            // Verification should always get fresh eyes
            (_, "staging-agent" | "prod-agent" | "skeptic-agent") => SessionDecision::SpawnFresh,

            // Retrying the same agent → continue (has error context)
            (current, next) if current == next => SessionDecision::Continue,

            // Scoping → dev: research explored the files, dev needs that context
            ("scoping-agent", "dev-agent") => SessionDecision::Continue,

            // Dev → staging: fresh eyes for adversarial review
            ("dev-agent", "staging-agent") => SessionDecision::SpawnFresh,

            // Default: spawn fresh for unrelated agents
            _ => SessionDecision::SpawnFresh,
        }
    }
}

// ── Persistent State ────────────────────────────────────────────

/// Persistent record of a feature orchestrator's state.
/// Stored in the backend for crash recovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorRecord {
    pub bead_ref: BeadRef,
    pub issue_type: String,
    pub state: OrchestratorState,
    pub current_phase: u32,
    pub current_agent: Option<String>,
    pub retries: u32,
    pub last_session_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
