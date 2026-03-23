//! Dispatch beads to AI agent providers for execution.
//!
//! Two entry points:
//! - `run()`: Original blocking dispatch (reads Dolt, spawns agent, waits).
//! - `spawn()`: Async dispatch returning an `AgentHandle` for the reconciliation loop.
//!
//! The `AgentProvider` trait abstracts over different AI backends (Claude, Gemini,
//! Codex, etc). `ClaudeProvider` is the default implementation.

pub mod prompt;
pub mod providers;
pub mod session;

#[cfg(test)]
pub(crate) mod tests;

// Re-export public API so callers can still use `dispatch::X`.
#[allow(unused_imports)] // API surface — not all re-exports consumed within crate yet
pub use prompt::{
    PROMPT_VERSION, build_prompt, build_system_prompt, load_agent_prompt, strip_frontmatter,
};
#[allow(unused_imports)] // API surface
pub use providers::{
    AcpCliProvider, AgentProvider, ClaudeProvider, GeminiProvider, provider_by_name,
};
#[allow(unused_imports)] // API surface
pub use session::{AgentSession, CliSession};

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::bead::Bead;
use crate::dolt::{DoltClient, DoltConfig};
use crate::scanner::expand_path;
use session::ComputeSession;

/// Permission profile for dispatched agents.
///
/// Derived from bead metadata (issue_type or explicit field), not the provider.
/// Each provider translates this to its own CLI flags.
///
/// Profiles are intentionally simple -- 3 levels. Complex per-tool rules
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
    ///
    /// rsry tools are scoped per role -- agents cannot close beads or merge
    /// workspaces. Only the reconciler/feature-agent does that.
    pub fn claude_allowed_tools(&self) -> &str {
        match self {
            // Dev/implement: can read code, edit, commit, comment on beads.
            // Cannot close beads, merge workspaces, or dispatch other agents.
            Self::Implement => concat!(
                "Read,Edit,Write,Bash(cargo *),Bash(go *),Bash(git *),Bash(task *),Glob,Grep,",
                "mcp__mache__*,",
                "mcp__rsry__rsry_bead_comment,mcp__rsry__rsry_bead_search,",
                "mcp__rsry__rsry_status,mcp__rsry__rsry_list_beads,mcp__rsry__rsry_active"
            ),
            // Review/audit: read-only code access + bead comments.
            Self::ReadOnly => concat!(
                "Read,Glob,Grep,",
                "mcp__mache__*,",
                "mcp__rsry__rsry_bead_comment,mcp__rsry__rsry_bead_search,",
                "mcp__rsry__rsry_status,mcp__rsry__rsry_list_beads"
            ),
            // Planning/triage: read code + full bead management (create, update, link).
            // Can create/update beads but still cannot close or merge.
            Self::Plan => concat!(
                "Read,Glob,Grep,",
                "mcp__mache__*,",
                "mcp__rsry__rsry_bead_create,mcp__rsry__rsry_bead_update,",
                "mcp__rsry__rsry_bead_comment,mcp__rsry__rsry_bead_search,",
                "mcp__rsry__rsry_bead_link,",
                "mcp__rsry__rsry_status,mcp__rsry__rsry_list_beads,",
                "mcp__rsry__rsry_decompose"
            ),
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
    #[allow(dead_code)] // Used by reconciler path
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

// ---------------------------------------------------------------------------
// Agent pipeline -- phase progression
// ---------------------------------------------------------------------------

/// The hardcoded agent pipeline for a given issue type.
/// Used by `default_agent()` for callers without a PipelineEngine.
/// The reconciler uses PipelineEngine (config-driven) instead.
fn agent_pipeline(issue_type: &str) -> &'static [&'static str] {
    match issue_type {
        "bug" => &["dev-agent", "staging-agent"],
        "feature" => &["dev-agent", "staging-agent", "prod-agent"],
        "task" | "chore" => &["dev-agent"],
        "review" => &["staging-agent"],
        "design" | "research" => &["architect-agent"],
        "epic" => &["pm-agent"],
        _ => &["dev-agent"],
    }
}

/// The default (first) agent for a given issue type.
pub fn default_agent(issue_type: &str) -> &'static str {
    agent_pipeline(issue_type)
        .first()
        .copied()
        .unwrap_or("dev-agent")
}

/// Derive the permission profile from the bead's issue type.
pub fn permission_profile(issue_type: &str) -> PermissionProfile {
    match issue_type {
        "review" | "survey" | "audit" => PermissionProfile::ReadOnly,
        "epic" | "plan" | "triage" => PermissionProfile::Plan,
        _ => PermissionProfile::Implement,
    }
}

