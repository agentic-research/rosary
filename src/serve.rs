//! MCP server for rosary — exposes beads capabilities as tools over JSON-RPC.
//!
//! Supports two transports:
//! - **stdio**: line-delimited JSON-RPC over stdin/stdout (default)
//! - **http**: MCP Streamable HTTP transport over a single `/mcp` endpoint

use anyhow::{Context, Result};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::RwLock;

use crate::bead::BeadState;
use crate::config;
use crate::dolt::{DoltClient, DoltConfig};
use crate::pool::RepoPool;
use crate::scanner;

// ---------------------------------------------------------------------------
// JSON-RPC types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

impl JsonRpcResponse {
    fn success(id: Value, result: Value) -> Self {
        JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Value, code: i32, message: String) -> Self {
        JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message,
                data: None,
            }),
        }
    }

    fn method_not_found(id: Value, method: &str) -> Self {
        Self::error(id, -32601, format!("Method not found: {method}"))
    }
}

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

fn tool_definitions() -> Value {
    json!({
        "tools": [
            {
                "name": "rsry_scan",
                "description": "Scan all configured repos for beads (work items). Returns a JSON array of beads with their status, priority, and metadata.",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            },
            {
                "name": "rsry_status",
                "description": "Return aggregated status counts across all repos: open, ready, in_progress, and blocked bead counts.",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            },
            {
                "name": "rsry_list_beads",
                "description": "List all beads with optional status filter. Returns a JSON array of matching beads.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "status": {
                            "type": "string",
                            "description": "Filter by status (open, in_progress, blocked, done, etc.). If omitted, returns all beads."
                        }
                    },
                    "required": []
                }
            },
            {
                "name": "rsry_run_once",
                "description": "Run a single reconciliation pass (scan, triage, dispatch, verify). Use dry_run=true to preview without spawning agents.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "dry_run": {
                            "type": "boolean",
                            "description": "If true, print what would be dispatched without actually spawning agents. Defaults to true.",
                            "default": true
                        }
                    },
                    "required": []
                }
            },
            {
                "name": "rsry_bead_create",
                "description": "Create a new bead (work item) in a repo's Dolt database.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "repo_path": { "type": "string", "description": "Path to repo with .beads/ directory" },
                        "title": { "type": "string", "description": "Bead title" },
                        "description": { "type": "string", "description": "Bead description", "default": "" },
                        "priority": { "type": "integer", "description": "Priority 0-3 (0=P0 highest)", "default": 2 },
                        "issue_type": { "type": "string", "description": "Issue type (bug, task, feature, review, epic)", "default": "task" }
                    },
                    "required": ["repo_path", "title"]
                }
            },
            {
                "name": "rsry_bead_close",
                "description": "Close a bead by ID.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "repo_path": { "type": "string", "description": "Path to repo with .beads/ directory" },
                        "id": { "type": "string", "description": "Bead ID to close" }
                    },
                    "required": ["repo_path", "id"]
                }
            },
            {
                "name": "rsry_bead_comment",
                "description": "Add a comment to a bead.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "repo_path": { "type": "string", "description": "Path to repo with .beads/ directory" },
                        "id": { "type": "string", "description": "Bead ID" },
                        "body": { "type": "string", "description": "Comment text" }
                    },
                    "required": ["repo_path", "id", "body"]
                }
            },
            {
                "name": "rsry_bead_search",
                "description": "Search beads by title/description substring.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "repo_path": { "type": "string", "description": "Path to repo with .beads/ directory" },
                        "query": { "type": "string", "description": "Search query" }
                    },
                    "required": ["repo_path", "query"]
                }
            },
            {
                "name": "rsry_dispatch",
                "description": "Dispatch an agent to work on a specific bead. Spawns a Claude/Gemini agent in the bead's repo with appropriate permissions.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "bead_id": { "type": "string", "description": "Bead ID to dispatch" },
                        "repo_path": { "type": "string", "description": "Path to repo containing the bead" },
                        "provider": { "type": "string", "description": "Agent provider (claude, gemini, acp)", "default": "claude" }
                    },
                    "required": ["bead_id", "repo_path"]
                }
            },
            {
                "name": "rsry_active",
                "description": "Show currently running agent sessions with bead ID, repo, provider, elapsed time.",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            }
        ]
    })
}

// ---------------------------------------------------------------------------
// Tool execution
// ---------------------------------------------------------------------------

async fn call_tool(name: &str, args: &Value, config_path: &str, pool: &RepoPool) -> Result<Value> {
    match name {
        "rsry_scan" => tool_scan(config_path).await,
        "rsry_status" => tool_status(config_path).await,
        "rsry_list_beads" => {
            let status = args
                .get("status")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            tool_list_beads(config_path, status.as_deref()).await
        }
        "rsry_run_once" => {
            let dry_run = args
                .get("dry_run")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            tool_run_once(config_path, dry_run).await
        }
        "rsry_bead_create" => tool_bead_create(args, pool).await,
        "rsry_bead_close" => tool_bead_close(args, pool).await,
        "rsry_bead_comment" => tool_bead_comment(args, pool).await,
        "rsry_bead_search" => tool_bead_search(args, pool).await,
        "rsry_dispatch" => tool_dispatch(args, config_path).await,
        "rsry_active" => tool_active().await,
        _ => anyhow::bail!("Unknown tool: {name}"),
    }
}

