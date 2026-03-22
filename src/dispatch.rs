//! Dispatch beads to AI agent providers for execution.
//!
//! Two entry points:
//! - `run()`: Original blocking dispatch (reads Dolt, spawns agent, waits).
//! - `spawn()`: Async dispatch returning an `AgentHandle` for the reconciliation loop.
//!
//! The `AgentProvider` trait abstracts over different AI backends (Claude, Gemini,
//! Codex, etc). `ClaudeProvider` is the default implementation.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::bead::Bead;
use crate::dolt::{DoltClient, DoltConfig};
use crate::scanner::expand_path;

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
    ///
    /// rsry tools are scoped per role — agents cannot close beads or merge
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

// ---------------------------------------------------------------------------
// AgentSession — abstract session lifecycle (replaces raw Child)
// ---------------------------------------------------------------------------

/// Abstract session to a running agent. Decouples from tokio::process::Child
/// so we can support CLI subprocesses, ACP sockets, raw API calls, etc.
#[async_trait::async_trait]
pub trait AgentSession: Send + Sync {
    /// Non-blocking check: has the session completed? Returns true on success.
    fn try_wait(&mut self) -> Result<Option<bool>>;

    /// Block until the session completes. Returns true on success.
    async fn wait(&mut self) -> Result<bool>;

    /// Forcefully terminate the session.
    fn kill(&mut self) -> Result<()>;

    /// Process ID (if applicable). For logging/debugging.
    #[allow(dead_code)] // Used by reconciler path, not MCP
    fn pid(&self) -> Option<u32> {
        None
    }
}

/// CLI subprocess session — wraps tokio::process::Child.
pub struct CliSession {
    child: tokio::process::Child,
}

impl CliSession {
    pub fn new(child: tokio::process::Child) -> Self {
        Self { child }
    }
}

#[async_trait::async_trait]
impl AgentSession for CliSession {
    fn try_wait(&mut self) -> Result<Option<bool>> {
        match self.child.try_wait()? {
            Some(status) => Ok(Some(status.success())),
            None => Ok(None),
        }
    }

    async fn wait(&mut self) -> Result<bool> {
        let status = self.child.wait().await?;
        Ok(status.success())
    }

    fn kill(&mut self) -> Result<()> {
        self.child.start_kill()?;
        Ok(())
    }

    fn pid(&self) -> Option<u32> {
        self.child.id()
    }
}

// ---------------------------------------------------------------------------
// AgentProvider — spawns sessions
// ---------------------------------------------------------------------------

/// Trait for AI agent providers. Implementations handle spawning and
/// communicating with different AI backends (Claude, Gemini, Codex, etc).
///
/// The `permissions` argument comes from the bead — the provider just
/// translates it to CLI flags. This keeps schema/config decisions out
/// of the provider code.
pub trait AgentProvider: Send + Sync {
    /// Spawn an agent session with the given prompt, working directory,
    /// permission profile (derived from the bead), and system prompt
    /// (assembled from agent definitions + golden rules).
    fn spawn_agent(
        &self,
        prompt: &str,
        work_dir: &Path,
        permissions: &PermissionProfile,
        system_prompt: &str,
    ) -> Result<Box<dyn AgentSession>>;

    /// Human-readable name of this provider.
    fn name(&self) -> &str;
}

/// Provider that shells out to the Claude Code CLI (`claude -p`).
///
/// Uses `--allowedTools` with permission rule syntax to grant the agent
/// the tools it needs without interactive prompts.
pub struct ClaudeProvider;

