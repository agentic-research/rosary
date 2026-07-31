//! CloisterProvider — `ComputeProvider` impl that confines the dispatched
//! agent to its own workspace via `cloister run`, instead of provisioning a
//! separate remote environment (rosary-56b557).
//!
//! Every other `ComputeProvider` (Docker, Sprites) provisions a FRESH
//! environment that gets its own checkout of the repo — `provision()` is
//! genuinely creating a new resource. `cloister run --repo <path>` is the
//! opposite shape: it binds an EXISTING host directory (the git worktree
//! `rosary` already created for this bead) as the harness's sole writable
//! root, denying everything else (other repos, `~/.ssh`, outbound network
//! except loopback to cloister). There is nothing to provision — the
//! confinement is applied per-invocation by `cloister run` itself — so
//! `provision()` here is a local no-op that just carries the workspace path
//! forward to `exec()` via `ExecHandle.id` (opaque to callers by design; see
//! `ProvisionOpts::workspace_path`, which this provider requires).
//!
//! `exec()` maps the agent binary rosary's `AgentProvider::build_command`
//! already resolved (`cmd[0]`, e.g. `"claude"`) to the cloister-declared
//! harness NAME (`"claude-code"`) — cloister resolves its own binary for a
//! declared harness (`cluster.toml`'s `gateway.harnessTargets`), so `cmd[0]`
//! is discarded once it has picked the `--harness` value, and only the
//! REMAINING args (`cmd[1..]`) are forwarded after `--`, verbatim, as the
//! harness's own argv — exactly the shape `cloister run`'s own CLI expects
//! (`scripts/cli-run.mjs`: "Everything after `--` belongs to the harness").
//!
//! Only harnesses cloister has actually declared are supported —
//! `harness_name` refuses anything else rather than guessing a name and
//! producing a "could not resolve" failure deep inside cloister instead of a
//! clear one here.

use std::path::PathBuf;

use anyhow::{Context as _, Result};

use crate::backend::{ComputeProvider, ExecHandle, ExecResult, ProvisionOpts};

/// Compute provider that runs the dispatched agent as a host process
/// confined by `cloister run`, rather than inside a separately-provisioned
/// container/VM.
pub struct CloisterProvider {
    /// Absolute path to the `cloister` CLI.
    cloister_bin: PathBuf,
}

impl CloisterProvider {
    pub fn new(cloister_bin: PathBuf) -> Self {
        Self { cloister_bin }
    }

    /// Map rosary's `AgentProvider::name()` to cloister's declared harness
    /// target name (`cluster.toml`'s `gateway.harnessTargets`). Only
    /// `claude` (-> `claude-code`) and `codex` are declared there today —
    /// `gemini`/`acp` have no cloister harness target yet.
    fn harness_name(agent_provider_name: &str) -> Result<&'static str> {
        match agent_provider_name {
            "claude" => Ok("claude-code"),
            "codex" => Ok("codex"),
            other => anyhow::bail!(
                "cloister has no declared harness target for provider \"{other}\" — \
                 only \"claude\" (cloister's \"claude-code\") and \"codex\" are registered \
                 in cluster.toml today"
            ),
        }
    }
}

