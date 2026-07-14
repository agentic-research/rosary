//! Native Codex transport — the experimental app-server / JSON-RPC provider,
//! extracted from providers.rs (rosary-167459). Dormant until config selects it.
//!
//! This file holds the **wire contract** (request envelopes + params) and the
//! provider/session surface. The **runtime** — the persistent connection and
//! the event loop that reads past `turn/start`'s ack to observe real completion
//! — lives in [`super::codex_runtime`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;

use anyhow::Result;

use super::PermissionProfile;
use super::codex_runtime::CodexAppServerRuntime;
use super::codex_transport::CodexUnixSocketConnector;
use super::providers::{AgentProvider, AgentRunSpec};
use super::session::{AgentSession, AgentSessionRef};

/// JSON-RPC request envelope for Codex's remote app-server transport — a minimal wire contract, testable without `codex exec`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[allow(dead_code)] // Native Codex transport seam; exercised by focused adapter tests until config selects it.
pub struct CodexAppServerRequest {
    jsonrpc: &'static str,
    id: String,
    method: &'static str,
    params: serde_json::Value,
}

impl CodexAppServerRequest {
    #[allow(dead_code)] // Used by CodexAppServerRuntime; runtime is dormant until transport config lands.
    pub fn thread_start(id: impl Into<String>, start: &CodexThreadStart) -> Self {
        let work_dir = start.work_dir.display().to_string();
        Self {
            jsonrpc: "2.0",
            id: id.into(),
            method: "thread/start",
            params: serde_json::json!({
                "model": start.model,
                "cwd": work_dir,
                "runtimeWorkspaceRoots": [work_dir],
                "approvalPolicy": codex_approval_policy(start.permissions),
                "sandbox": codex_sandbox_mode(start.permissions),
                "developerInstructions": start.system_prompt,
                "config": codex_config(start),
            }),
        }
    }

    #[allow(dead_code)] // Used by CodexAppServerRuntime; runtime is dormant until transport config lands.
    pub fn turn_start(
        id: impl Into<String>,
        thread_id: impl Into<String>,
        prompt: impl Into<String>,
        work_dir: &Path,
        permissions: PermissionProfile,
        model: Option<String>,
    ) -> Self {
        let work_dir = work_dir.display().to_string();
        Self {
            jsonrpc: "2.0",
            id: id.into(),
            method: "turn/start",
            params: serde_json::json!({
                "threadId": thread_id.into(),
                "input": [{
                    "type": "text",
                    "text": prompt.into(),
                    "textElements": [],
                }],
                "cwd": work_dir,
                "runtimeWorkspaceRoots": [work_dir],
                "approvalPolicy": codex_approval_policy(permissions),
                "sandboxPolicy": codex_sandbox_policy(permissions, &work_dir),
                "model": model,
            }),
        }
    }
}

#[allow(dead_code)] // Used by the dormant Codex app-server request constructors.
fn codex_approval_policy(permissions: PermissionProfile) -> &'static str {
    match permissions {
        PermissionProfile::Implement | PermissionProfile::Plan => "on-request",
        PermissionProfile::ReadOnly => "never",
    }
}

#[allow(dead_code)] // Used by the dormant Codex app-server request constructors.
pub(crate) fn codex_sandbox_mode(permissions: PermissionProfile) -> &'static str {
    match permissions {
        PermissionProfile::Implement => "workspace-write",
        PermissionProfile::ReadOnly | PermissionProfile::Plan => "read-only",
    }
}

#[allow(dead_code)] // Used by the dormant Codex app-server request constructors.
fn codex_sandbox_policy(permissions: PermissionProfile, work_dir: &str) -> serde_json::Value {
    match permissions {
        PermissionProfile::Implement => serde_json::json!({
            "type": "workspaceWrite",
            "writableRoots": [work_dir],
            "networkAccess": false,
            "excludeTmpdirEnvVar": false,
            "excludeSlashTmp": false,
        }),
        PermissionProfile::ReadOnly | PermissionProfile::Plan => serde_json::json!({
            "type": "readOnly",
            "networkAccess": false,
        }),
    }
}

#[allow(dead_code)] // Used by the dormant Codex app-server request constructors.
fn codex_config(start: &CodexThreadStart) -> serde_json::Value {
    serde_json::json!({
        "rsry.bead_id": start.bead_id,
        "rsry.agent_name": start.agent_name,
        "rsry.mcp_servers": start.mcp_servers,
        "rsry.expected_mcp_tools": start.expected_mcp_tools,
    })
}

/// Structured request for the native Codex runtime boundary — the subset of
/// [`AgentRunSpec`] needed to start a thread; the adapter maps it to `ThreadStartParams`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexThreadStart {
    pub bead_id: Option<String>,
    pub agent_name: Option<String>,
    pub prompt: String,
    pub work_dir: PathBuf,
    pub permissions: PermissionProfile,
    pub system_prompt: String,
    pub mcp_servers: BTreeMap<String, String>,
    pub expected_mcp_tools: Vec<String>,
    pub model: Option<String>,
}

