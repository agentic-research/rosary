//! MCP server for rosary — exposes beads capabilities as tools over JSON-RPC.
//!
//! Implements the Model Context Protocol (MCP) over stdio transport.
//! Reads line-delimited JSON-RPC from stdin, writes responses to stdout.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

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
    let blocked = beads
        .iter()
        .filter(|b| b.dependency_count > 0 && b.status == "open")
        .count();
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
        "http" => {
            eprintln!("HTTP transport not yet implemented (port={port})");
            eprintln!("Use --transport stdio for now");
            std::process::exit(1);
        }
        other => {
            anyhow::bail!("Unknown transport: {other}. Supported: stdio, http");
        }
    }
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
}