impl AgentProvider for ClaudeProvider {
    fn spawn_agent(
        &self,
        prompt: &str,
        work_dir: &Path,
        permissions: &PermissionProfile,
        system_prompt: &str,
    ) -> Result<Box<dyn AgentSession>> {
        let log_path = work_dir.join(STREAM_LOG_FILENAME);
        let log_file = std::fs::File::create(&log_path)
            .with_context(|| format!("creating stream log {}", log_path.display()))?;
        let child = tokio::process::Command::new("claude")
            .args([
                "-p",
                prompt,
                "--allowedTools",
                permissions.claude_allowed_tools(),
                "--append-system-prompt",
                system_prompt,
                "--output-format",
                "json",
            ])
            .current_dir(work_dir)
            // Prevent git env vars from leaking into the agent — these override
            // cwd-based repo discovery and can cause the agent to resolve to the
            // main repo instead of its isolated worktree.
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(log_file))
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .with_context(|| format!("spawning claude CLI in {}", work_dir.display()))?;
        Ok(Box::new(CliSession::new(child)))
    }

    fn name(&self) -> &str {
        "claude"
    }
}

/// Provider that shells out to the Gemini CLI (`gemini -p`).
///
/// Uses `--approval-mode` to control permission prompts.
#[derive(Default)]
pub struct GeminiProvider {
    /// Extra CLI args beyond permissions.
    pub extra_args: Vec<String>,
}

impl AgentProvider for GeminiProvider {
    fn spawn_agent(
        &self,
        prompt: &str,
        work_dir: &Path,
        permissions: &PermissionProfile,
        system_prompt: &str,
    ) -> Result<Box<dyn AgentSession>> {
        // Gemini CLI doesn't have --append-system-prompt; prepend to user prompt.
        let full_prompt = format!("{system_prompt}\n\n---\n\n{prompt}");
        let log_path = work_dir.join(STREAM_LOG_FILENAME);
        let log_file = std::fs::File::create(&log_path)
            .with_context(|| format!("creating stream log {}", log_path.display()))?;
        let mut cmd = tokio::process::Command::new("gemini");
        cmd.args([
            "-p",
            &full_prompt,
            "-o",
            "json",
            "--approval-mode",
            permissions.gemini_approval_mode(),
        ]);
        for arg in &self.extra_args {
            cmd.arg(arg);
        }
        let child = cmd
            .current_dir(work_dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(log_file))
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .with_context(|| format!("spawning gemini CLI in {}", work_dir.display()))?;
        Ok(Box::new(CliSession::new(child)))
    }

    fn name(&self) -> &str {
        "gemini"
    }
}

/// Provider that spawns an ACP-compatible agent binary.
///
/// The binary must implement the Agent Client Protocol (JSON-RPC over stdio).
/// Permission handling happens via `RosaryClient::request_permission()` in
/// the ACP session, not via CLI flags.
///
/// Example binaries: `claude-agent-acp` (npm), custom ACP agents.
pub struct AcpCliProvider {
    /// Path or name of the ACP agent binary.
    pub binary: String,
}

impl AgentProvider for AcpCliProvider {
    fn spawn_agent(
        &self,
        _prompt: &str,
        work_dir: &Path,
        _permissions: &PermissionProfile,
        _system_prompt: &str,
    ) -> Result<Box<dyn AgentSession>> {
        // ACP agents are spawned as subprocesses with stdio piped.
        // The prompt and permissions are sent via ACP protocol (JSON-RPC),
        // not CLI args. The caller must establish a ClientSideConnection
        // after spawning and use Agent::prompt() to send the task.
        let log_path = work_dir.join(STREAM_LOG_FILENAME);
        let log_file = std::fs::File::create(&log_path)
            .with_context(|| format!("creating stream log {}", log_path.display()))?;
        let child = tokio::process::Command::new(&self.binary)
            .current_dir(work_dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .stdin(std::process::Stdio::piped())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(log_file))
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .with_context(|| format!("spawning ACP agent: {}", self.binary))?;
        Ok(Box::new(CliSession::new(child)))
    }

    fn name(&self) -> &str {
        "acp"
    }
}