impl CodexThreadStart {
    fn from_run_spec(spec: &AgentRunSpec, model: Option<String>) -> Self {
        Self {
            bead_id: spec.bead_id.clone(),
            agent_name: spec.agent_name.clone(),
            prompt: spec.prompt.clone(),
            work_dir: spec.work_dir.clone(),
            permissions: spec.permissions,
            system_prompt: spec.system_prompt.clone(),
            mcp_servers: spec.mcp_servers.clone(),
            expected_mcp_tools: spec.expected_mcp_tools.clone(),
            model,
        }
    }
}

/// Native Codex runtime adapter — calls Codex app-server/protocol APIs directly,
/// with no binary/argv concept, so the durable path can't regress to `codex exec`.
pub trait CodexRuntime: Send + Sync {
    fn start_thread(&self, start: CodexThreadStart) -> Result<CodexNativeSession>;
}

/// Native Codex session handle. It exposes a Codex thread id instead of an OS
/// PID, and its completion is resolved by the runtime's event-loop thread over a
/// oneshot channel (mirroring `ComputeSession`) — so `wait` reflects the turn's
/// real outcome rather than a placeholder.
#[derive(Debug)]
pub struct CodexNativeSession {
    thread_id: String,
    rx: Option<tokio::sync::oneshot::Receiver<bool>>,
    result: Option<bool>,
}

impl CodexNativeSession {
    /// A running turn whose success is delivered by the event-loop thread on `rx`.
    pub fn pending(thread_id: impl Into<String>, rx: tokio::sync::oneshot::Receiver<bool>) -> Self {
        Self {
            thread_id: thread_id.into(),
            rx: Some(rx),
            result: None,
        }
    }

    #[allow(dead_code)] // Pre-resolved handle for runtime tests.
    pub fn completed_success(thread_id: impl Into<String>) -> Self {
        Self {
            thread_id: thread_id.into(),
            rx: None,
            result: Some(true),
        }
    }

    #[allow(dead_code)] // Pre-resolved handle for runtime tests.
    pub fn completed_failure(thread_id: impl Into<String>) -> Self {
        Self {
            thread_id: thread_id.into(),
            rx: None,
            result: Some(false),
        }
    }
}

#[async_trait::async_trait]
impl AgentSession for CodexNativeSession {
    fn try_wait(&mut self) -> Result<Option<bool>> {
        if let Some(result) = self.result {
            return Ok(Some(result));
        }
        let Some(rx) = self.rx.as_mut() else {
            return Ok(None);
        };
        match rx.try_recv() {
            Ok(success) => {
                self.result = Some(success);
                self.rx = None;
                Ok(Some(success))
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => Ok(None),
            // Loop thread dropped the sender without a result — a failed turn,
            // never a hang.
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                self.result = Some(false);
                self.rx = None;
                Ok(Some(false))
            }
        }
    }

    async fn wait(&mut self) -> Result<bool> {
        if let Some(result) = self.result {
            return Ok(result);
        }
        if let Some(rx) = self.rx.take() {
            let success = rx.await.unwrap_or(false);
            self.result = Some(success);
            Ok(success)
        } else {
            Ok(false)
        }
    }

    fn kill(&mut self) -> Result<()> {
        // Drop the receiver and resolve as failed; the event-loop thread's send
        // becomes a no-op and it exits when the turn ends. A cooperative
        // turn/interrupt is a follow-up.
        self.rx = None;
        self.result = Some(false);
        Ok(())
    }

    fn session_ref(&self) -> Option<AgentSessionRef> {
        Some(AgentSessionRef::new("codex", self.thread_id.clone()))
    }
}

/// Provider for Codex native thread/session dispatch — builds no CLI command;
/// consumes [`AgentRunSpec`] and delegates to a native runtime adapter.
#[derive(Clone)]
pub struct CodexProvider {
    runtime: Arc<dyn CodexRuntime>,
    model: Option<String>,
}

impl Default for CodexProvider {
    fn default() -> Self {
        Self {
            runtime: Arc::new(CodexAppServerRuntime::new(Arc::new(
                CodexUnixSocketConnector::default(),
            ))),
            model: None,
        }
    }
}

impl CodexProvider {
    #[cfg(test)]
    pub(crate) fn with_runtime(runtime: Arc<dyn CodexRuntime>) -> Self {
        Self {
            runtime,
            model: None,
        }
    }
}

impl AgentProvider for CodexProvider {
    fn spawn_run(&self, spec: &AgentRunSpec) -> Result<Box<dyn AgentSession>> {
        let start = CodexThreadStart::from_run_spec(spec, self.model.clone());
        let session = self.runtime.start_thread(start)?;
        Ok(Box::new(session))
    }

    fn spawn_agent(
        &self,
        _prompt: &str,
        _work_dir: &Path,
        _permissions: &PermissionProfile,
        _system_prompt: &str,
    ) -> Result<Box<dyn AgentSession>> {
        anyhow::bail!("CodexProvider requires structured spawn_run")
    }

    fn name(&self) -> &str {
        "codex"
    }

    fn with_model(&self, model: Option<String>) -> Box<dyn AgentProvider> {
        Box::new(Self {
            runtime: Arc::clone(&self.runtime),
            model,
        })
    }
}
