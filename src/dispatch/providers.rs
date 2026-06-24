//! Agent provider implementations — Claude, Gemini, ACP CLI.
//!
//! The `AgentProvider` trait abstracts over different AI backends.
//! Each provider translates `PermissionProfile` to its own CLI flags.

use anyhow::{Context, Result};
use std::path::Path;

use super::session::{AgentSession, CliSession};
use super::{PermissionProfile, STREAM_LOG_FILENAME};

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

    /// Build the CLI command that would be passed to the agent, without spawning.
    /// Returns (binary, args). Used by ComputeProvider to run in a container.
    #[allow(dead_code)] // API surface — used when compute != local
    fn build_command(
        &self,
        prompt: &str,
        permissions: &PermissionProfile,
        system_prompt: &str,
    ) -> (String, Vec<String>) {
        // Default: not supported — providers override if they can be containerized
        let _ = (prompt, permissions, system_prompt);
        (String::new(), Vec::new())
    }

    /// Human-readable name of this provider.
    fn name(&self) -> &str;

    /// Clone this provider with a model override for a single dispatch.
    ///
    /// Providers that support model selection (Claude) use the model;
    /// others return a copy of themselves unchanged.
    fn with_model(&self, model: Option<String>) -> Box<dyn AgentProvider>;
}

/// Provider that shells out to the Claude Code CLI (`claude -p`).
///
/// Uses `--allowedTools` with permission rule syntax to grant the agent
/// the tools it needs without interactive prompts.
pub struct ClaudeProvider {
    /// Absolute path to the claude binary. If empty, uses PATH lookup.
    pub binary: String,
    /// Optional model override (e.g. "claude-haiku-4-5-20251001").
    /// Passed as `--model` to the Claude CLI.
    pub model: Option<String>,
}

impl Default for ClaudeProvider {
    fn default() -> Self {
        Self {
            binary: "claude".to_string(),
            model: None,
        }
    }
}

impl AgentProvider for ClaudeProvider {
    fn spawn_agent(
        &self,
        prompt: &str,
        work_dir: &Path,
        permissions: &PermissionProfile,
        system_prompt: &str,
    ) -> Result<Box<dyn AgentSession>> {
        let log_path = work_dir.join(STREAM_LOG_FILENAME);
        let err_path = work_dir.join(".rsry-stderr.log");
        let err_file = std::fs::File::create(&err_path)
            .with_context(|| format!("creating stderr log {}", err_path.display()))?;

        // Exact pattern from PR #110 that was validated working with OAuth.
        // Key: -p mode, --output-format json, piped stdin, ENTRYPOINT removed.
        let allowed = permissions.claude_allowed_tools();
        let log_file = std::fs::File::create(&log_path)
            .with_context(|| format!("creating stream log {}", log_path.display()))?;
        eprintln!(
            "[spawn] {} -p <prompt> --allowedTools '{}' --output-format json (cwd={})",
            self.binary,
            allowed,
            work_dir.display()
        );

        // Resolve auth + endpoint env for launchd/rig contexts where Keychain
        // OAuth isn't reachable. Best-effort: on no-creds we still spawn (the
        // child may use Keychain in interactive/nested contexts) but warn
        // loudly — a credential-less daemon context is the rosary-b1495c
        // "Not logged in" failure.
        let launch_env = resolve_launch_env(work_dir);

        // MCP servers for the agent so its granted mcp__rsry__*/mcp__mache__*
        // tools connect during the run (rosary-563b3f). String must outlive
        // base_args (which borrows &str).
        let mcp_cfg = write_agent_mcp_config(work_dir);
        let mcp_cfg_str = mcp_cfg.as_ref().map(|p| p.to_string_lossy().into_owned());

        let mut cmd = tokio::process::Command::new(&self.binary);
        let mut base_args = vec![
            "-p",
            prompt,
            "--allowedTools",
            allowed,
            "--append-system-prompt",
            system_prompt,
            "--output-format",
            "json",
        ];
        if let Some(ref m) = self.model {
            base_args.extend_from_slice(&["--model", m.as_str()]);
        }
        if let Some(ref s) = mcp_cfg_str {
            base_args.extend_from_slice(&[
                "--mcp-config",
                s,
                "--strict-mcp-config",
                // Override a per-project `disabledMcpServers` that would otherwise
                // suppress the injected servers by name (rosary-563b3f).
                "--settings",
                AGENT_SETTINGS_OVERRIDE,
            ]);
        }
        cmd.args(base_args)
            .current_dir(work_dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("CLAUDECODE")
            .env_remove("CLAUDE_CODE_ENTRYPOINT")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::from(log_file))
            .stderr(std::process::Stdio::from(err_file));

        match &launch_env {
            Ok(env) => {
                for (k, v) in &env.vars {
                    cmd.env(k, v);
                }
                let keys: Vec<&str> = env.vars.iter().map(|(k, _)| k.as_str()).collect();
                eprintln!("[spawn] injected agent auth/env: {}", keys.join(", "));
            }
            Err(AuthError::NoCredentials) => {
                eprintln!(
                    "[spawn] WARNING: no claude credentials in env/.envrc/config — relying on \
                     ambient Keychain auth. If this is a rig/launchd daemon the agent will die \
                     'Not logged in': run `claude setup-token` and export CLAUDE_CODE_OAUTH_TOKEN \
                     (rosary-b1495c)."
                );
            }
        }

        let child = cmd
            .spawn()
            .with_context(|| format!("spawning claude CLI in {}", work_dir.display()))?;

        let pid = child.id().unwrap_or(0);
        eprintln!("[spawn] claude started (pid={pid})");

        Ok(Box::new(CliSession::new(child)))
    }