async fn tool_scan(config_path: &str) -> Result<Value> {
    let cfg = config::load(config_path)?;
    let beads = scanner::scan_repos(&cfg.repo).await?;
    Ok(json!({
        "count": beads.len(),
        "beads": beads,
    }))
}

async fn tool_status(config_path: &str) -> Result<Value> {
    let cfg = config::load(config_path)?;
    let beads = scanner::scan_repos(&cfg.repo).await?;

    let open = beads.iter().filter(|b| b.status == "open").count();
    let in_progress = beads.iter().filter(|b| b.status == "in_progress").count();
    let blocked = beads.iter().filter(|b| b.is_blocked()).count();
    let ready = beads.iter().filter(|b| b.is_ready()).count();
    let total = beads.len();

    Ok(json!({
        "total": total,
        "open": open,
        "ready": ready,
        "in_progress": in_progress,
        "blocked": blocked,
    }))
}

async fn tool_list_beads(config_path: &str, status: Option<&str>) -> Result<Value> {
    let cfg = config::load(config_path)?;
    let beads = scanner::scan_repos(&cfg.repo).await?;

    let filtered: Vec<_> = match status {
        Some("blocked") => beads.into_iter().filter(|b| b.is_blocked()).collect(),
        Some("ready") => beads.into_iter().filter(|b| b.is_ready()).collect(),
        Some(s) => beads.into_iter().filter(|b| b.status == s).collect(),
        None => beads,
    };

    Ok(json!({
        "count": filtered.len(),
        "beads": filtered,
    }))
}

async fn tool_run_once(config_path: &str, dry_run: bool) -> Result<Value> {
    use crate::reconcile::{Reconciler, ReconcilerConfig};
    use std::time::Duration;

    let cfg = config::load(config_path)?;

    let reconciler_config = ReconcilerConfig {
        max_concurrent: 5,
        scan_interval: Duration::from_secs(30),
        repo: cfg.repo,
        once: true,
        dry_run,
        compute: cfg.compute,
        ..Default::default()
    };

    let mut reconciler = Reconciler::new(reconciler_config);
    let summary = reconciler.iterate().await?;

    Ok(json!({
        "scanned": summary.scanned,
        "triaged": summary.triaged,
        "dispatched": summary.dispatched,
        "completed": summary.completed,
        "passed": summary.passed,
        "failed": summary.failed,
        "deadlettered": summary.deadlettered,
        "dry_run": dry_run,
    }))
}

// ---------------------------------------------------------------------------
// Bead CRUD helpers
// ---------------------------------------------------------------------------

/// Get a DoltClient — try the pool first (by name then path), fall back to fresh connect.
async fn get_client<'a>(repo_path: &str, pool: &'a RepoPool) -> Result<ClientRef<'a>> {
    // Try by repo name (last path component)
    let name = repo_name_from_path(repo_path);
    if let Some(client) = pool.get(&name) {
        return Ok(ClientRef::Pooled(client));
    }
    // Try by full path
    if let Some((_name, client)) = pool.get_by_path(repo_path) {
        return Ok(ClientRef::Pooled(client));
    }
    let path = std::path::Path::new(repo_path);
    let root = config::discover_repo_root(path).unwrap_or_else(|| path.to_path_buf());
    // Worktrees (e.g. .claude/worktrees/X) don't have .beads/ — resolve via git
    let beads_dir = if root.join(".beads").exists() {
        root.join(".beads")
    } else {
        // Try to find the main worktree's .beads/ via git commondir
        let git_common = std::process::Command::new("git")
            .args(["rev-parse", "--git-common-dir"])
            .current_dir(&root)
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    String::from_utf8(o.stdout).ok()
                } else {
                    None
                }
            })
            .map(|s| std::path::PathBuf::from(s.trim()));
        if let Some(common) = git_common {
            // git common dir is .git in main worktree — parent is repo root
            let main_root = common.parent().unwrap_or(&root);
            main_root.join(".beads")
        } else {
            root.join(".beads")
        }
    };
    let config = DoltConfig::from_beads_dir(&beads_dir)?;
    let client = DoltClient::connect(&config).await?;
    Ok(ClientRef::Owned(client))
}

enum ClientRef<'a> {
    Pooled(&'a DoltClient),
    Owned(DoltClient),
}

impl std::ops::Deref for ClientRef<'_> {
    type Target = DoltClient;
    fn deref(&self) -> &DoltClient {
        match self {
            ClientRef::Pooled(c) => c,
            ClientRef::Owned(c) => c,
        }
    }
}

