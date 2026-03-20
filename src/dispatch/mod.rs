//! Dispatch beads to AI agent providers for execution.
//!
//! Two entry points:
//! - `run()`: Original blocking dispatch (reads Dolt, spawns agent, waits).
//! - `spawn()`: Async dispatch returning an `AgentHandle` for the reconciliation loop.
//!
//! The `AgentProvider` trait abstracts over different AI backends (Claude, Gemini,
//! Codex, etc). `ClaudeProvider` is the default implementation.

mod pipeline;
mod prompt;
mod providers;
mod session;
#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::bead::Bead;
use crate::dolt::{DoltClient, DoltConfig};
use crate::scanner::expand_path;

// Re-exports — public API of this module.
// Some items are currently only consumed by tests but are part of the public
// API surface (other crates/binaries may use them).
#[allow(unused_imports)]
pub use pipeline::{agent_pipeline, default_agent, next_agent, resolve_agents_dir};
#[allow(unused_imports)]
pub use prompt::{
    PROMPT_VERSION, build_prompt, build_system_prompt, load_agent_prompt, strip_frontmatter,
};
#[allow(unused_imports)]
pub use providers::{
    AcpCliProvider, AgentProvider, ClaudeProvider, GeminiProvider, provider_by_name,
};
#[allow(unused_imports)]
pub use session::{AgentSession, CliSession};

/// Permission profile for dispatched agents.
///
/// Derived from bead metadata (issue_type or explicit field), not the provider.
/// Each provider translates this to its own CLI flags.
///
/// Profiles are intentionally simple — 3 levels. Complex per-tool rules
/// belong in a schema/config file, not in Rust match arms.
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionProfile {
    /// Read + analyze only. For review, survey, audit.
    ReadOnly,
    /// Read + edit + test + commit. For bug, task, feature.
    #[default]
    Implement,
    /// Bead/project management via MCP. For planning, triage.
    Plan,
}

impl PermissionProfile {
    /// Claude `--allowedTools` flag value.
    pub fn claude_allowed_tools(&self) -> &str {
        match self {
            Self::ReadOnly => "Read,Glob,Grep,mcp__mache__*,mcp__rsry__*",
            Self::Implement => {
                "Read,Edit,Write,Bash(cargo *),Bash(go *),Bash(git *),Bash(task *),Glob,Grep,mcp__mache__*,mcp__rsry__*"
            }
            Self::Plan => "Read,Glob,Grep,mcp__mache__*,mcp__rsry__*",
        }
    }

    /// Gemini `--approval-mode` flag value.
    pub fn gemini_approval_mode(&self) -> &str {
        match self {
            Self::ReadOnly => "plan",
            Self::Implement => "auto_edit",
            Self::Plan => "plan",
        }
    }
}

/// Filename for the agent stdout stream log within a workspace.
pub const STREAM_LOG_FILENAME: &str = ".rsry-stream.jsonl";

/// Handle to a running agent session.
pub struct AgentHandle {
    #[allow(dead_code)]
    pub bead_id: String,
    #[allow(dead_code)]
    pub generation: u64,
    pub session: Box<dyn AgentSession>,
    pub work_dir: PathBuf,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub workspace: Option<crate::workspace::Workspace>,
    /// Claude Code session ID (from --output-format json). Set after capture.
    /// Enables `--resume` on retry to preserve agent context across failures.
    #[allow(dead_code)]
    pub session_id: Option<String>,
    /// Path to the workspace directory (jj workspace or git worktree).
    /// Recorded in DispatchRecord for resume and debugging.
    #[allow(dead_code)]
    pub workspace_path: Option<String>,
    /// Path to the JSONL stream log capturing agent stdout.
    /// Contains init, assistant, and result events from `--output-format json`.
    #[allow(dead_code)]
    pub log_path: Option<PathBuf>,
}

impl AgentHandle {
    /// Non-blocking check: has the agent completed? Returns Some(success).
    pub fn try_wait(&mut self) -> Result<Option<bool>> {
        self.session.try_wait()
    }

    /// Block until the agent completes. Returns success.
    pub async fn wait(&mut self) -> Result<bool> {
        self.session.wait().await
    }

    /// Kill the agent.
    pub fn kill(&mut self) -> Result<()> {
        self.session.kill()
    }

    /// Process ID (if applicable).
    pub fn pid(&self) -> Option<u32> {
        self.session.pid()
    }

    /// Set the session ID (captured from agent output after spawn).
    /// Enables `--resume` on retry to preserve agent context.
    #[allow(dead_code)]
    pub fn set_session_id(&mut self, session_id: String) {
        self.session_id = Some(session_id);
    }