    fn build_command(
        &self,
        prompt: &str,
        permissions: &PermissionProfile,
        system_prompt: &str,
    ) -> (String, Vec<String>) {
        let mut args = vec![
            "-p".to_string(),
            prompt.to_string(),
            "--allowedTools".to_string(),
            permissions.claude_allowed_tools().to_string(),
            "--append-system-prompt".to_string(),
            system_prompt.to_string(),
            "--output-format".to_string(),
            "json".to_string(),
        ];
        if let Some(ref m) = self.model {
            args.push("--model".to_string());
            args.push(m.clone());
        }
        (self.binary.clone(), args)
    }

    fn name(&self) -> &str {
        "claude"
    }

    fn with_model(&self, model: Option<String>) -> Box<dyn AgentProvider> {
        Box::new(ClaudeProvider {
            binary: self.binary.clone(),
            model,
        })
    }
}

/// Provider that shells out to the Gemini CLI (`gemini -p`).
///
/// Uses `--approval-mode` to control permission prompts.
#[derive(Default)]
pub struct GeminiProvider {
    /// Path to the gemini binary. Defaults to "gemini".
    #[allow(dead_code)]
    pub binary: String,
    /// Extra CLI args beyond permissions.
    pub extra_args: Vec<String>,
}

impl GeminiProvider {
    /// Resolve the binary path, defaulting to `gemini` from PATH.
    fn resolved_binary(&self) -> &str {
        if self.binary.is_empty() {
            "gemini"
        } else {
            self.binary.as_str()
        }
    }
}

impl AgentProvider for GeminiProvider {
    fn build_command(
        &self,
        prompt: &str,
        permissions: &PermissionProfile,
        system_prompt: &str,
    ) -> (String, Vec<String>) {
        // Mirror spawn_agent's invocation so detached spawn (MCP path)
        // produces the same wire as the orchestrator path. Gemini doesn't
        // support --append-system-prompt; prepend to user prompt instead.
        let full_prompt = format!("{system_prompt}\n\n---\n\n{prompt}");
        let mut args = vec![
            "-p".to_string(),
            full_prompt,
            "-o".to_string(),
            "json".to_string(),
            "--approval-mode".to_string(),
            permissions.gemini_approval_mode().to_string(),
        ];
        args.extend(self.extra_args.iter().cloned());
        (self.resolved_binary().to_string(), args)
    }

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
        let err_path = work_dir.join(".rsry-stderr.log");
        let err_file = std::fs::File::create(&err_path)
            .with_context(|| format!("creating stderr log {}", err_path.display()))?;
        let bin = if self.binary.is_empty() {
            "gemini"
        } else {
            &self.binary
        };
        let mut cmd = tokio::process::Command::new(bin);
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
            .env_remove("CLAUDECODE")
            .env_remove("CLAUDE_CODE_ENTRYPOINT")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(log_file))
            .stderr(std::process::Stdio::from(err_file))
            .spawn()
            .with_context(|| format!("spawning gemini CLI in {}", work_dir.display()))?;
        Ok(Box::new(CliSession::new(child)))
    }

    fn name(&self) -> &str {
        "gemini"
    }

    fn with_model(&self, _model: Option<String>) -> Box<dyn AgentProvider> {
        Box::new(GeminiProvider {
            binary: self.binary.clone(),
            extra_args: self.extra_args.clone(),
        })
    }
}

