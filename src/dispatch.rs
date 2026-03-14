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
    pub fn claude_allowed_tools(&self) -> &str {
        match self {
            Self::ReadOnly => "Read,Glob,Grep,mcp__mache__*,mcp__rsry__*",
            Self::Implement => {
                "Read,Edit,Write,Bash(cargo *),Bash(go *),Bash(git diff *),Bash(git log *),Bash(git status *),Bash(git add *),Bash(git commit *),Bash(task *),Glob,Grep,mcp__mache__*,mcp__rsry__*"
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

/// Trait for AI agent providers. Implementations handle spawning and
/// communicating with different AI backends (Claude, Gemini, Codex, etc).
///
/// The `permissions` argument comes from the bead — the provider just
/// translates it to CLI flags. This keeps schema/config decisions out
/// of the provider code.
pub trait AgentProvider: Send + Sync {
    /// Spawn an agent process with the given prompt, working directory,
    /// and permission profile (derived from the bead).
    fn spawn_agent(
        &self,
        prompt: &str,
        work_dir: &Path,
        permissions: &PermissionProfile,
    ) -> Result<tokio::process::Child>;

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
    ) -> Result<tokio::process::Child> {
        tokio::process::Command::new("claude")
            .args([
                "-p",
                prompt,
                "--allowedTools",
                permissions.claude_allowed_tools(),
                "--append-system-prompt",
                AGENT_SYSTEM_PROMPT,
                "--output-format",
                "json",
            ])
            .current_dir(work_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("spawning claude CLI in {}", work_dir.display()))
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
    ) -> Result<tokio::process::Child> {
        let mut cmd = tokio::process::Command::new("gemini");
        cmd.args([
            "-p",
            prompt,
            "-o",
            "json",
            "--approval-mode",
            permissions.gemini_approval_mode(),
        ]);
        for arg in &self.extra_args {
            cmd.arg(arg);
        }
        cmd.current_dir(work_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("spawning gemini CLI in {}", work_dir.display()))
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
    ) -> Result<tokio::process::Child> {
        // ACP agents are spawned as subprocesses with stdio piped.
        // The prompt and permissions are sent via ACP protocol (JSON-RPC),
        // not CLI args. The caller must establish a ClientSideConnection
        // after spawning and use Agent::prompt() to send the task.
        tokio::process::Command::new(&self.binary)
            .current_dir(work_dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("spawning ACP agent: {}", self.binary))
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

/// Handle to a running Claude Code agent process.
pub struct AgentHandle {
    #[allow(dead_code)] // stored for debugging/logging
    pub bead_id: String,
    #[allow(dead_code)] // stored for generation tracking
    pub generation: u64,
    pub child: tokio::process::Child,
    pub work_dir: PathBuf,
    pub started_at: chrono::DateTime<chrono::Utc>,
}

impl AgentHandle {
    /// Check if the agent process has exited (non-blocking).
    pub fn try_wait(&mut self) -> Result<Option<std::process::ExitStatus>> {
        Ok(self.child.try_wait()?)
    }

    /// Wait for the agent to complete.
    pub async fn wait(&mut self) -> Result<std::process::ExitStatus> {
        Ok(self.child.wait().await?)
    }

    /// Kill the agent process.
    pub fn kill(&mut self) -> Result<()> {
        self.child.start_kill()?;
        Ok(())
    }

    /// Elapsed time since dispatch.
    pub fn elapsed(&self) -> chrono::Duration {
        chrono::Utc::now() - self.started_at
    }
}

/// Build the prompt for a bead.
pub fn build_prompt(bead: &Bead) -> String {
    format!(
        "Fix this issue. Make the minimal change needed.\n\
         \n\
         Title: {}\n\
         Description: {}\n\
         \n\
         After fixing:\n\
         1. Run tests via `task test` (not raw cargo/go test)\n\
         2. Create a commit with a descriptive message\n\
         3. Report what you changed",
        bead.title, bead.description
    )
}

/// System prompt appended to all dispatched agents.
/// Tells agents about available MCP tools and workflow expectations.
const AGENT_SYSTEM_PROMPT: &str = "\
You are a rosary-dispatched agent working on a bead (work item).\n\
\n\
## Available Tools\n\
- **mache MCP**: Use mcp__mache__* tools for structural code navigation \
  (find_definition, find_callers, find_callees, search, get_overview). \
  Prefer mache over grep for understanding code structure.\n\
- **rsry MCP**: Use mcp__rsry__* tools for bead management \
  (bead_create, bead_close, bead_comment, bead_search).\n\
\n\
## Workflow\n\
- Use `task build` / `task test` instead of raw `cargo` or `go` commands\n\
- Make minimal, focused changes\n\
- Commit with descriptive messages\n\
- Do NOT add co-author lines to commits\n\
";

/// Create a git worktree for isolated work. Returns the worktree path on success.
async fn create_worktree(repo_path: &Path, bead_id: &str) -> Result<PathBuf, ()> {
    let branch_name = format!("fix/{bead_id}");
    let worktree_path = repo_path.join(format!("../{branch_name}"));

    let output = tokio::process::Command::new("git")
        .args([
            "worktree",
            "add",
            "-b",
            &branch_name,
            &worktree_path.to_string_lossy(),
        ])
        .current_dir(repo_path)
        .output()
        .await;

    match output {
        Ok(o) if o.status.success() => {
            println!("Created worktree: {}", worktree_path.display());
            Ok(worktree_path)
        }
        _ => Err(()),
    }
}

/// Spawn an AI agent for a bead. Returns a handle without waiting.
///
/// This is the async entry point for the reconciliation loop.
/// The `provider` argument controls which AI backend is used.
pub async fn spawn(
    bead: &Bead,
    repo_path: &Path,
    isolate: bool,
    generation: u64,
    provider: &dyn AgentProvider,
) -> Result<AgentHandle> {
    let path = expand_path(repo_path);
    let prompt = build_prompt(bead);

    let work_dir = if isolate {
        create_worktree(&path, &bead.id).await.unwrap_or_else(|()| {
            eprintln!("warning: worktree creation failed, running in-place");
            path.clone()
        })
    } else {
        path.clone()
    };

    // Permissions come from the bead, not the provider.
    // TODO: read from bead.permissions field or schema config once available.
    // For now, derive from issue_type as a sensible default.
    let permissions = match bead.issue_type.as_str() {
        "review" | "survey" | "audit" => PermissionProfile::ReadOnly,
        "epic" | "plan" | "triage" => PermissionProfile::Plan,
        _ => PermissionProfile::Implement,
    };

    println!(
        "Dispatching {} to {} (perms={:?})...",
        bead.id,
        provider.name(),
        permissions
    );

    let child = provider
        .spawn_agent(&prompt, &work_dir, &permissions)
        .with_context(|| format!("spawning {} for {}", provider.name(), bead.id))?;

    Ok(AgentHandle {
        bead_id: bead.id.clone(),
        generation,
        child,
        work_dir,
        started_at: chrono::Utc::now(),
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

    let mut handle = spawn(&bead, &path, isolate, bead.generation(), &ClaudeProvider).await?;
    let status = handle.wait().await?;

    if status.success() {
        println!("Claude Code completed successfully for {bead_id}");
    } else {
        eprintln!("warning: claude exited with {status} for {bead_id}");
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
        };

        let prompt = build_prompt(&bead);
        assert!(prompt.contains("Fix the widget"));
        assert!(prompt.contains("The widget is broken"));
        assert!(prompt.contains("Run tests via"));
    }
}