/// Resolve agents_dir from config by finding the self-managed repo.
pub fn resolve_agents_dir() -> Option<PathBuf> {
    let cfg = crate::config::load_global().ok()?;
    cfg.repo
        .iter()
        .find(|r| r.self_managed)
        .map(|r| expand_path(&r.path).join("agents"))
        .filter(|p| p.exists())
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

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
    compute: Option<&dyn crate::backend::ComputeProvider>,
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

    // Write bead ID to worktree so the commit-msg hook can inject the
    // [bead-id] prefix instead of rejecting commits.
    let _ = std::fs::write(work_dir.join(".rsry-bead-id"), &bead.id);

    // Exclude dispatch artifacts from git -- these are rosary metadata,
    // not part of the agent's work. Uses .git/info/exclude (local to this
    // worktree, not committed to the repo).
    // Worktrees have .git as a file (not a dir) pointing to the real gitdir.
    // Resolve the actual info/exclude path for either layout.
    let exclude_dir = if work_dir.join(".git").is_dir() {
        work_dir.join(".git").join("info")
    } else if let Ok(gitfile) = std::fs::read_to_string(work_dir.join(".git"))
        && let Some(gitdir) = gitfile.trim().strip_prefix("gitdir: ")
    {
        std::path::PathBuf::from(gitdir).join("info")
    } else {
        work_dir.join(".git").join("info") // fallback
    };
    let _ = (|| {
        use std::io::Write;
        std::fs::create_dir_all(&exclude_dir)?;
        let exclude_path = exclude_dir.join("exclude");
        let existing = std::fs::read_to_string(&exclude_path).unwrap_or_default();
        if !existing.lines().any(|l| l.trim() == ".rsry-*") {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&exclude_path)?;
            if !existing.is_empty() && !existing.ends_with('\n') {
                writeln!(file)?;
            }
            writeln!(file, "# rosary dispatch artifacts")?;
            writeln!(file, ".rsry-*")?;
        }
        Ok::<(), std::io::Error>(())
    })();

    // Set core.hooksPath to ~/.rsry/hooks/ so the worktree uses our
    // inject hook instead of the main repo's pre-commit framework wrapper.
    // Only set in isolated worktrees to avoid mutating the user's main repo config.
    if isolate
        && let Some(hooks) = dirs_next::home_dir()
            .map(|h| h.join(".rsry/hooks"))
            .filter(|p| p.exists())
    {
        let _ = std::process::Command::new("git")
            .args(["config", "core.hooksPath", &hooks.to_string_lossy()])
            .current_dir(&work_dir)
            .output();
    }

    let prompt = build_prompt(
        bead,
        &path.display().to_string(),
        Some(&work_dir),
        bead.owner.as_deref(),
    );

    // Build agent-aware system prompt from bead.owner
    let system_prompt = build_system_prompt(bead.owner.as_deref(), agents_dir);

    // Agent-specific permission override: scoping-agent is always ReadOnly
    let permissions = match bead.owner.as_deref() {
        Some("scoping-agent") => PermissionProfile::ReadOnly,
        Some("staging-agent") => PermissionProfile::ReadOnly,
        Some("pm-agent") => PermissionProfile::Plan,
        Some("architect-agent") => PermissionProfile::Plan,
        _ => permission_profile(&bead.issue_type),
    };

    let agent_label = bead.owner.as_deref().unwrap_or("generic");
    eprintln!(
        "[dispatch] {} -> {} (agent={}, perms={:?})",
        bead.id,
        provider.name(),
        agent_label,
        permissions
    );

    let session: Box<dyn AgentSession> = if let Some(compute) = compute {
        // Container dispatch: build command, provision, exec, destroy.
        // Synchronous -- spawn() blocks for exec duration. Session is already resolved.
        let (bin, args) = provider.build_command(&prompt, &permissions, &system_prompt);
        anyhow::ensure!(
            !bin.is_empty(),
            "{} does not support build_command()",
            provider.name()
        );

        let opts = crate::backend::ProvisionOpts::new(&bead.id, &repo_name);
        let exec_handle = compute
            .provision(&opts)
            .await
            .with_context(|| format!("provisioning {} for {}", compute.name(), bead.id))?;

        let mut cmd: Vec<String> = vec![bin];
        cmd.extend(args);

        let bead_id_clone = bead.id.clone();
        let handle_id = exec_handle.id.clone();
        let _backend_name = compute.name().to_string();

        // Background task: exec -> destroy (always, even on failure)
        let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
        // We need to move the exec_handle into the spawned task, but
        // compute is borrowed. Use the ExecHandle + backend name to
        // call docker CLI directly in the task. This is a known limitation --
        // the real fix is making ComputeProvider: 'static + Clone.
        // For now, exec synchronously before spawning (same as before but
        // with proper cleanup).
        let cmd_refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
        let exec_result = compute.exec(&exec_handle, &cmd_refs).await;

        // Always destroy, even on exec failure
        if let Err(e) = compute.destroy(&exec_handle).await {
            eprintln!("[dispatch] cleanup {}: {e}", handle_id);
        }

        let success = match exec_result {
            Ok(r) => {
                let ok = r.success();
                eprintln!(
                    "[dispatch] {} container {} exited {}",
                    bead_id_clone,
                    handle_id,
                    if ok { "ok" } else { "fail" }
                );
                ok
            }
            Err(e) => {
                eprintln!("[dispatch] {} exec failed: {e}", bead_id_clone);
                false
            }
        };

        // Send result -- if rx was dropped (kill), this is a no-op
        let _ = tx.send(success);

        Box::new(ComputeSession {
            rx: Some(rx),
            result: None,
        })
    } else {
        // Local dispatch: spawn agent process directly (existing behavior)
        provider
            .spawn_agent(&prompt, &work_dir, &permissions, &system_prompt)
            .with_context(|| format!("spawning {} for {}", provider.name(), bead.id))?
    };

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

/// Original blocking dispatch -- reads Dolt, spawns agent, waits for completion.
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
        &ClaudeProvider::default(),
        agents_dir.as_deref(),
        None, // compute: local subprocess (default)
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
            eprintln!("[dispatch] {bead_id} failed but left commits -- marking blocked for review");
            let _ = client
                .add_comment(
                    bead_id,
                    "agent",
                    "Agent exited with failure but produced commits. Needs human review.",
                )
                .await;
            let _ = client.update_status(bead_id, "blocked").await;
        } else {
            eprintln!("[dispatch] {bead_id} crashed silently -- no commits, no artifacts");
            let _ = client
                .add_comment(
                    bead_id,
                    "agent",
                    "Agent crashed silently -- no commits produced. Returning to open for retry.",
                )
                .await;
            let _ = client.update_status(bead_id, "open").await;
        }
    }

    Ok(())
}