/// Provider that spawns an ACP-compatible agent binary.
///
/// The binary must implement the Agent Client Protocol (JSON-RPC over stdio).
/// Permission handling happens via `RosaryClient::request_permission()` in
/// the ACP session, not via CLI flags.
///
/// Example binaries: `claude-agent-acp` (npm), custom ACP agents.
#[allow(dead_code)] // Legacy stub — replaced by AcpNativeProvider
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
        let err_path = work_dir.join(".rsry-stderr.log");
        let err_file = std::fs::File::create(&err_path)
            .with_context(|| format!("creating stderr log {}", err_path.display()))?;
        let child = tokio::process::Command::new(&self.binary)
            .current_dir(work_dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("CLAUDECODE")
            .env_remove("CLAUDE_CODE_ENTRYPOINT")
            // null stdin — piped stdin triggers CC's SDK detection which
            // uses different auth handling and fails with "Not logged in".
            // -p mode with null stdin uses standard OAuth from Keychain.
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(log_file))
            .stderr(std::process::Stdio::from(err_file))
            .spawn()
            .with_context(|| format!("spawning ACP agent: {}", self.binary))?;
        Ok(Box::new(CliSession::new(child)))
    }

    fn name(&self) -> &str {
        "acp"
    }

    fn with_model(&self, _model: Option<String>) -> Box<dyn AgentProvider> {
        Box::new(AcpCliProvider {
            binary: self.binary.clone(),
        })
    }
}

/// Provider that uses the ACP protocol natively via `ClientSideConnection`.
///
/// Spawns the agent binary as a subprocess, establishes a JSON-RPC connection,
/// and runs the full ACP lifecycle (initialize → new_session → prompt).
/// Works with any ACP-compatible binary: claude-agent-acp, gemini-agent-acp, etc.
pub struct AcpNativeProvider {
    /// Path or name of the ACP agent binary.
    pub binary: String,
}

impl AgentProvider for AcpNativeProvider {
    fn spawn_agent(
        &self,
        prompt: &str,
        work_dir: &Path,
        permissions: &PermissionProfile,
        system_prompt: &str,
    ) -> Result<Box<dyn AgentSession>> {
        let log_path = work_dir.join(STREAM_LOG_FILENAME);
        eprintln!(
            "[spawn] ACP native: {} (cwd={})",
            self.binary,
            work_dir.display()
        );
        // Inject auth + endpoint env via the shared resolver (single
        // credential by priority + ANTHROPIC_BASE_URL passthrough), same as
        // the claude paths; warn loudly on no-creds (rosary-b1495c / 5582ae).
        let launch_vars: Vec<(String, String)> = match resolve_launch_env(work_dir) {
            Ok(env) => {
                let keys: Vec<&str> = env.vars.iter().map(|(k, _)| k.as_str()).collect();
                eprintln!("[spawn] injected ACP agent auth/env: {}", keys.join(", "));
                env.vars
            }
            Err(AuthError::NoCredentials) => {
                eprintln!(
                    "[spawn] WARNING: no claude credentials in env/.envrc/config for ACP agent — \
                     relying on ambient Keychain. For headless, run `claude setup-token` and \
                     export CLAUDE_CODE_OAUTH_TOKEN (rosary-b1495c)."
                );
                Vec::new()
            }
        };
        let session = crate::acp::spawn_acp_session(
            &self.binary,
            prompt,
            work_dir,
            *permissions,
            system_prompt,
            &log_path,
            &launch_vars,
        )?;
        Ok(Box::new(session))
    }

    fn name(&self) -> &str {
        "acp"
    }

    fn with_model(&self, _model: Option<String>) -> Box<dyn AgentProvider> {
        Box::new(AcpNativeProvider {
            binary: self.binary.clone(),
        })
    }
}

