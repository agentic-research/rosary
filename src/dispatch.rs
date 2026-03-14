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

/// Trait for AI agent providers. Implementations handle spawning and
/// communicating with different AI backends (Claude, Gemini, Codex, etc).
pub trait AgentProvider: Send + Sync {
    /// Spawn an agent process for the given prompt in the given directory.
    /// Returns a handle to the running process.
    fn spawn_agent(
        &self,
        prompt: &str,
        work_dir: &Path,
    ) -> Result<tokio::process::Child>;

    /// Human-readable name of this provider.
    fn name(&self) -> &str;
}

/// Provider that shells out to the Claude Code CLI (`claude --print`).
pub struct ClaudeProvider;

impl AgentProvider for ClaudeProvider {
    fn spawn_agent(
        &self,
        prompt: &str,
        work_dir: &Path,
    ) -> Result<tokio::process::Child> {
        tokio::process::Command::new("claude")
            .args(["--print", prompt])
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
/// Gemini supports:
/// - Headless mode: `-p "prompt"` (non-interactive)
/// - JSON output: `-o json` for structured results
/// - MCP servers: `--allowed-mcp-server-names`
/// - Auto-approve: `--approval-mode yolo`
/// - ACP mode: `--experimental-acp` (Agent Client Protocol)
pub struct GeminiProvider {
    /// Extra CLI args (e.g. `["--approval-mode", "yolo"]`).
    pub extra_args: Vec<String>,
}

impl Default for GeminiProvider {
    fn default() -> Self {
        Self {
            extra_args: Vec::new(),
        }
    }
}

impl AgentProvider for GeminiProvider {
    fn spawn_agent(
        &self,
        prompt: &str,
        work_dir: &Path,
    ) -> Result<tokio::process::Child> {
        let mut cmd = tokio::process::Command::new("gemini");
        cmd.args(["-p", prompt, "-o", "json"]);
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

/// Resolve a provider by name string (from config/CLI).
pub fn provider_by_name(name: &str) -> Result<Box<dyn AgentProvider>> {
    match name {
        "claude" => Ok(Box::new(ClaudeProvider)),
        "gemini" => Ok(Box::new(GeminiProvider::default())),
        other => anyhow::bail!("unknown provider: {other} (available: claude, gemini)"),
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
        "Fix this issue. Make the minimal change needed, scoped to a single file.\n\
         \n\
         Title: {}\n\
         Description: {}\n\
         \n\
         After fixing:\n\
         1. Run tests to verify\n\
         2. Create a commit with a descriptive message\n\
         3. Report what you changed",
        bead.title, bead.description
    )
}

/// Create a git worktree for isolated work. Returns the worktree path on success.
async fn create_worktree(
    repo_path: &Path,
    bead_id: &str,
) -> Result<PathBuf, ()> {
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
        create_worktree(&path, &bead.id)
            .await
            .unwrap_or_else(|()| {
                eprintln!("warning: worktree creation failed, running in-place");
                path.clone()
            })
    } else {
        path.clone()
    };

    println!("Dispatching {} to {}...", bead.id, provider.name());

    let child = provider
        .spawn_agent(&prompt, &work_dir)
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
        assert!(prompt.contains("Run tests to verify"));
    }
}
