//! Native Codex transport — the experimental app-server / JSON-RPC provider,
//! extracted from providers.rs (rosary-167459). Dormant until config selects it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tungstenite::{Message, WebSocket};

use anyhow::{Context, Result};

use super::PermissionProfile;
use super::providers::{AgentProvider, AgentRunSpec};
use super::session::{AgentSession, AgentSessionRef};

/// JSON-RPC request envelope for Codex's remote app-server transport — a minimal
/// wire contract so dispatch is testable without the Codex workspace or `codex exec`.
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

/// Minimal app-server client boundary for the native Codex runtime: production
/// speaks Codex's WebSocket/UDS JSON-RPC, tests provide an in-memory client.
#[allow(dead_code)] // Production transport is the next slice; tests exercise the boundary now.
pub trait CodexAppServerClient: Send + Sync {
    fn request(&self, request: CodexAppServerRequest) -> Result<serde_json::Value>;
}

/// Remote Codex app-server client over the local control Unix socket — a native
/// JSON-RPC/WebSocket transport (not `codex exec`) returning Codex's thread id.
pub struct CodexUnixSocketClient {
    socket_path: PathBuf,
}

impl Default for CodexUnixSocketClient {
    fn default() -> Self {
        Self {
            socket_path: default_codex_app_server_socket_path(),
        }
    }
}

impl CodexUnixSocketClient {
    #[cfg(test)]
    pub(crate) fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }
}

impl CodexAppServerClient for CodexUnixSocketClient {
    fn request(&self, request: CodexAppServerRequest) -> Result<serde_json::Value> {
        request_codex_app_server(&self.socket_path, request)
    }
}

/// Native Codex runtime backed by a Codex app-server client.
#[allow(dead_code)] // Production transport is the next slice; tests exercise the boundary now.
pub struct CodexAppServerRuntime {
    client: Arc<dyn CodexAppServerClient>,
}

impl CodexAppServerRuntime {
    #[allow(dead_code)] // Constructed by tests and future configured runtime factory.
    pub fn new(client: Arc<dyn CodexAppServerClient>) -> Self {
        Self { client }
    }
}

impl CodexRuntime for CodexAppServerRuntime {
    fn start_thread(&self, start: CodexThreadStart) -> Result<CodexNativeSession> {
        let thread_response = self
            .client
            .request(CodexAppServerRequest::thread_start(
                "rsry-thread-start",
                &start,
            ))
            .context("starting codex app-server thread")?;
        let thread_id = thread_id_from_thread_start_response(&thread_response)?;
        self.client
            .request(CodexAppServerRequest::turn_start(
                "rsry-turn-start",
                thread_id.clone(),
                start.prompt,
                &start.work_dir,
                start.permissions,
                start.model,
            ))
            .context("starting codex app-server turn")?;
        Ok(CodexNativeSession::running(thread_id))
    }
}

#[allow(dead_code)] // Used by the dormant Codex app-server runtime.
fn thread_id_from_thread_start_response(response: &serde_json::Value) -> Result<String> {
    response
        .get("thread")
        .and_then(|thread| thread.get("id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .context("codex thread/start response missing thread.id")
}

fn default_codex_app_server_socket_path() -> PathBuf {
    if let Ok(path) = std::env::var("RSRY_CODEX_APP_SERVER_SOCKET") {
        return PathBuf::from(path);
    }
    if let Ok(path) = std::env::var("CODEX_APP_SERVER_SOCKET") {
        return PathBuf::from(path);
    }
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(dirs_next::home_dir)
        .map(|home| {
            if home.ends_with(".codex") {
                home
            } else {
                home.join(".codex")
            }
        })
        .unwrap_or_else(|| PathBuf::from(".codex"));
    codex_home
        .join("app-server-control")
        .join("app-server-control.sock")
}

fn request_codex_app_server(
    socket_path: &Path,
    request: CodexAppServerRequest,
) -> Result<serde_json::Value> {
    let stream = std::os::unix::net::UnixStream::connect(socket_path).with_context(|| {
        format!(
            "connecting Codex app-server socket {}",
            socket_path.display()
        )
    })?;
    let (mut websocket, _response) = tungstenite::client::client("ws://localhost/", stream)
        .with_context(|| {
            format!(
                "upgrading Codex app-server socket {}",
                socket_path.display()
            )
        })?;

    initialize_codex_app_server(&mut websocket)?;
    let request_id = request.id.clone();
    send_jsonrpc_value(&mut websocket, &serde_json::to_value(request)?)?;
    read_jsonrpc_result(&mut websocket, &request_id, CODEX_RPC_TIMEOUT)
}

fn initialize_codex_app_server(
    websocket: &mut WebSocket<std::os::unix::net::UnixStream>,
) -> Result<()> {
    send_jsonrpc_value(
        websocket,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": "rsry-initialize",
            "method": "initialize",
            "params": {
                "clientInfo": {
                    "name": "rosary",
                    "title": "Rosary",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "capabilities": {
                    "experimentalApi": true,
                    "requestAttestation": false,
                    "mcpServerOpenaiFormElicitation": false,
                }
            }
        }),
    )?;
    let _ = read_jsonrpc_result(websocket, "rsry-initialize", CODEX_RPC_TIMEOUT)?;
    send_jsonrpc_value(
        websocket,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "method": "initialized",
        }),
    )
}