/// Auth + endpoint environment to inject into a spawned agent CLI so it
/// authenticates identically regardless of how the parent was launched —
/// interactive CLI, a rig/OTP daemon with a stripped env, or nested inside
/// another claude-code session. The dispatch "Not logged in" failures
/// (rosary-b1495c) are credential-propagation bugs, not TTY bugs: a spawned
/// `claude` in a credential-less env fails ~84ms in. This is the single
/// source of truth every spawn site (`spawn_agent`, `bdr_enrich`, `verify`)
/// must apply to the child env.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct AgentLaunchEnv {
    /// (key, value) env pairs to set on the child process.
    pub vars: Vec<(String, String)>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AuthError {
    /// No usable credential found in env, `.envrc`, or global config —
    /// i.e. nothing to inject. Callers should still *proceed* (interactive
    /// and nested-claude-code contexts can authenticate via macOS Keychain)
    /// but warn loudly, since a credential-less daemon context is the
    /// rosary-b1495c "Not logged in" failure. Not a hard error.
    NoCredentials,
}

/// Pure resolver: given an env lookup and the contents of candidate `.envrc`
/// files (highest priority first), produce the env to inject — or fail loud.
/// Kept side-effect-free (no `std::env`, no fs) so it is unit-testable.
pub(crate) fn resolve_launch_env_from(
    getenv: impl Fn(&str) -> Option<String>,
    envrc_contents: &[String],
) -> Result<AgentLaunchEnv, AuthError> {
    // env takes priority; .envrc fills only what env lacks (direnv pattern).
    let lookup = |key: &str| -> Option<String> {
        getenv(key)
            .filter(|v| !v.is_empty())
            .or_else(|| envrc_value(envrc_contents, key))
    };

    // Exactly ONE credential is selected and injected — injecting all present
    // creds causes ambiguous auth selection by the claude CLI and leaks more
    // secrets to the child than needed (PR #226 review). When a non-default
    // endpoint is set (gateway / model-swap), the first-party OAuth token
    // doesn't apply, so gateway creds take priority over it.
    let gateway = lookup("ANTHROPIC_BASE_URL");
    let cred_priority: &[&str] = if gateway.is_some() {
        &[
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "CLAUDE_CODE_OAUTH_TOKEN",
        ]
    } else {
        &[
            "CLAUDE_CODE_OAUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
        ]
    };

    let Some((cred_key, cred_val)) = cred_priority
        .iter()
        .find_map(|k| lookup(k).map(|v| (k.to_string(), v)))
    else {
        return Err(AuthError::NoCredentials);
    };

    // ANTHROPIC_BASE_URL is the model-swap hook (Qwen/Kimi/local via an
    // Anthropic-compatible gateway): forwarded when present, not a credential.
    let mut vars = vec![(cred_key, cred_val)];
    if let Some(url) = gateway {
        vars.push(("ANTHROPIC_BASE_URL".to_string(), url));
    }
    Ok(AgentLaunchEnv { vars })
}

/// Parse `export KEY=value` from `.envrc` contents (highest priority first),
/// stripping surrounding quotes. Returns the first non-empty match.
fn envrc_value(envrc_contents: &[String], key: &str) -> Option<String> {
    let prefix = format!("export {key}=");
    for content in envrc_contents {
        for line in content.lines() {
            if let Some(val) = line.trim().strip_prefix(&prefix) {
                let val = val.trim().trim_matches('"').trim_matches('\'');
                if !val.is_empty() {
                    return Some(val.to_string());
                }
            }
        }
    }
    None
}

/// Impure entry point spawn sites use: gather the live process env + repo
/// `.envrc` files + global config, then resolve via [`resolve_launch_env_from`].
/// `Err(NoCredentials)` means "nothing to inject" — callers should still
/// proceed (the child may authenticate via macOS Keychain in interactive /
/// nested-claude-code contexts) but emit a loud warning, since a credential-
/// less *daemon* context (rig/launchd) is exactly the rosary-b1495c failure.
pub(crate) fn resolve_launch_env(work_dir: &Path) -> Result<AgentLaunchEnv, AuthError> {
    let mut envrc = collect_envrc(work_dir);
    // Global config (dispatch.anthropic_api_key) is the lowest-priority
    // credential source — after env and real .envrc. Append it as a synthetic
    // .envrc line so resolution runs through the SAME resolver, which means
    // endpoint passthrough (ANTHROPIC_BASE_URL from env/.envrc) still applies
    // when the credential comes from config (PR #226 review).
    if let Ok(cfg) = crate::config::load_global()
        && let Some(key) = cfg.dispatch.and_then(|d| d.anthropic_api_key)
        && !key.is_empty()
    {
        envrc.push(format!("export ANTHROPIC_API_KEY={key}"));
    }
    resolve_launch_env_from(|k| std::env::var(k).ok(), &envrc)
}