    /// Elapsed time since dispatch.
    pub fn elapsed(&self) -> chrono::Duration {
        chrono::Utc::now() - self.started_at
    }
}

/// Spawn an AI agent for a bead. Returns a handle without waiting.
///
/// This is the async entry point for the reconciliation loop.
/// The `provider` argument controls which AI backend is used.
/// The `agents_dir` enables agent-aware system prompts from definition files.
///
/// Isolation uses `Workspace` which tries jj first, git worktree second,
/// then falls back to in-place if neither is available.
pub async fn spawn(
    bead: &Bead,
    repo_path: &Path,
    isolate: bool,
    generation: u64,
    provider: &dyn AgentProvider,
    agents_dir: Option<&Path>,
) -> Result<AgentHandle> {
    let path = expand_path(repo_path);
    let repo_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let workspace = crate::workspace::Workspace::create(&bead.id, &repo_name, &path, isolate)
        .await
        .with_context(|| format!("creating workspace for {}", bead.id))?;

    let work_dir = workspace.work_dir.clone();
    let prompt = build_prompt(
        bead,
        &path.display().to_string(),
        Some(&work_dir),
        bead.owner.as_deref(),
    );

    // Build agent-aware system prompt from bead.owner
    let system_prompt = build_system_prompt(bead.owner.as_deref(), agents_dir);

    // Permissions come from the bead, not the provider.
    let permissions = match bead.issue_type.as_str() {
        "review" | "survey" | "audit" => PermissionProfile::ReadOnly,
        "epic" | "plan" | "triage" => PermissionProfile::Plan,
        _ => PermissionProfile::Implement,
    };

    let agent_label = bead.owner.as_deref().unwrap_or("generic");
    eprintln!(
        "[dispatch] {} → {} (agent={}, perms={:?})",
        bead.id,
        provider.name(),
        agent_label,
        permissions
    );

    let session = provider
        .spawn_agent(&prompt, &work_dir, &permissions, &system_prompt)
        .with_context(|| format!("spawning {} for {}", provider.name(), bead.id))?;

    // Record workspace path for dispatch tracking (resume + debugging).
    // This is the isolated work_dir, not the original repo root.
    let workspace_path = if work_dir != path {
        Some(work_dir.display().to_string())
    } else {
        None
    };

    let log_path = work_dir.join(STREAM_LOG_FILENAME);

    Ok(AgentHandle {
        bead_id: bead.id.clone(),
        generation,
        session,
        work_dir,
        started_at: chrono::Utc::now(),
        workspace: Some(workspace),
        session_id: None,
        workspace_path,
        log_path: Some(log_path),
    })
}

/// Original blocking dispatch — reads Dolt, spawns agent, waits for completion.
/// Kept for `rsry dispatch` CLI command.
pub async fn run(bead_id: &str, repo_path: &Path, isolate: bool) -> Result<()> {
    let path = expand_path(repo_path);
    let beads_dir = path.join(".beads");

    let config = DoltConfig::from_beads_dir(&beads_dir)?;
    let client = DoltClient::connect(&config).await?;

    let bead = client
        .get_bead(bead_id, &path.display().to_string())
        .await?
        .ok_or_else(|| anyhow::anyhow!("bead {bead_id} not found"))?;

    client.update_status(bead_id, "dispatched").await?;

    let agents_dir = resolve_agents_dir();
    let mut handle = spawn(
        &bead,
        &path,
        isolate,
        bead.generation(),
        &ClaudeProvider,
        agents_dir.as_deref(),
    )
    .await?;
    let success = handle.wait().await?;

    if success {
        eprintln!("[dispatch] {bead_id} completed successfully");
    } else {
        // Check if agent produced any artifacts (commits in worktree)
        let has_commits = if let Some(ref ws_path) = handle.workspace_path {
            std::process::Command::new("git")
                .args(["log", "--oneline", "-1", "HEAD", "--not", "HEAD~1"])
                .current_dir(ws_path)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        } else {
            false
        };

        if has_commits {
            eprintln!("[dispatch] {bead_id} failed but left commits — marking blocked for review");
            let _ = client
                .add_comment(
                    bead_id,
                    "agent",
                    "Agent exited with failure but produced commits. Needs human review.",
                )
                .await;
            let _ = client.update_status(bead_id, "blocked").await;
        } else {
            eprintln!("[dispatch] {bead_id} crashed silently — no commits, no artifacts");
            let _ = client
                .add_comment(
                    bead_id,
                    "agent",
                    "Agent crashed silently — no commits produced. Returning to open for retry.",
                )
                .await;
            let _ = client.update_status(bead_id, "open").await;
        }
    }

    Ok(())
}