fn repo_name_from_path(repo_path: &str) -> String {
    std::path::Path::new(repo_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".into())
}

async fn tool_bead_create(args: &Value, pool: &RepoPool) -> Result<Value> {
    let repo_path = args["repo_path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("repo_path required"))?;
    let title = args["title"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("title required"))?;
    let description = args["description"].as_str().unwrap_or("");
    let priority = args["priority"].as_u64().unwrap_or(2) as u8;
    let issue_type = args["issue_type"].as_str().unwrap_or("task");

    let client = get_client(repo_path, pool).await?;
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let id = format!("rsry-{:06x}", millis & 0xffffff);

    client
        .create_bead(&id, title, description, priority, issue_type)
        .await?;

    Ok(json!({ "id": id, "title": title, "priority": priority }))
}

async fn tool_bead_close(args: &Value, pool: &RepoPool) -> Result<Value> {
    let repo_path = args["repo_path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("repo_path required"))?;
    let id = args["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("id required"))?;

    let client = get_client(repo_path, pool).await?;
    client.close_bead(id).await?;

    Ok(json!({ "id": id, "status": "closed" }))
}

async fn tool_bead_comment(args: &Value, pool: &RepoPool) -> Result<Value> {
    let repo_path = args["repo_path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("repo_path required"))?;
    let id = args["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("id required"))?;
    let body = args["body"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("body required"))?;

    let client = get_client(repo_path, pool).await?;
    client.add_comment(id, body, "rsry-mcp").await?;

    Ok(json!({ "id": id, "comment_added": true }))
}

async fn tool_bead_search(args: &Value, pool: &RepoPool) -> Result<Value> {
    let repo_path = args["repo_path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("repo_path required"))?;
    let query_str = args["query"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("query required"))?;

    let client = get_client(repo_path, pool).await?;
    let repo_name = repo_name_from_path(repo_path);
    let beads = client.search_beads(query_str, &repo_name).await?;

    Ok(json!({ "count": beads.len(), "beads": beads }))
}

async fn tool_dispatch(args: &Value, _config_path: &str) -> Result<Value> {
    let bead_id = args["bead_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("bead_id required"))?;
    let repo_path = args["repo_path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("repo_path required"))?;
    let provider_name = args["provider"].as_str().unwrap_or("claude");

    // Find the bead
    let path = std::path::Path::new(repo_path);
    let root = config::discover_repo_root(path).unwrap_or_else(|| path.to_path_buf());
    let beads_dir = root.join(".beads");
    let dolt_config = DoltConfig::from_beads_dir(&beads_dir)?;
    let client = DoltClient::connect(&dolt_config).await?;

    let repo_name = root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".into());

    let bead = client
        .get_bead(bead_id, &repo_name)
        .await?
        .ok_or_else(|| anyhow::anyhow!("bead {bead_id} not found"))?;

    // Resolve provider and dispatch
    let provider = crate::dispatch::provider_by_name(provider_name)?;
    let handle =
        crate::dispatch::spawn(&bead, &root, true, bead.generation(), provider.as_ref()).await?;

    // Update status
    let _ = client.update_status(bead_id, "dispatched").await;

    Ok(json!({
        "bead_id": bead_id,
        "status": "dispatched",
        "provider": provider_name,
        "pid": handle.child.id(),
        "work_dir": handle.work_dir.to_string_lossy(),
    }))
}

async fn tool_active() -> Result<Value> {
    // Read active sessions from the daemon's PID file + process list
    // For now, scan for running claude/gemini -p processes
    let output = tokio::process::Command::new("ps")
        .args(["aux"])
        .output()
        .await?;

    let ps_output = String::from_utf8_lossy(&output.stdout);
    let agents: Vec<Value> = ps_output
        .lines()
        .filter(|line| line.contains("claude -p") || line.contains("gemini -p"))
        .filter(|line| !line.contains("grep"))
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 11 {
                return None;
            }
            let pid = parts[1];
            let cpu = parts[2];
            let mem = parts[3];
            // Extract bead ID from the command line if present
            let cmd = parts[10..].join(" ");
            let bead_id = cmd
                .find("Bead ID: ")
                .map(|i| {
                    cmd[i + 9..]
                        .split('\n')
                        .next()
                        .unwrap_or("")
                        .split("\\012")
                        .next()
                        .unwrap_or("")
                        .trim()
                })
                .unwrap_or("unknown");
            let provider = if cmd.contains("claude") {
                "claude"
            } else {
                "gemini"
            };

            Some(json!({
                "pid": pid,
                "provider": provider,
                "bead_id": bead_id,
                "cpu": cpu,
                "mem": mem,
            }))
        })
        .collect();

    Ok(json!({
        "active": agents.len(),
        "agents": agents,
    }))
}

// ---------------------------------------------------------------------------
// Message handling
// ---------------------------------------------------------------------------

fn handle_initialize(id: Value) -> JsonRpcResponse {
    JsonRpcResponse::success(
        id,
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": "rosary",
                "version": env!("CARGO_PKG_VERSION")
            }
        }),
    )
}

fn handle_tools_list(id: Value) -> JsonRpcResponse {
    JsonRpcResponse::success(id, tool_definitions())
}

async fn handle_tools_call(
    id: Value,
    params: &Value,
    config_path: &str,
    pool: &RepoPool,
) -> JsonRpcResponse {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");

    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    match call_tool(name, &args, config_path, pool).await {
        Ok(result) => JsonRpcResponse::success(
            id,
            json!({
                "content": [
                    {
                        "type": "text",
                        "text": serde_json::to_string_pretty(&result).unwrap_or_default()
                    }
                ]
            }),
        ),
        Err(e) => JsonRpcResponse::success(
            id,
            json!({
                "content": [
                    {
                        "type": "text",
                        "text": format!("Error: {e}")
                    }
                ],
                "isError": true
            }),
        ),
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Run the MCP server.
pub async fn run(transport: &str, port: u16) -> Result<()> {
    match transport {
        "stdio" => run_stdio(&crate::config::resolve_config_path()).await,
        "http" => run_http(&crate::config::resolve_config_path(), port).await,
        other => {
            anyhow::bail!("Unknown transport: {other}. Supported: stdio, http");
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP transport (MCP Streamable HTTP)
// ---------------------------------------------------------------------------

/// Shared state for the HTTP transport.
#[derive(Clone)]
struct AppState {
    pool: Arc<RepoPool>,
    config_path: Arc<str>,
    sessions: Arc<RwLock<HashSet<String>>>,
    /// Webhook signing secret (from config or env).
    webhook_secret: Option<Arc<str>>,
}

/// Validate Origin header to prevent DNS rebinding attacks.
#[allow(clippy::result_large_err)]
fn validate_origin(
    headers: &axum::http::HeaderMap,
) -> std::result::Result<(), axum::response::Response> {
    use axum::response::IntoResponse;
    if let Some(origin) = headers.get("origin") {
        let o = origin.to_str().unwrap_or("");
        let allowed = o.starts_with("http://localhost")
            || o.starts_with("http://127.0.0.1")
            || o.starts_with("https://localhost")
            || o.starts_with("https://127.0.0.1");
        if !allowed {
            return Err((axum::http::StatusCode::FORBIDDEN, "Origin not allowed").into_response());
        }
    }
    Ok(())
}

/// Validate Accept header includes both required MIME types.
#[allow(clippy::result_large_err)]
fn validate_accept(
    headers: &axum::http::HeaderMap,
) -> std::result::Result<(), axum::response::Response> {
    use axum::response::IntoResponse;
    let accept = headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !accept.contains("application/json") || !accept.contains("text/event-stream") {
        return Err((
            axum::http::StatusCode::NOT_ACCEPTABLE,
            "Accept must include application/json and text/event-stream",
        )
            .into_response());
    }
    Ok(())
}

/// Validate session ID is present and known.
async fn validate_session(
    headers: &axum::http::HeaderMap,
    sessions: &RwLock<HashSet<String>>,
) -> std::result::Result<(), axum::response::Response> {
    use axum::response::IntoResponse;
    let session_id = headers.get("mcp-session-id").and_then(|v| v.to_str().ok());
    match session_id {
        None => Err((
            axum::http::StatusCode::BAD_REQUEST,
            "Missing Mcp-Session-Id header",
        )
            .into_response()),
        Some(id) => {
            if sessions.read().await.contains(id) {
                Ok(())
            } else {
                Err((axum::http::StatusCode::NOT_FOUND, "Unknown session").into_response())
            }
        }
    }
}

/// Build a JSON response with optional extra header.
fn json_response(
    status: axum::http::StatusCode,
    response: &JsonRpcResponse,
    extra_header: Option<(&str, &str)>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let body = serde_json::to_value(response).unwrap_or(Value::Null);
    let mut resp = (status, axum::Json(body)).into_response();
    if let Some((key, val)) = extra_header
        && let (Ok(k), Ok(v)) = (
            key.parse::<axum::http::header::HeaderName>(),
            val.parse::<axum::http::header::HeaderValue>(),
        )
    {
        resp.headers_mut().insert(k, v);
    }
    resp
}

/// POST /mcp — main JSON-RPC handler.
async fn handle_mcp_post(
    axum::extract::State(state): axum::extract::State<AppState>,
    headers: axum::http::HeaderMap,
    body: String,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    if let Err(resp) = validate_origin(&headers) {
        return resp;
    }
    if let Err(resp) = validate_accept(&headers) {
        return resp;
    }

    let request: JsonRpcRequest = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => {
            let err = JsonRpcResponse::error(Value::Null, -32700, format!("Parse error: {e}"));
            return json_response(axum::http::StatusCode::OK, &err, None);
        }
    };

    if request.jsonrpc != "2.0" {
        if let Some(id) = request.id {
            let resp = JsonRpcResponse::error(
                id,
                -32600,
                "Invalid Request: jsonrpc must be \"2.0\"".into(),
            );
            return json_response(axum::http::StatusCode::OK, &resp, None);
        }
        return axum::http::StatusCode::BAD_REQUEST.into_response();
    }

    // Notifications (no id) — accept silently
    if request.id.is_none() {
        return axum::http::StatusCode::ACCEPTED.into_response();
    }

    let id = request.id.unwrap();
    let is_initialize = request.method == "initialize";

    // Session validation (skip for initialize)
    if !is_initialize && let Err(resp) = validate_session(&headers, &state.sessions).await {
        return resp;
    }

    let response = match request.method.as_str() {
        "initialize" => handle_initialize(id.clone()),
        "tools/list" => handle_tools_list(id),
        "tools/call" => {
            handle_tools_call(id, &request.params, &state.config_path, &state.pool).await
        }
        _ => JsonRpcResponse::method_not_found(id, &request.method),
    };

    if is_initialize {
        let session_id = uuid::Uuid::new_v4().to_string();
        state.sessions.write().await.insert(session_id.clone());
        eprintln!("[rsry-mcp] new session: {session_id}");
        return json_response(
            axum::http::StatusCode::OK,
            &response,
            Some(("mcp-session-id", &session_id)),
        );
    }

    json_response(axum::http::StatusCode::OK, &response, None)
}

/// GET /mcp — SSE stream for server→client notifications (v1: not supported).
async fn handle_mcp_get() -> axum::http::StatusCode {
    axum::http::StatusCode::METHOD_NOT_ALLOWED
}

/// DELETE /mcp — terminate session.
async fn handle_mcp_delete(
    axum::extract::State(state): axum::extract::State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::http::StatusCode {
    let session_id = headers.get("mcp-session-id").and_then(|v| v.to_str().ok());
    match session_id {
        Some(id) => {
            if state.sessions.write().await.remove(id) {
                eprintln!("[rsry-mcp] session terminated: {id}");
                axum::http::StatusCode::OK
            } else {
                axum::http::StatusCode::NOT_FOUND
            }
        }
        None => axum::http::StatusCode::BAD_REQUEST,
    }
}

// ---------------------------------------------------------------------------
// Linear webhook types
// ---------------------------------------------------------------------------

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Deserialize)]
struct WebhookPayload {
    action: String,
    #[serde(rename = "type")]
    entity_type: String,
    data: Option<WebhookIssueData>,
    #[serde(rename = "webhookTimestamp")]
    #[allow(dead_code)]
    webhook_timestamp: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct WebhookIssueData {
    identifier: Option<String>,
    state: Option<WebhookState>,
}

#[derive(Debug, Deserialize)]
struct WebhookState {
    name: String,
    #[serde(rename = "type")]
    state_type: String,
}

/// Verify HMAC-SHA256 signature from Linear webhook.
/// Uses constant-time comparison via the hmac crate's `verify_slice`.
fn verify_webhook_signature(body: &[u8], secret: &[u8], signature_hex: &str) -> bool {
    let Ok(signature_bytes) = hex::decode(signature_hex) else {
        return false;
    };
    let Ok(mut mac) = HmacSha256::new_from_slice(secret) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&signature_bytes).is_ok()
}

/// POST /webhook — Linear webhook handler.
///
/// Receives webhook payloads from Linear, verifies HMAC-SHA256 signature,
/// and updates bead status in Dolt when an Issue state changes.
async fn handle_webhook(
    axum::extract::State(state): axum::extract::State<AppState>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    // 1. Read webhook secret from state (loaded from config or env at startup)
    let secret = match &state.webhook_secret {
        Some(s) => s.clone(),
        None => {
            eprintln!(
                "[webhook] webhook_secret not configured (set [linear].webhook_secret in config or LINEAR_WEBHOOK_SECRET env)"
            );
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "webhook secret not configured",
            )
                .into_response();
        }
    };

    // 2. Extract Linear-Signature header (hex-encoded HMAC-SHA256)
    let signature = match headers
        .get("linear-signature")
        .and_then(|v| v.to_str().ok())
    {
        Some(s) => s.to_string(),
        None => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                "missing Linear-Signature header",
            )
                .into_response();
        }
    };

    // 3. Verify HMAC-SHA256 (constant-time comparison)
    if !verify_webhook_signature(&body, secret.as_bytes(), &signature) {
        eprintln!("[webhook] HMAC verification failed");
        return (axum::http::StatusCode::UNAUTHORIZED, "invalid signature").into_response();
    }

    // 4. Extract Linear-Event header for entity type filtering
    let event_type = headers
        .get("linear-event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // 5. Parse the JSON payload
    let payload: WebhookPayload = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[webhook] failed to parse payload: {e}");
            return (axum::http::StatusCode::BAD_REQUEST, "invalid JSON payload").into_response();
        }
    };

    // 6. Only process Issue update events
    if payload.action != "update" || payload.entity_type != "Issue" {
        eprintln!(
            "[webhook] ignoring {}/{} event (type={})",
            payload.action, payload.entity_type, event_type
        );
        return (axum::http::StatusCode::OK, "ignored").into_response();
    }

    // 7. Extract state info from the payload
    let (identifier, bead_state) = match &payload.data {
        Some(data) => {
            let ident = match &data.identifier {
                Some(id) => id.clone(),
                None => {
                    eprintln!("[webhook] Issue update missing identifier");
                    return (axum::http::StatusCode::OK, "no identifier").into_response();
                }
            };
            let state = match &data.state {
                Some(s) => BeadState::from_linear_type(&s.state_type, &s.name),
                None => {
                    eprintln!("[webhook] Issue update missing state");
                    return (axum::http::StatusCode::OK, "no state").into_response();
                }
            };
            (ident, state)
        }
        None => {
            eprintln!("[webhook] Issue update missing data");
            return (axum::http::StatusCode::OK, "no data").into_response();
        }
    };

    // 8. Find the bead by external_ref across all repos in the pool
    let mut found = false;
    for (repo_name, client) in state.pool.iter_clients() {
        match client.find_by_external_ref(&identifier).await {
            Ok(Some(bead_id)) => {
                let new_status = bead_state.to_string();
                match client.update_status(&bead_id, &new_status).await {
                    Ok(()) => {
                        eprintln!(
                            "[webhook] updated bead {bead_id} in {repo_name}: {identifier} -> {new_status}"
                        );
                        client
                            .log_event(
                                &bead_id,
                                "webhook_update",
                                &format!("Linear {identifier} state -> {new_status}"),
                            )
                            .await;
                        found = true;
                        break;
                    }
                    Err(e) => {
                        eprintln!("[webhook] failed to update bead {bead_id}: {e}");
                    }
                }
            }
            Ok(None) => continue,
            Err(e) => {
                eprintln!("[webhook] error searching {repo_name} for {identifier}: {e}");
            }
        }
    }

    if !found {
        eprintln!("[webhook] no bead found for external_ref={identifier}");
    }

    (axum::http::StatusCode::OK, "ok").into_response()
}

async fn run_http(config_path: &str, port: u16) -> Result<()> {
    let cfg = config::load(config_path)?;
    let webhook_secret = cfg
        .linear
        .as_ref()
        .and_then(|l| l.webhook_secret.clone())
        .or_else(|| std::env::var("LINEAR_WEBHOOK_SECRET").ok())
        .filter(|s| !s.is_empty());

    let pool = Arc::new(RepoPool::from_config(config_path).await?);
    let state = AppState {
        pool: pool.clone(),
        config_path: Arc::from(config_path),
        sessions: Arc::new(RwLock::new(HashSet::new())),
        webhook_secret: webhook_secret.map(|s| Arc::from(s.as_str())),
    };

    let app = axum::Router::new()
        .route(
            "/mcp",
            axum::routing::post(handle_mcp_post)
                .get(handle_mcp_get)
                .delete(handle_mcp_delete),
        )
        .route("/webhook", axum::routing::post(handle_webhook))
        .layer(axum::extract::DefaultBodyLimit::max(1024 * 1024))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    eprintln!(
        "[rsry-mcp] HTTP server listening on http://127.0.0.1:{port}/mcp ({} repos: {})",
        pool.len(),
        pool.repo_names().join(", ")
    );

    axum::serve(listener, app).await?;
    Ok(())
}

async fn run_stdio(config_path: &str) -> Result<()> {
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let reader = BufReader::new(stdin);
    let mut lines = reader.lines();

    // Create connection pool on startup — reused across all tool calls
    let pool = RepoPool::from_config(config_path).await?;
    eprintln!(
        "[rsry-mcp] server started (stdio transport, {} repos: {})",
        pool.len(),
        pool.repo_names().join(", ")
    );

    while let Some(line) = lines.next_line().await.context("reading stdin")? {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let err_resp =
                    JsonRpcResponse::error(Value::Null, -32700, format!("Parse error: {e}"));
                let mut out = serde_json::to_string(&err_resp)?;
                out.push('\n');
                stdout.write_all(out.as_bytes()).await?;
                stdout.flush().await?;
                continue;
            }
        };

        // Validate jsonrpc version
        if request.jsonrpc != "2.0" {
            if let Some(id) = request.id {
                let resp = JsonRpcResponse::error(
                    id,
                    -32600,
                    "Invalid Request: jsonrpc must be \"2.0\"".to_string(),
                );
                let mut out = serde_json::to_string(&resp)?;
                out.push('\n');
                stdout.write_all(out.as_bytes()).await?;
                stdout.flush().await?;
            }
            continue;
        }

        // Notifications (no id) — handle but don't respond
        if request.id.is_none() {
            // "notifications/initialized" is expected after initialize — just acknowledge silently
            continue;
        }

        let id = request.id.unwrap();

        let response = match request.method.as_str() {
            "initialize" => handle_initialize(id),
            "tools/list" => handle_tools_list(id),
            "tools/call" => handle_tools_call(id, &request.params, config_path, &pool).await,
            _ => JsonRpcResponse::method_not_found(id, &request.method),
        };

        let mut out = serde_json::to_string(&response)?;
        out.push('\n');
        stdout.write_all(out.as_bytes()).await?;
        stdout.flush().await?;
    }

    eprintln!("[rsry-mcp] stdin closed, shutting down");
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_definitions_has_ten_tools() {
        let defs = tool_definitions();
        let tools = defs["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 10);

        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"rsry_scan"));
        assert!(names.contains(&"rsry_status"));
        assert!(names.contains(&"rsry_list_beads"));
        assert!(names.contains(&"rsry_run_once"));
        assert!(names.contains(&"rsry_bead_create"));
        assert!(names.contains(&"rsry_bead_close"));
        assert!(names.contains(&"rsry_bead_comment"));
        assert!(names.contains(&"rsry_bead_search"));
        assert!(names.contains(&"rsry_dispatch"));
        assert!(names.contains(&"rsry_active"));
    }

    #[test]
    fn tool_definitions_have_input_schemas() {
        let defs = tool_definitions();
        let tools = defs["tools"].as_array().unwrap();
        for tool in tools {
            assert!(
                tool.get("inputSchema").is_some(),
                "tool {} missing inputSchema",
                tool["name"]
            );
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn initialize_response_format() {
        let resp = handle_initialize(json!(1));
        let result = resp.result.unwrap();
        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert!(result["capabilities"]["tools"].is_object());
        assert_eq!(result["serverInfo"]["name"], "rosary");
    }

    #[test]
    fn tools_list_response_format() {
        let resp = handle_tools_list(json!(2));
        let result = resp.result.unwrap();
        assert!(result["tools"].is_array());
        assert_eq!(result["tools"].as_array().unwrap().len(), 10);
    }

    #[test]
    fn bead_crud_tools_have_repo_path() {
        let defs = tool_definitions();
        let tools = defs["tools"].as_array().unwrap();
        for tool in tools {
            let name = tool["name"].as_str().unwrap();
            if name.starts_with("rsry_bead_") {
                let props = &tool["inputSchema"]["properties"];
                assert!(
                    props.get("repo_path").is_some(),
                    "{name} missing repo_path parameter"
                );
            }
        }
    }

    #[test]
    fn method_not_found_response() {
        let resp = JsonRpcResponse::method_not_found(json!(99), "bogus/method");
        assert!(resp.error.is_some());
        assert_eq!(resp.error.as_ref().unwrap().code, -32601);
        assert!(
            resp.error
                .as_ref()
                .unwrap()
                .message
                .contains("bogus/method")
        );
        assert!(resp.result.is_none());
    }

    #[test]
    fn parse_request_with_params() {
        let raw = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"rsry_list_beads","arguments":{"status":"open"}}}"#;
        let req: JsonRpcRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(req.method, "tools/call");
        assert_eq!(req.params["name"], "rsry_list_beads");
        assert_eq!(req.params["arguments"]["status"], "open");
    }

    #[test]
    fn parse_notification_no_id() {
        let raw = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let req: JsonRpcRequest = serde_json::from_str(raw).unwrap();
        assert!(req.id.is_none());
        assert_eq!(req.method, "notifications/initialized");
    }

    #[test]
    fn success_response_serialization() {
        let resp = JsonRpcResponse::success(json!(1), json!({"ok": true}));
        let serialized = serde_json::to_string(&resp).unwrap();
        let parsed: Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert_eq!(parsed["id"], 1);
        assert!(parsed["result"]["ok"].as_bool().unwrap());
        // error field should be absent (skip_serializing_if)
        assert!(parsed.get("error").is_none());
    }

    #[test]
    fn error_response_serialization() {
        let resp = JsonRpcResponse::error(json!(2), -32600, "bad request".to_string());
        let serialized = serde_json::to_string(&resp).unwrap();
        let parsed: Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert_eq!(parsed["id"], 2);
        assert_eq!(parsed["error"]["code"], -32600);
        assert_eq!(parsed["error"]["message"], "bad request");
        // result field should be absent
        assert!(parsed.get("result").is_none());
    }

    // --- HTTP transport tests ---

    #[test]
    fn validate_origin_allows_localhost() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("origin", "http://localhost:3000".parse().unwrap());
        assert!(validate_origin(&headers).is_ok());

        headers.insert("origin", "http://127.0.0.1:8080".parse().unwrap());
        assert!(validate_origin(&headers).is_ok());
    }

    #[test]
    fn validate_origin_rejects_external() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("origin", "http://evil.com".parse().unwrap());
        assert!(validate_origin(&headers).is_err());
    }

    #[test]
    fn validate_origin_allows_no_origin() {
        let headers = axum::http::HeaderMap::new();
        assert!(validate_origin(&headers).is_ok());
    }

    #[test]
    fn validate_accept_requires_both_types() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "accept",
            "application/json, text/event-stream".parse().unwrap(),
        );
        assert!(validate_accept(&headers).is_ok());

        headers.insert("accept", "application/json".parse().unwrap());
        assert!(validate_accept(&headers).is_err());

        headers.insert("accept", "text/event-stream".parse().unwrap());
        assert!(validate_accept(&headers).is_err());
    }

    #[tokio::test]
    async fn validate_session_rejects_missing() {
        let sessions = RwLock::new(HashSet::new());
        let headers = axum::http::HeaderMap::new();
        assert!(validate_session(&headers, &sessions).await.is_err());
    }

    #[tokio::test]
    async fn validate_session_rejects_unknown() {
        let sessions = RwLock::new(HashSet::new());
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("mcp-session-id", "unknown-id".parse().unwrap());
        assert!(validate_session(&headers, &sessions).await.is_err());
    }

    #[tokio::test]
    async fn validate_session_accepts_known() {
        let sessions = RwLock::new(HashSet::from(["abc-123".to_string()]));
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("mcp-session-id", "abc-123".parse().unwrap());
        assert!(validate_session(&headers, &sessions).await.is_ok());
    }

    #[tokio::test]
    async fn http_initialize_returns_session_id() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let state = AppState {
            pool: Arc::new(RepoPool::empty()),
            config_path: Arc::from("test.toml"),
            sessions: Arc::new(RwLock::new(HashSet::new())),
            webhook_secret: None,
        };

        let app = axum::Router::new()
            .route("/mcp", axum::routing::post(handle_mcp_post))
            .with_state(state.clone());

        let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#;
        let req = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        assert!(resp.headers().get("mcp-session-id").is_some());

        let sessions = state.sessions.read().await;
        assert_eq!(sessions.len(), 1);
    }

    #[tokio::test]
    async fn http_tools_list_requires_session() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let state = AppState {
            pool: Arc::new(RepoPool::empty()),
            config_path: Arc::from("test.toml"),
            sessions: Arc::new(RwLock::new(HashSet::new())),
            webhook_secret: None,
        };

        let app = axum::Router::new()
            .route("/mcp", axum::routing::post(handle_mcp_post))
            .with_state(state);

        let body = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        let req = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        // No session ID → 400
        assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn http_delete_terminates_session() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let state = AppState {
            pool: Arc::new(RepoPool::empty()),
            config_path: Arc::from("test.toml"),
            sessions: Arc::new(RwLock::new(HashSet::from(["sess-1".to_string()]))),
            webhook_secret: None,
        };

        let app = axum::Router::new()
            .route(
                "/mcp",
                axum::routing::post(handle_mcp_post).delete(handle_mcp_delete),
            )
            .with_state(state.clone());

        let req = Request::builder()
            .method("DELETE")
            .uri("/mcp")
            .header("mcp-session-id", "sess-1")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        assert!(state.sessions.read().await.is_empty());
    }

    // --- Webhook tests ---

    #[test]
    fn webhook_hmac_verification_valid() {
        let secret = b"test-webhook-secret";
        let body = b"hello webhook body";

        // Compute expected HMAC
        let mut mac = HmacSha256::new_from_slice(secret).unwrap();
        mac.update(body);
        let result = mac.finalize();
        let expected_hex = hex::encode(result.into_bytes());

        assert!(verify_webhook_signature(body, secret, &expected_hex));
    }

    #[test]
    fn webhook_hmac_verification_invalid() {
        let secret = b"test-webhook-secret";
        let body = b"hello webhook body";

        // Wrong signature
        let bad_signature = "deadbeef".repeat(8); // 64 hex chars = 32 bytes
        assert!(!verify_webhook_signature(body, secret, &bad_signature));
    }

    #[test]
    fn webhook_hmac_verification_invalid_hex() {
        let secret = b"test-webhook-secret";
        let body = b"hello webhook body";

        // Not valid hex
        assert!(!verify_webhook_signature(body, secret, "not-hex!!!"));
    }

    #[test]
    fn webhook_hmac_verification_wrong_body() {
        let secret = b"test-webhook-secret";
        let body = b"original body";
        let tampered = b"tampered body";

        // Compute HMAC for original body
        let mut mac = HmacSha256::new_from_slice(secret).unwrap();
        mac.update(body);
        let sig = hex::encode(mac.finalize().into_bytes());

        // Verify against tampered body should fail
        assert!(!verify_webhook_signature(tampered, secret, &sig));
    }

    #[test]
    fn webhook_payload_parse_issue_update() {
        let raw = r#"{
            "action": "update",
            "type": "Issue",
            "data": {
                "identifier": "AGE-42",
                "state": {
                    "name": "In Progress",
                    "type": "started"
                }
            },
            "webhookTimestamp": 1710000000000
        }"#;

        let payload: WebhookPayload = serde_json::from_str(raw).unwrap();
        assert_eq!(payload.action, "update");
        assert_eq!(payload.entity_type, "Issue");
        assert_eq!(payload.webhook_timestamp, Some(1710000000000));

        let data = payload.data.unwrap();
        assert_eq!(data.identifier.unwrap(), "AGE-42");
        let state = data.state.unwrap();
        assert_eq!(state.name, "In Progress");
        assert_eq!(state.state_type, "started");

        // Verify the mapping to BeadState
        let bead_state = BeadState::from_linear_type(&state.state_type, &state.name);
        assert_eq!(bead_state, BeadState::Dispatched);
    }

    #[test]
    fn webhook_payload_parse_completed() {
        let raw = r#"{
            "action": "update",
            "type": "Issue",
            "data": {
                "identifier": "AGE-99",
                "state": {
                    "name": "Done",
                    "type": "completed"
                }
            }
        }"#;

        let payload: WebhookPayload = serde_json::from_str(raw).unwrap();
        let data = payload.data.unwrap();
        let state = data.state.unwrap();
        let bead_state = BeadState::from_linear_type(&state.state_type, &state.name);
        assert_eq!(bead_state, BeadState::Done);
    }

    #[test]
    fn webhook_payload_parse_non_issue_ignored() {
        let raw = r#"{
            "action": "create",
            "type": "Comment",
            "data": {
                "body": "some comment"
            }
        }"#;

        let payload: WebhookPayload = serde_json::from_str(raw).unwrap();
        assert_eq!(payload.entity_type, "Comment");
        // Non-Issue events should be ignored by the handler
        assert!(
            payload.entity_type != "Issue" || payload.action != "update",
            "this should not be processed as an Issue update"
        );
    }

    #[test]
    fn webhook_payload_parse_non_update_action_ignored() {
        let raw = r#"{
            "action": "create",
            "type": "Issue",
            "data": {
                "identifier": "AGE-1",
                "state": {
                    "name": "Todo",
                    "type": "unstarted"
                }
            }
        }"#;

        let payload: WebhookPayload = serde_json::from_str(raw).unwrap();
        assert_eq!(payload.action, "create");
        assert_eq!(payload.entity_type, "Issue");
        // create action should be ignored
        assert_ne!(payload.action, "update");
    }

    #[tokio::test]
    async fn webhook_rejects_missing_signature() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let state = AppState {
            pool: Arc::new(RepoPool::empty()),
            config_path: Arc::from("test.toml"),
            sessions: Arc::new(RwLock::new(HashSet::new())),
            webhook_secret: Some(Arc::from("test-secret")),
        };

        let app = axum::Router::new()
            .route("/webhook", axum::routing::post(handle_webhook))
            .with_state(state);

        let req = Request::builder()
            .method("POST")
            .uri("/webhook")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"action":"update","type":"Issue"}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn webhook_rejects_invalid_signature() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let state = AppState {
            pool: Arc::new(RepoPool::empty()),
            config_path: Arc::from("test.toml"),
            sessions: Arc::new(RwLock::new(HashSet::new())),
            webhook_secret: Some(Arc::from("test-secret")),
        };

        let app = axum::Router::new()
            .route("/webhook", axum::routing::post(handle_webhook))
            .with_state(state);

        let body = r#"{"action":"update","type":"Issue"}"#;
        let req = Request::builder()
            .method("POST")
            .uri("/webhook")
            .header("content-type", "application/json")
            .header(
                "linear-signature",
                "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            )
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn webhook_accepts_valid_signature_ignores_non_issue() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let secret = "test-secret-for-webhook";

        let state = AppState {
            pool: Arc::new(RepoPool::empty()),
            config_path: Arc::from("test.toml"),
            sessions: Arc::new(RwLock::new(HashSet::new())),
            webhook_secret: Some(Arc::from(secret)),
        };

        let app = axum::Router::new()
            .route("/webhook", axum::routing::post(handle_webhook))
            .with_state(state);

        let body = r#"{"action":"create","type":"Comment","data":null}"#;

        // Compute valid HMAC
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body.as_bytes());
        let sig = hex::encode(mac.finalize().into_bytes());

        let req = Request::builder()
            .method("POST")
            .uri("/webhook")
            .header("content-type", "application/json")
            .header("linear-signature", sig)
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        // Valid signature, but non-Issue event -> 200 OK (ignored)
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
    }
}