#[async_trait::async_trait]
impl ComputeProvider for CloisterProvider {
    async fn provision(&self, opts: &ProvisionOpts) -> Result<ExecHandle> {
        let workspace = opts.workspace_path.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "CloisterProvider requires ProvisionOpts::workspace_path — cloister run --repo \
                 confines an EXISTING directory, it does not provision one"
            )
        })?;
        Ok(ExecHandle {
            id: workspace.display().to_string(),
            backend: "cloister".to_string(),
        })
    }

    async fn exec(&self, handle: &ExecHandle, cmd: &[&str]) -> Result<ExecResult> {
        anyhow::ensure!(!cmd.is_empty(), "empty command");
        let harness = Self::harness_name(cmd[0])?;

        let mut args: Vec<&str> = vec!["run", "--harness", harness, "--repo", &handle.id, "--"];
        args.extend(&cmd[1..]);

        let output = tokio::process::Command::new(&self.cloister_bin)
            .args(&args)
            .output()
            .await
            .with_context(|| {
                format!(
                    "cloister run --harness {harness} --repo {}: {}",
                    handle.id,
                    cmd.join(" ")
                )
            })?;

        Ok(ExecResult {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }

    async fn destroy(&self, _handle: &ExecHandle) -> Result<()> {
        // No resource was provisioned — cloister run's confinement is
        // per-invocation and already torn down when the process exited.
        Ok(())
    }

    fn name(&self) -> &str {
        "cloister"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_name_maps_claude_to_claude_code() {
        assert_eq!(
            CloisterProvider::harness_name("claude").unwrap(),
            "claude-code"
        );
    }

    #[test]
    fn harness_name_maps_codex_verbatim() {
        assert_eq!(CloisterProvider::harness_name("codex").unwrap(), "codex");
    }

    #[test]
    fn harness_name_refuses_undeclared_providers() {
        // gemini/acp have no cloister harness target yet (cluster.toml) —
        // must refuse rather than guess a name cloister would reject anyway.
        assert!(CloisterProvider::harness_name("gemini").is_err());
        assert!(CloisterProvider::harness_name("acp").is_err());
        assert!(CloisterProvider::harness_name("").is_err());
    }

    #[tokio::test]
    async fn provision_requires_workspace_path() {
        let provider = CloisterProvider::new(PathBuf::from("/usr/local/bin/cloister"));
        let opts = ProvisionOpts::new("bead-1", "rosary"); // no .workspace_path()
        let err = provider.provision(&opts).await.unwrap_err();
        assert!(format!("{err:#}").contains("workspace_path"), "{err:#}");
    }

    #[tokio::test]
    async fn provision_carries_workspace_path_into_the_handle_id() {
        let provider = CloisterProvider::new(PathBuf::from("/usr/local/bin/cloister"));
        let opts = ProvisionOpts::new("bead-1", "rosary")
            .workspace_path(PathBuf::from("/tmp/rosary-bead-1-workspace"));
        let handle = provider.provision(&opts).await.unwrap();
        assert_eq!(handle.id, "/tmp/rosary-bead-1-workspace");
        assert_eq!(handle.backend, "cloister");
    }

    #[tokio::test]
    async fn exec_empty_command_errors() {
        let provider = CloisterProvider::new(PathBuf::from("/usr/local/bin/cloister"));
        let handle = ExecHandle {
            id: "/tmp/workspace".into(),
            backend: "cloister".into(),
        };
        let result = provider.exec(&handle, &[]).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn exec_rejects_a_provider_cloister_has_no_harness_for() {
        let provider = CloisterProvider::new(PathBuf::from("/usr/local/bin/cloister"));
        let handle = ExecHandle {
            id: "/tmp/workspace".into(),
            backend: "cloister".into(),
        };
        let err = provider
            .exec(&handle, &["gemini", "-p", "prompt"])
            .await
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("no declared harness target"),
            "{err:#}"
        );
    }

    #[tokio::test]
    async fn exec_invokes_cloister_run_with_the_declared_harness_and_forwards_only_the_args() {
        // Real subprocess, real /bin/echo standing in for `cloister` — proves
        // the exact argv shape without needing a real cloister install: bin
        // (cmd[0], "claude") is CONSUMED to pick --harness claude-code and
        // does NOT reappear in argv; only cmd[1..] is forwarded after `--`.
        let provider = CloisterProvider::new(PathBuf::from("/bin/echo"));
        let handle = ExecHandle {
            id: "/tmp/rosary-workspace".into(),
            backend: "cloister".into(),
        };
        let result = provider
            .exec(&handle, &["claude", "-p", "the prompt"])
            .await
            .unwrap();
        assert!(result.success());
        assert_eq!(
            result.stdout.trim(),
            "run --harness claude-code --repo /tmp/rosary-workspace -- -p the prompt"
        );
    }

    #[tokio::test]
    async fn destroy_is_a_noop() {
        let provider = CloisterProvider::new(PathBuf::from("/usr/local/bin/cloister"));
        let handle = ExecHandle {
            id: "/tmp/workspace".into(),
            backend: "cloister".into(),
        };
        provider.destroy(&handle).await.unwrap();
    }

    #[test]
    fn provider_name() {
        let provider = CloisterProvider::new(PathBuf::from("/usr/local/bin/cloister"));
        assert_eq!(provider.name(), "cloister");
    }

    #[test]
    fn trait_object_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CloisterProvider>();
    }
}