fn send_jsonrpc_value(
    websocket: &mut WebSocket<std::os::unix::net::UnixStream>,
    value: &serde_json::Value,
) -> Result<()> {
    websocket
        .send(Message::Text(serde_json::to_string(value)?.into()))
        .context("sending Codex app-server JSON-RPC message")
}

/// Default deadline for a single Codex app-server JSON-RPC round-trip. A hung
/// app-server must not block a dispatch forever (rosary-72fc26).
const CODEX_RPC_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) fn read_jsonrpc_result(
    websocket: &mut WebSocket<std::os::unix::net::UnixStream>,
    request_id: &str,
    timeout: Duration,
) -> Result<serde_json::Value> {
    // Total deadline across the whole wait: a server that dribbles keep-alives
    // but never answers still bounds out, and a fully-hung one trips the first read.
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| {
                anyhow::anyhow!("Codex app-server request {request_id} timed out after {timeout:?}")
            })?;
        websocket
            .get_ref()
            .set_read_timeout(Some(remaining))
            .context("setting Codex app-server read timeout")?;
        let message = match websocket.read() {
            Ok(m) => m,
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                anyhow::bail!("Codex app-server request {request_id} timed out after {timeout:?}");
            }
            Err(e) => return Err(e).context("reading Codex app-server JSON-RPC message"),
        };
        let payload = match message {
            Message::Text(text) => text.to_string(),
            Message::Binary(bytes) => String::from_utf8(bytes.to_vec())
                .context("Codex app-server returned non-UTF8 binary JSON-RPC payload")?,
            Message::Close(_) => anyhow::bail!("Codex app-server closed before {request_id}"),
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => continue,
        };
        let value: serde_json::Value =
            serde_json::from_str(&payload).context("parsing Codex app-server JSON-RPC payload")?;
        if value.get("id").and_then(serde_json::Value::as_str) != Some(request_id) {
            continue;
        }
        if let Some(error) = value.get("error") {
            anyhow::bail!("Codex app-server request {request_id} failed: {error}");
        }
        return value
            .get("result")
            .cloned()
            .with_context(|| format!("Codex app-server response {request_id} missing result"));
    }
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

/// Native Codex runtime adapter — calls Codex app-server/protocol APIs directly.
/// Intentionally has no binary/argv concept, so the durable Codex path can't
/// regress to `codex exec`.
pub trait CodexRuntime: Send + Sync {
    fn start_thread(&self, start: CodexThreadStart) -> Result<CodexNativeSession>;
}

/// Native Codex session handle. It exposes a Codex thread id instead of an OS PID.
#[derive(Debug)]
pub struct CodexNativeSession {
    thread_id: String,
    result: Arc<Mutex<Option<bool>>>,
}

impl CodexNativeSession {
    #[allow(dead_code)] // Used by the dormant Codex app-server runtime.
    pub fn running(thread_id: impl Into<String>) -> Self {
        Self {
            thread_id: thread_id.into(),
            result: Arc::new(Mutex::new(None)),
        }
    }

    #[allow(dead_code)] // Public constructor for native runtime adapters and tests.
    pub fn completed_success(thread_id: impl Into<String>) -> Self {
        Self {
            thread_id: thread_id.into(),
            result: Arc::new(Mutex::new(Some(true))),
        }
    }

    #[allow(dead_code)] // Useful for future Codex runtime tests.
    pub fn completed_failure(thread_id: impl Into<String>) -> Self {
        Self {
            thread_id: thread_id.into(),
            result: Arc::new(Mutex::new(Some(false))),
        }
    }
}

#[async_trait::async_trait]
impl AgentSession for CodexNativeSession {
    fn try_wait(&mut self) -> Result<Option<bool>> {
        Ok(*self.result.lock().unwrap())
    }

    async fn wait(&mut self) -> Result<bool> {
        Ok(self.result.lock().unwrap().unwrap_or(false))
    }

    fn kill(&mut self) -> Result<()> {
        *self.result.lock().unwrap() = Some(false);
        Ok(())
    }

    fn session_ref(&self) -> Option<AgentSessionRef> {
        Some(AgentSessionRef::new("codex", self.thread_id.clone()))
    }
}

/// Provider for Codex native thread/session dispatch — unlike Claude/Gemini it
/// builds no CLI command; it consumes [`AgentRunSpec`] and delegates to a native
/// runtime adapter.
#[derive(Clone)]
pub struct CodexProvider {
    runtime: Arc<dyn CodexRuntime>,
    model: Option<String>,
}

impl Default for CodexProvider {
    fn default() -> Self {
        Self {
            runtime: Arc::new(CodexAppServerRuntime::new(Arc::new(
                CodexUnixSocketClient::default(),
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
