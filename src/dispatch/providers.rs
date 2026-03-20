//! AI agent provider implementations (Claude, Gemini, ACP).

use anyhow::{Context, Result};
use std::path::Path;

use super::PermissionProfile;
use super::STREAM_LOG_FILENAME;
use super::session::CliSession;

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
    ) -> Result<Box<dyn super::session::AgentSession>>;

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
    ) -> Result<Box<dyn super::session::AgentSession>> {
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
    ) -> Result<Box<dyn super::session::AgentSession>> {
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
    ) -> Result<Box<dyn super::session::AgentSession>> {
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