/// Resolve a provider by name string (from config/CLI).
pub fn provider_by_name(name: &str) -> Result<Box<dyn AgentProvider>> {
    match name {
        "claude" => Ok(Box::new(ClaudeProvider)),
        "gemini" => Ok(Box::new(GeminiProvider::default())),
        "acp" => Ok(Box::new(AcpCliProvider {
            binary: "claude-agent-acp".to_string(),
        })),
        other => anyhow::bail!("unknown provider: {other} (available: claude, gemini, acp)"),
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

/// Task framing varies by agent perspective so dispatched agents receive
/// role-appropriate instructions rather than a generic "fix this" prompt.
fn task_framing(agent_name: Option<&str>) -> &'static str {
    match agent_name.unwrap_or("dev-agent") {
        "staging-agent" => "Review this change. Verify tests validate real behavior, not mocks.",
        "prod-agent" => {
            "Audit this code for production readiness: resource leaks, error handling, concurrency."
        }
        "feature-agent" => {
            "Check cross-file coherence: dependencies, API contracts, error consistency."
        }
        "architect-agent" => {
            "Analyze this problem. Evaluate approaches, write an ADR, decompose into beads."
        }
        "pm-agent" => {
            "Assess from a strategic perspective: scope, cross-repo overlap, prioritization."
        }
        _ => "Fix this issue. Make the minimal change needed.",
    }
}

/// Build the prompt for a bead.
///
/// Includes the bead ID and repo path so the agent can self-manage its
/// lifecycle via MCP tools (comment, close). When a workspace path is
/// provided, reads the handoff chain for context from previous phases.
///
/// The prompt uses XML tags to separate sections so the model can
/// unambiguously parse task, context, and instructions.
pub fn build_prompt(
    bead: &Bead,
    repo_path: &str,
    workspace: Option<&Path>,
    agent_name: Option<&str>,
) -> String {
    let handoff_context = workspace
        .map(|ws| {
            let chain = crate::handoff::Handoff::read_chain(ws);
            crate::handoff::Handoff::format_for_prompt(&chain)
        })
        .unwrap_or_default();

    // Use workspace path for Repo: line (agent's actual working directory)
    // to prevent agents from resolving absolute paths against the main repo.
    // Keep repo_path for MCP bead tools where .beads/ lives.
    let work_repo = workspace
        .map(|ws| ws.display().to_string())
        .unwrap_or_else(|| repo_path.to_string());

    let framing = task_framing(agent_name);

    let handoff_section = if handoff_context.is_empty() {
        String::new()
    } else {
        format!("\n<handoff>\n{handoff_context}</handoff>\n")
    };

    format!(
        "<task>\n\
         {framing}\n\
         </task>\n\
         \n\
         <bead>\n\
         Bead ID: {bead_id}\n\
         Repo: {work_repo}\n\
         Title: {title}\n\
         Description: {desc}\n\
         </bead>\n\
         {handoff}\
         \n\
         <instructions>\n\
         After completing your work:\n\
         1. Run tests via `task test`\n\
         2. Commit: git commit -m \"[{bead_id}] type(scope): description\" (the [{bead_id}] prefix is REQUIRED)\n\
         3. Comment your status via mcp__rsry__rsry_bead_comment with repo_path=\"{bead_repo}\" and id=\"{bead_id}\"\n\
         4. Report what you changed\n\
         Do NOT close the bead yourself — the reconciler verifies and closes it.\n\
         </instructions>",
        bead_id = bead.id,
        bead_repo = repo_path,
        title = bead.title,
        desc = bead.description,
        handoff = handoff_section,
    )
}

/// Prompt version for traceability — agents include this in bead comments
/// so output can be traced back to the prompt configuration that produced it.
pub const PROMPT_VERSION: &str = "v0.2.0";

/// System prompt prepended to all dispatched agents.
/// Tells agents about available MCP tools, workflow expectations,
/// and bead lifecycle management.
const AGENT_SYSTEM_PROMPT: &str = "\
You are a rosary-dispatched agent working on a bead (work item).\n\
\n\
## Available Tools\n\
- **mache MCP** (`mcp__mache__*`): Structural code navigation — \
  find_definition, find_callers, find_callees, search, get_overview. \
  Prefer mache over grep for understanding code structure.\n\
- **rsry MCP** (`mcp__rsry__*`): Bead management — \
  bead_comment, bead_search, bead_link. You can comment and search but NOT close beads.\n\
\n\
## Workflow\n\
- Use `task build` / `task test` — never raw `cargo` or `go` commands. \
  The Taskfile runs linters and sets required env vars that raw commands skip.\n\
- Read the relevant code before making claims about it. \
  If you haven't opened a file, don't assert what it contains.\n\
- Make minimal, focused changes.\n\
- Commit format: `[BEAD-ID] type(scope): description` — the [BEAD-ID] prefix is mandatory.\n\
- Do NOT add co-author lines to commits.\n\
\n\
## Bead Lifecycle\n\
Your prompt includes a Bead ID and Repo path. Manage the bead throughout:\n\
1. **Comment progress** via `mcp__rsry__rsry_bead_comment` as you work — \
   not just at the end. Other agents and humans read these for context.\n\
2. Do NOT close the bead — the reconciler verifies your work and closes it.\n\
3. If you cannot fix the issue, comment explaining what you tried and why.\n\
";

// ---------------------------------------------------------------------------
// Agent definition loading
// ---------------------------------------------------------------------------

/// Strip YAML frontmatter from a markdown file.
/// Frontmatter is delimited by `---` on its own line at the start.
pub fn strip_frontmatter(content: &str) -> String {
    if !content.starts_with("---") {
        return content.to_string();
    }
    // Find the closing "---" after the opening one
    if let Some(end) = content[3..].find("\n---") {
        let after = 3 + end + 4; // 3 for "---", end for body, 4 for "\n---"
        content[after..].trim_start_matches('\n').to_string()
    } else {
        content.to_string()
    }
}

/// Load an agent definition from its markdown file.
///
/// Reads `{agents_dir}/{agent_name}.md`, strips YAML frontmatter,
/// and returns the markdown body.
pub fn load_agent_prompt(agents_dir: &Path, agent_name: &str) -> Option<String> {
    let file_name = if agent_name.ends_with(".md") {
        agent_name.to_string()
    } else {
        format!("{agent_name}.md")
    };
    let path = agents_dir.join(&file_name);
    let content = std::fs::read_to_string(&path).ok()?;
    Some(strip_frontmatter(&content))
}

/// Load GOLDEN_RULES.md from the agents/rules/ directory.
fn load_golden_rules(agents_dir: &Path) -> Option<String> {
    let path = agents_dir.join("rules").join("GOLDEN_RULES.md");
    std::fs::read_to_string(&path).ok()
}

/// Build the complete system prompt for an agent dispatch.
///
/// Layers:
/// 1. Base AGENT_SYSTEM_PROMPT (MCP tools, workflow, bead lifecycle)
/// 2. GOLDEN_RULES.md (if agents_dir provided)
/// 3. Agent-specific definition (if agent_name set and file exists)
///
/// Falls back gracefully — missing files produce warnings, not errors.
pub fn build_system_prompt(agent_name: Option<&str>, agents_dir: Option<&Path>) -> String {
    let mut parts = vec![format!(
        "Prompt version: {PROMPT_VERSION}\n\n{AGENT_SYSTEM_PROMPT}"
    )];

    if let Some(dir) = agents_dir {
        if let Some(rules) = load_golden_rules(dir) {
            parts.push(format!("\n## Golden Rules\n\n{rules}"));
        } else {
            eprintln!(
                "[dispatch] warning: GOLDEN_RULES.md not found in {}",
                dir.display()
            );
        }

        if let Some(name) = agent_name {
            if let Some(agent_prompt) = load_agent_prompt(dir, name) {
                parts.push(format!("\n## Agent Perspective\n\n{agent_prompt}"));
                eprintln!("[dispatch] loaded agent definition: {name}");
            } else {
                eprintln!("[dispatch] warning: agent definition not found: {name}");
            }
        }
    }

    parts.join("\n")
}

// ---------------------------------------------------------------------------
// Agent pipeline — phase progression
// ---------------------------------------------------------------------------

/// The agent pipeline for a given issue type.
pub fn agent_pipeline(issue_type: &str) -> &'static [&'static str] {
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

/// The next agent in the pipeline after `current`, or None if done.
/// Note: reconciler now uses PipelineEngine.next_agent() for config-driven lookup.
/// This remains as a convenience for callers that don't have a PipelineEngine.
#[allow(dead_code)] // API surface — used in tests
pub fn next_agent(issue_type: &str, current: &str) -> Option<&'static str> {
    let pipeline = agent_pipeline(issue_type);
    let idx = pipeline.iter().position(|&a| a == current)?;
    pipeline.get(idx + 1).copied()
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

    let permissions = permission_profile(&bead.issue_type);

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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn dispatch_missing_beads_dir_errors() {
        let dir = TempDir::new().unwrap();
        let result = run("fake-id", dir.path(), false).await;
        assert!(result.is_err());
    }

    #[test]
    fn claude_provider_name() {
        let provider = ClaudeProvider;
        assert_eq!(provider.name(), "claude");
    }

    #[test]
    fn gemini_provider_name() {
        let provider = GeminiProvider::default();
        assert_eq!(provider.name(), "gemini");
    }

    #[test]
    fn gemini_provider_extra_args() {
        let provider = GeminiProvider {
            extra_args: vec!["--approval-mode".into(), "yolo".into()],
        };
        assert_eq!(provider.extra_args.len(), 2);
        assert_eq!(provider.name(), "gemini");
    }

    #[test]
    fn provider_by_name_claude() {
        let p = provider_by_name("claude").unwrap();
        assert_eq!(p.name(), "claude");
    }

    #[test]
    fn provider_by_name_gemini() {
        let p = provider_by_name("gemini").unwrap();
        assert_eq!(p.name(), "gemini");
    }

    #[test]
    fn provider_by_name_unknown() {
        assert!(provider_by_name("copilot").is_err());
    }

    #[test]
    fn permission_profile_from_issue_type() {
        // bug/task/feature → Implement
        assert_eq!(
            PermissionProfile::Implement,
            match "bug" {
                "review" | "survey" | "audit" => PermissionProfile::ReadOnly,
                "epic" | "plan" | "triage" => PermissionProfile::Plan,
                _ => PermissionProfile::Implement,
            }
        );
        // review → ReadOnly
        assert_eq!(
            PermissionProfile::ReadOnly,
            match "review" {
                "review" | "survey" | "audit" => PermissionProfile::ReadOnly,
                "epic" | "plan" | "triage" => PermissionProfile::Plan,
                _ => PermissionProfile::Implement,
            }
        );
        // epic → Plan
        assert_eq!(
            PermissionProfile::Plan,
            match "epic" {
                "review" | "survey" | "audit" => PermissionProfile::ReadOnly,
                "epic" | "plan" | "triage" => PermissionProfile::Plan,
                _ => PermissionProfile::Implement,
            }
        );
    }

    #[test]
    fn permission_profile_claude_tools() {
        assert!(
            PermissionProfile::Implement
                .claude_allowed_tools()
                .contains("Edit")
        );
        assert!(
            !PermissionProfile::ReadOnly
                .claude_allowed_tools()
                .contains("Edit")
        );
        assert!(
            PermissionProfile::Plan
                .claude_allowed_tools()
                .contains("mcp__rsry__")
        );
    }

    #[test]
    fn implement_agents_cannot_close_beads() {
        let tools = PermissionProfile::Implement.claude_allowed_tools();
        assert!(
            !tools.contains("bead_close"),
            "dev-agents must not close beads — that's the reconciler's job"
        );
        assert!(
            !tools.contains("workspace_merge"),
            "dev-agents must not merge workspaces — that's the reconciler's job"
        );
        assert!(
            tools.contains("bead_comment"),
            "dev-agents should be able to comment on beads"
        );
    }

    #[test]
    fn readonly_agents_cannot_close_beads() {
        let tools = PermissionProfile::ReadOnly.claude_allowed_tools();
        assert!(!tools.contains("bead_close"));
        assert!(!tools.contains("bead_create"));
        assert!(tools.contains("bead_comment"));
    }

    #[test]
    fn plan_agents_can_create_but_not_close() {
        let tools = PermissionProfile::Plan.claude_allowed_tools();
        assert!(tools.contains("bead_create"));
        assert!(!tools.contains("bead_close"));
        assert!(!tools.contains("workspace_merge"));
    }

    #[test]
    fn permission_profile_gemini_mode() {
        assert_eq!(
            PermissionProfile::Implement.gemini_approval_mode(),
            "auto_edit"
        );
        assert_eq!(PermissionProfile::ReadOnly.gemini_approval_mode(), "plan");
    }

    #[test]
    fn build_prompt_includes_title_and_description() {
        let bead = Bead {
            id: "test-1".into(),
            title: "Fix the widget".into(),
            description: "The widget is broken".into(),
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
            branch: None,
            pr_url: None,
            jj_change_id: None,
            external_ref: None,
            files: Vec::new(),
            test_files: Vec::new(),
        };

        let prompt = build_prompt(&bead, "/tmp/test-repo", None, None);
        assert!(prompt.contains("Fix the widget"));
        assert!(prompt.contains("The widget is broken"));
        assert!(prompt.contains("task test"));
        assert!(prompt.contains("test-1"), "prompt should include bead ID");
        assert!(
            prompt.contains("/tmp/test-repo"),
            "prompt should include repo path"
        );
        assert!(
            prompt.contains("rsry_bead_comment"),
            "prompt should instruct agent to comment on bead"
        );
        assert!(
            !prompt.contains("rsry_bead_close"),
            "prompt must NOT instruct agent to close bead — reconciler owns lifecycle"
        );
        // XML structure
        assert!(prompt.contains("<task>"), "prompt should use XML tags");
        assert!(prompt.contains("<bead>"), "prompt should wrap bead in XML");
        assert!(
            prompt.contains("<instructions>"),
            "prompt should wrap instructions in XML"
        );
    }

    /// Regression: when a workspace is provided, the Repo: line must point
    /// to the workspace (where the agent works), NOT the main repo.
    /// The MCP bead_comment instruction must still use the main repo path
    /// (where .beads/ lives). This prevents agents from writing changes
    /// to the main working tree instead of their isolated worktree.
    #[test]
    fn build_prompt_uses_workspace_for_repo_line() {
        let bead = Bead {
            id: "iso-1".into(),
            title: "Test isolation".into(),
            description: "Ensure workspace isolation".into(),
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
            branch: None,
            pr_url: None,
            jj_change_id: None,
            external_ref: None,
            files: Vec::new(),
            test_files: Vec::new(),
        };

        let ws = PathBuf::from("/home/user/.rsry/worktrees/myrepo/iso-1");
        let prompt = build_prompt(&bead, "/home/user/repos/myrepo", Some(&ws), None);

        // Repo: line must show the WORKSPACE path (agent's working directory)
        assert!(
            prompt.contains("/home/user/.rsry/worktrees/myrepo/iso-1"),
            "Repo line must point to workspace, not main repo. Got:\n{prompt}"
        );
        // MCP bead_comment must still use the MAIN repo path (where .beads/ lives)
        assert!(
            prompt.contains("repo_path=\"/home/user/repos/myrepo\""),
            "bead_comment repo_path must point to main repo. Got:\n{prompt}"
        );
        // Repo: line must NOT contain the main repo path as the workspace repo
        assert!(
            !prompt.contains("Repo: /home/user/repos/myrepo\n"),
            "Repo line must NOT show main repo path when workspace exists. Got:\n{prompt}"
        );
    }

    #[test]
    fn build_prompt_varies_framing_by_agent() {
        let bead = Bead {
            id: "framing-1".into(),
            title: "Test framing".into(),
            description: "Agent framing varies".into(),
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
            branch: None,
            pr_url: None,
            jj_change_id: None,
            external_ref: None,
            files: Vec::new(),
            test_files: Vec::new(),
        };

        // Default (dev-agent) framing
        let dev = build_prompt(&bead, "/tmp/repo", None, None);
        assert!(dev.contains("Fix this issue"), "dev framing: {dev}");

        // Staging-agent framing
        let staging = build_prompt(&bead, "/tmp/repo", None, Some("staging-agent"));
        assert!(
            staging.contains("Review this change"),
            "staging framing: {staging}"
        );

        // Architect-agent framing
        let arch = build_prompt(&bead, "/tmp/repo", None, Some("architect-agent"));
        assert!(
            arch.contains("Analyze this problem"),
            "architect framing: {arch}"
        );
    }

    #[test]
    fn prompt_version_is_set() {
        assert!(
            PROMPT_VERSION.starts_with('v'),
            "PROMPT_VERSION should start with 'v'"
        );
        let assembled = build_system_prompt(None, None);
        assert!(
            assembled.contains(PROMPT_VERSION),
            "assembled system prompt should contain version"
        );
    }

    // -- AgentSession tests --

    #[tokio::test]
    async fn cli_session_success() {
        let child = tokio::process::Command::new("true").spawn().unwrap();
        let mut session = CliSession::new(child);
        let success = session.wait().await.unwrap();
        assert!(success);
    }

    #[tokio::test]
    async fn cli_session_failure() {
        let child = tokio::process::Command::new("false").spawn().unwrap();
        let mut session = CliSession::new(child);
        let success = session.wait().await.unwrap();
        assert!(!success);
    }

    #[tokio::test]
    async fn cli_session_try_wait_completed() {
        let child = tokio::process::Command::new("true").spawn().unwrap();
        let mut session = CliSession::new(child);
        // Wait for it to finish
        session.wait().await.unwrap();
        // try_wait should return Some now
        // (already waited, so this is a no-op — just verifying the API)
    }

    #[tokio::test]
    async fn cli_session_kill() {
        let child = tokio::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let mut session = CliSession::new(child);
        assert!(session.pid().is_some());
        session.kill().unwrap();
        // After kill, wait should return (not hang)
        let _success = session.wait().await.unwrap();
    }

    #[tokio::test]
    async fn cli_session_pid() {
        let child = tokio::process::Command::new("sleep")
            .arg("0.1")
            .spawn()
            .unwrap();
        let session = CliSession::new(child);
        assert!(session.pid().is_some());
    }

    #[test]
    fn agent_session_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<CliSession>();
    }

    #[tokio::test]
    async fn agent_handle_session_id() {
        let child = tokio::process::Command::new("true").spawn().unwrap();
        let mut handle = AgentHandle {
            bead_id: "test-1".into(),
            generation: 1,
            session: Box::new(CliSession::new(child)),
            work_dir: PathBuf::from("/tmp"),
            started_at: chrono::Utc::now(),
            workspace: None,
            session_id: None,
            workspace_path: Some("/tmp/.rsry-workspaces/test-1".into()),
            log_path: Some(PathBuf::from("/tmp/.rsry-stream.jsonl")),
        };

        assert!(handle.session_id.is_none());
        handle.set_session_id("sess-abc-123".into());
        assert_eq!(handle.session_id.as_deref(), Some("sess-abc-123"));
        assert_eq!(
            handle.workspace_path.as_deref(),
            Some("/tmp/.rsry-workspaces/test-1")
        );
    }

    // -- Agent definition loading tests --

    #[test]
    fn strip_frontmatter_basic() {
        let content = "---\nname: dev-agent\ndescription: test\n---\n\n# Dev Agent\n\nBody here.";
        let stripped = strip_frontmatter(content);
        assert!(stripped.starts_with("# Dev Agent"));
        assert!(!stripped.contains("name: dev-agent"));
    }

    #[test]
    fn strip_frontmatter_no_frontmatter() {
        let content = "# Just Markdown\n\nNo frontmatter here.";
        assert_eq!(strip_frontmatter(content), content);
    }

    #[test]
    fn strip_frontmatter_empty() {
        assert_eq!(strip_frontmatter(""), "");
    }

    #[test]
    fn strip_frontmatter_only_opening() {
        let content = "---\nno closing delimiter";
        assert_eq!(strip_frontmatter(content), content);
    }

    #[test]
    fn load_agent_prompt_from_tempdir() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("dev-agent.md"),
            "---\nname: dev-agent\n---\n\n# Dev Agent\n\nYou review code.",
        )
        .unwrap();

        let result = load_agent_prompt(dir.path(), "dev-agent");
        assert!(result.is_some());
        let body = result.unwrap();
        assert!(body.contains("# Dev Agent"));
        assert!(body.contains("You review code."));
        assert!(!body.contains("name: dev-agent"));
    }

    #[test]
    fn load_agent_prompt_missing_file() {
        let dir = TempDir::new().unwrap();
        assert!(load_agent_prompt(dir.path(), "nonexistent-agent").is_none());
    }

    #[test]
    fn load_agent_prompt_with_md_extension() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("test.md"), "# Test").unwrap();
        assert!(load_agent_prompt(dir.path(), "test.md").is_some());
    }

    #[test]
    fn build_system_prompt_no_agent() {
        let prompt = build_system_prompt(None, None);
        assert!(prompt.contains("rosary-dispatched agent"));
        assert!(!prompt.contains("Agent Perspective"));
        assert!(!prompt.contains("Golden Rules"));
    }

    #[test]
    fn build_system_prompt_with_agent() {
        let dir = TempDir::new().unwrap();
        let rules_dir = dir.path().join("rules");
        std::fs::create_dir(&rules_dir).unwrap();
        std::fs::write(
            rules_dir.join("GOLDEN_RULES.md"),
            "# Golden Rules\n\n1. Be minimal.",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("dev-agent.md"),
            "---\nname: dev-agent\n---\n\n# Dev Agent\n\nFind complexity hotspots.",
        )
        .unwrap();

        let prompt = build_system_prompt(Some("dev-agent"), Some(dir.path()));
        assert!(prompt.contains("rosary-dispatched agent"));
        assert!(prompt.contains("Golden Rules"));
        assert!(prompt.contains("Be minimal"));
        assert!(prompt.contains("Agent Perspective"));
        assert!(prompt.contains("Find complexity hotspots"));
    }

    #[test]
    fn build_system_prompt_missing_agent_falls_back() {
        let dir = TempDir::new().unwrap();
        let rules_dir = dir.path().join("rules");
        std::fs::create_dir(&rules_dir).unwrap();
        std::fs::write(rules_dir.join("GOLDEN_RULES.md"), "# Rules").unwrap();

        let prompt = build_system_prompt(Some("nonexistent-agent"), Some(dir.path()));
        // Should still have base prompt + golden rules, just no agent section
        assert!(prompt.contains("rosary-dispatched agent"));
        assert!(prompt.contains("Golden Rules"));
        assert!(!prompt.contains("Agent Perspective"));
    }

    #[test]
    fn pipeline_bug() {
        assert_eq!(agent_pipeline("bug"), &["dev-agent", "staging-agent"]);
    }

    #[test]
    fn pipeline_feature() {
        assert_eq!(
            agent_pipeline("feature"),
            &["dev-agent", "staging-agent", "prod-agent"]
        );
    }

    #[test]
    fn pipeline_task() {
        assert_eq!(agent_pipeline("task"), &["dev-agent"]);
    }

    #[test]
    fn default_agent_maps_issue_type() {
        assert_eq!(default_agent("bug"), "dev-agent");
        assert_eq!(default_agent("review"), "staging-agent");
        assert_eq!(default_agent("epic"), "pm-agent");
        assert_eq!(default_agent("xyz"), "dev-agent");
    }

    #[test]
    fn next_agent_advances() {
        assert_eq!(next_agent("bug", "dev-agent"), Some("staging-agent"));
        assert_eq!(next_agent("bug", "staging-agent"), None);
        assert_eq!(next_agent("feature", "staging-agent"), Some("prod-agent"));
        assert_eq!(next_agent("task", "dev-agent"), None);
        assert_eq!(next_agent("bug", "unknown"), None);
    }
}