/// Read `.envrc` from `work_dir` and the git repo root (for worktrees).
fn collect_envrc(work_dir: &Path) -> Vec<String> {
    let mut paths = vec![work_dir.join(".envrc")];
    if let Ok(output) = std::process::Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(work_dir)
        .output()
    {
        let git_common = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if let Some(repo_root) = std::path::Path::new(&git_common).parent() {
            paths.push(repo_root.join(".envrc"));
        }
    }
    paths
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect()
}

/// MCP servers to expose to dispatched agents (rosary-563b3f), from
/// `[dispatch] agent_mcp` (name → HTTP URL); defaults to local rsry + mache.
fn agent_mcp_servers() -> std::collections::BTreeMap<String, String> {
    crate::config::load_global()
        .ok()
        .and_then(|c| c.dispatch.map(|d| d.agent_mcp))
        .unwrap_or_else(crate::config::default_agent_mcp)
}

/// Claude settings overlay passed via `--settings` at every dispatch spawn that
/// injects MCP config (rosary-563b3f). Without it, a per-project
/// `disabledMcpServers` entry in the user's `~/.claude.json` (e.g. the human
/// disabling `rsry`/`lectio` in interactive rosary sessions to avoid
/// self-recursion) silently wins over `--mcp-config --strict-mcp-config` — the
/// disable matches by server *name* — leaving the dispatched agent with zero
/// `mcp__rsry__*` tools. A dispatched agent runs in a worktree whose git root is
/// the host repo, so it inherits that repo's project settings. Verified
/// empirically: from a rosary worktree the `rsry` server only connects with this
/// flag; `--strict-mcp-config`, `disabledMcpServers:[]`, and
/// `enabledMcpjsonServers:[rsry]` all failed to override the disable, while
/// `enableAllProjectMcpServers` succeeds.
pub(crate) const AGENT_SETTINGS_OVERRIDE: &str = r#"{"enableAllProjectMcpServers":true}"#;

/// Build the claude `--mcp-config` JSON declaring the given HTTP MCP servers.
fn agent_mcp_config_json(servers: &std::collections::BTreeMap<String, String>) -> String {
    let entries: Vec<String> = servers
        .iter()
        .map(|(name, url)| format!(r#""{name}":{{"type":"http","url":"{url}"}}"#))
        .collect();
    format!(r#"{{"mcpServers":{{{}}}}}"#, entries.join(","))
}

/// Write the dispatched-agent MCP config into `work_dir`, returning its path
/// (or `None` if no servers are configured / write failed). Spawn sites pass
/// `--mcp-config <path> --strict-mcp-config` so the agent's granted rsry/mache
/// tools actually connect during the run (rosary-563b3f).
pub(crate) fn write_agent_mcp_config(work_dir: &Path) -> Option<std::path::PathBuf> {
    let servers = agent_mcp_servers();
    if servers.is_empty() {
        return None;
    }
    let path = work_dir.join(".rsry-mcp.json");
    match std::fs::write(&path, agent_mcp_config_json(&servers)) {
        Ok(()) => Some(path),
        Err(e) => {
            eprintln!(
                "[spawn] could not write agent MCP config {}: {e}",
                path.display()
            );
            None
        }
    }
}

/// Resolve a provider by name string, with optional binary path overrides from config.
pub fn provider_by_name(
    name: &str,
    binaries: &std::collections::HashMap<String, String>,
) -> Result<Box<dyn AgentProvider>> {
    match name {
        "claude" => {
            let binary = binaries
                .get("claude")
                .cloned()
                .unwrap_or_else(|| "claude".to_string());
            Ok(Box::new(ClaudeProvider {
                binary,
                model: None,
            }))
        }
        "gemini" => {
            let binary = binaries
                .get("gemini")
                .cloned()
                .unwrap_or_else(|| "gemini".to_string());
            Ok(Box::new(GeminiProvider {
                binary,
                ..Default::default()
            }))
        }
        "acp" => {
            let binary = binaries
                .get("acp")
                .cloned()
                .unwrap_or_else(|| "claude-agent-acp".to_string());
            Ok(Box::new(AcpNativeProvider { binary }))
        }
        other => anyhow::bail!("unknown provider: {other} (available: claude, gemini, acp)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// rosary-563b3f: the agent MCP config is valid JSON with HTTP servers.
    #[test]
    fn agent_mcp_config_json_is_valid_http_shaped() {
        let servers = std::collections::BTreeMap::from([
            ("rsry".to_string(), "http://localhost:8383/mcp".to_string()),
            ("mache".to_string(), "http://localhost:7532/mcp".to_string()),
        ]);
        let v: serde_json::Value =
            serde_json::from_str(&agent_mcp_config_json(&servers)).expect("valid json");
        assert_eq!(v["mcpServers"]["rsry"]["type"], "http");
        assert_eq!(v["mcpServers"]["rsry"]["url"], "http://localhost:8383/mcp");
        assert_eq!(v["mcpServers"]["mache"]["url"], "http://localhost:7532/mcp");
    }

    /// rosary-563b3f: the settings overlay re-enables project MCP servers that a
    /// per-project `disabledMcpServers` would otherwise suppress by name.
    #[test]
    fn agent_settings_override_enables_project_mcp_servers() {
        let v: serde_json::Value =
            serde_json::from_str(AGENT_SETTINGS_OVERRIDE).expect("valid json");
        assert_eq!(v["enableAllProjectMcpServers"], true);
    }

    #[test]
    fn default_agent_mcp_has_rsry_and_mache() {
        let d = crate::config::default_agent_mcp();
        assert!(d.contains_key("rsry"), "default must include rsry");
        assert!(d.contains_key("mache"), "default must include mache");
    }

    // --- Agent launch-env resolution (rosary-b1495c) ---

    #[test]
    fn launch_env_signals_no_credentials_when_absent() {
        // rig/OTP daemon context: stripped env, no .envrc creds. Old code
        // returned a silent None; the resolver now returns a loud Err so the
        // spawn site can warn about the impending "Not logged in" instead of
        // silently producing a dead agent.
        let empty_env = |_: &str| None;
        let result = resolve_launch_env_from(empty_env, &[]);
        assert_eq!(result, Err(AuthError::NoCredentials));
    }

    #[test]
    fn launch_env_forwards_oauth_token_from_env() {
        let env = |k: &str| (k == "CLAUDE_CODE_OAUTH_TOKEN").then(|| "tok-123".to_string());
        let resolved = resolve_launch_env_from(env, &[]).expect("token present");
        assert!(
            resolved
                .vars
                .contains(&("CLAUDE_CODE_OAUTH_TOKEN".to_string(), "tok-123".to_string())),
            "child env must carry the OAuth token, got {:?}",
            resolved.vars
        );
    }

    #[test]
    fn launch_env_forwards_model_gateway_endpoint() {
        // Qwen/Kimi/local via Anthropic-compatible gateway: BASE_URL is not a
        // credential by itself, but must be forwarded alongside AUTH_TOKEN so
        // dispatched agents target the non-Anthropic endpoint.
        let env = |k: &str| match k {
            "ANTHROPIC_BASE_URL" => Some("https://api.moonshot.ai/anthropic".to_string()),
            "ANTHROPIC_AUTH_TOKEN" => Some("sk-moon".to_string()),
            _ => None,
        };
        let resolved = resolve_launch_env_from(env, &[]).expect("auth token present");
        assert!(
            resolved.vars.contains(&(
                "ANTHROPIC_BASE_URL".to_string(),
                "https://api.moonshot.ai/anthropic".to_string()
            )),
            "endpoint must be forwarded for model-swap, got {:?}",
            resolved.vars
        );
        assert!(
            resolved
                .vars
                .contains(&("ANTHROPIC_AUTH_TOKEN".to_string(), "sk-moon".to_string())),
            "gateway auth token must be forwarded, got {:?}",
            resolved.vars
        );
    }

    #[test]
    fn launch_env_falls_back_to_envrc_when_env_empty() {
        // direnv pattern: rig/daemon has no creds in its stripped env, but the
        // repo's .envrc carries them. Mirrors resolve_auth_token's fallback.
        let empty_env = |_: &str| None;
        let envrc = vec!["export CLAUDE_CODE_OAUTH_TOKEN='envrc-tok'\n# comment\n".to_string()];
        let resolved = resolve_launch_env_from(empty_env, &envrc).expect("envrc token");
        assert!(
            resolved.vars.contains(&(
                "CLAUDE_CODE_OAUTH_TOKEN".to_string(),
                "envrc-tok".to_string()
            )),
            "must resolve token from .envrc, got {:?}",
            resolved.vars
        );
    }

    #[test]
    fn launch_env_keeps_endpoint_passthrough_when_cred_is_lower_priority() {
        // Regression for PR #226 review: credential only in the lower-priority
        // source (.envrc — how global-config flows in now), endpoint in env.
        // BOTH must survive; the old config-fallback branch dropped BASE_URL.
        let env = |k: &str| {
            (k == "ANTHROPIC_BASE_URL").then(|| "https://gw.example/anthropic".to_string())
        };
        let envrc = vec!["export ANTHROPIC_API_KEY='k-from-envrc'\n".to_string()];
        let resolved = resolve_launch_env_from(env, &envrc).expect("cred present via envrc");
        assert!(
            resolved
                .vars
                .contains(&("ANTHROPIC_API_KEY".to_string(), "k-from-envrc".to_string())),
            "credential from lower-priority source must resolve, got {:?}",
            resolved.vars
        );
        assert!(
            resolved.vars.contains(&(
                "ANTHROPIC_BASE_URL".to_string(),
                "https://gw.example/anthropic".to_string()
            )),
            "endpoint passthrough must survive when cred is lower-priority, got {:?}",
            resolved.vars
        );
    }

    #[test]
    fn launch_env_selects_single_credential_by_priority() {
        // Multiple creds present, default endpoint: inject exactly ONE
        // (highest priority = OAuth), not all — avoids ambiguous CLI auth
        // selection and over-propagation of secrets (PR #226 review).
        let env = |k: &str| match k {
            "CLAUDE_CODE_OAUTH_TOKEN" => Some("oauth-tok".to_string()),
            "ANTHROPIC_API_KEY" => Some("api-key".to_string()),
            "ANTHROPIC_AUTH_TOKEN" => Some("auth-tok".to_string()),
            _ => None,
        };
        let resolved = resolve_launch_env_from(env, &[]).expect("creds present");
        let cred_keys: Vec<&str> = resolved
            .vars
            .iter()
            .map(|(k, _)| k.as_str())
            .filter(|k| *k != "ANTHROPIC_BASE_URL")
            .collect();
        assert_eq!(
            cred_keys,
            vec!["CLAUDE_CODE_OAUTH_TOKEN"],
            "exactly one credential (highest priority) must be injected, got {:?}",
            resolved.vars
        );
    }

    #[test]
    fn launch_env_gateway_endpoint_outranks_oauth_token() {
        // Transition case: Max OAuth token AND a Kimi/Qwen gateway both set.
        // A first-party OAuth token can't auth against a third-party gateway,
        // so the gateway credential must win — and OAuth must not leak.
        let env = |k: &str| match k {
            "ANTHROPIC_BASE_URL" => Some("https://api.moonshot.ai/anthropic".to_string()),
            "ANTHROPIC_AUTH_TOKEN" => Some("sk-moon".to_string()),
            "CLAUDE_CODE_OAUTH_TOKEN" => Some("oauth-tok".to_string()),
            _ => None,
        };
        let resolved = resolve_launch_env_from(env, &[]).expect("creds present");
        let cred_keys: Vec<&str> = resolved
            .vars
            .iter()
            .map(|(k, _)| k.as_str())
            .filter(|k| *k != "ANTHROPIC_BASE_URL")
            .collect();
        assert_eq!(
            cred_keys,
            vec!["ANTHROPIC_AUTH_TOKEN"],
            "gateway cred must outrank OAuth when a non-default endpoint is set, got {:?}",
            resolved.vars
        );
    }

    /// End-to-end smoke test of the REAL spawn path (env injection + args +
    /// stdin handling) against live auth. Ignored by default: spends tokens
    /// and requires a logged-in `claude` (Keychain in interactive/nested
    /// contexts, or CLAUDE_CODE_OAUTH_TOKEN from `claude setup-token`). Run:
    ///   cargo test -- --ignored claude_spawn_agent_end_to_end
    #[tokio::test]
    #[ignore]
    async fn claude_spawn_agent_end_to_end_authenticates_and_completes() {
        let work = tempfile::tempdir().expect("tempdir");
        // git init so collect_envrc's `git rev-parse` resolves quietly.
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(work.path())
            .status()
            .ok();
        let provider = ClaudeProvider::default();
        let perms = PermissionProfile::default();
        let mut session = provider
            .spawn_agent(
                "Reply with exactly the token DISPATCH_OK and nothing else. Do not use any tools.",
                work.path(),
                &perms,
                "You are a terse test agent.",
            )
            .expect("spawn_agent should succeed");
        let ok = session.wait().await.expect("wait should not error");
        let log = std::fs::read_to_string(work.path().join(STREAM_LOG_FILENAME))
            .unwrap_or_else(|e| format!("<no stream log: {e}>"));
        assert!(
            !log.contains("Not logged in"),
            "auth must succeed via Max account, got: {log}"
        );
        assert!(ok, "agent process should exit 0; stream log: {log}");
        assert!(
            log.contains("\"is_error\":false"),
            "expected a successful result line, got: {log}"
        );
    }

    #[test]
    fn claude_build_command_produces_dash_p_invocation() {
        let provider = ClaudeProvider::default();
        let perms = PermissionProfile::default();
        let (bin, args) = provider.build_command("the prompt", &perms, "the sys prompt");
        assert_eq!(bin, "claude");
        // -p flag + prompt
        assert!(args.iter().any(|a| a == "-p"));
        assert!(args.iter().any(|a| a == "the prompt"));
        // System prompt is appended via --append-system-prompt (claude-specific)
        assert!(args.iter().any(|a| a == "--append-system-prompt"));
        assert!(args.iter().any(|a| a == "the sys prompt"));
        // Allowed tools wired through
        assert!(args.iter().any(|a| a == "--allowedTools"));
        // Output format pins JSON for stream parsing
        assert!(args.iter().any(|a| a == "--output-format"));
        assert!(args.iter().any(|a| a == "json"));
    }

    /// Regression for Copilot finding on PR #200: `GeminiProvider` used the
    /// trait-default `build_command` (empty), so `rsry_dispatch
    /// provider=gemini` errored at spawn time even though the tool schema
    /// advertised gemini as a valid choice. Gemini now implements
    /// `build_command` mirroring its `spawn_agent` invocation.
    #[test]
    fn gemini_build_command_is_non_empty_and_mirrors_spawn_agent() {
        let provider = GeminiProvider {
            binary: String::new(), // default → "gemini" from PATH
            extra_args: vec!["--foo".to_string()],
        };
        let perms = PermissionProfile::default();
        let (bin, args) = provider.build_command("the prompt", &perms, "the sys prompt");

        assert_eq!(bin, "gemini", "default binary should resolve to gemini");
        assert!(
            !args.is_empty(),
            "gemini build_command must not fall through to trait-default empty (rosary-748f07)"
        );
        // Gemini has no --append-system-prompt; system prompt is prepended.
        let p_index = args
            .iter()
            .position(|a| a == "-p")
            .expect("gemini build_command must include -p flag");
        let full_prompt = &args[p_index + 1];
        assert!(
            full_prompt.contains("the sys prompt"),
            "system prompt must be prepended into -p arg, got: {full_prompt}"
        );
        assert!(
            full_prompt.contains("the prompt"),
            "user prompt must be in -p arg, got: {full_prompt}"
        );
        // Extra args appended verbatim
        assert!(args.iter().any(|a| a == "--foo"));
        // Approval mode wired through
        assert!(args.iter().any(|a| a == "--approval-mode"));
    }

    #[test]
    fn gemini_resolved_binary_falls_back_to_path_lookup() {
        let p = GeminiProvider {
            binary: String::new(),
            extra_args: vec![],
        };
        assert_eq!(p.resolved_binary(), "gemini");
        let p = GeminiProvider {
            binary: "/opt/gemini-cli".to_string(),
            extra_args: vec![],
        };
        assert_eq!(p.resolved_binary(), "/opt/gemini-cli");
    }
}
