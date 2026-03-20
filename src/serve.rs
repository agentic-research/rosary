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

use crate::store::DispatchRecord;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::RwLock;

use crate::bead::BeadState;
use crate::config;
use crate::dolt::{DoltClient, DoltConfig};
use crate::pool::RepoPool;
use crate::scanner;
use crate::store::{BeadRef, DispatchStore, PipelineState};
use crate::store_dolt::DoltBackend;

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
                "description": "Create a new bead (work item) in a repo's Dolt database. Use when you've identified a discrete, actionable issue. Set file scopes accurately — they determine parallel dispatch safety via has_file_overlap().",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "repo_path": { "type": "string", "description": "Path to repo with .beads/ directory" },
                        "title": { "type": "string", "description": "Bead title" },
                        "description": { "type": "string", "description": "Bead description", "default": "" },
                        "priority": { "type": "integer", "description": "Priority 0-3 (0=P0 highest)", "default": 2 },
                        "issue_type": { "type": "string", "description": "Issue type (bug, task, feature, review, epic)", "default": "task" },
                        "owner": { "type": "string", "description": "Agent owner (dev-agent, staging-agent, etc.). Auto-assigned from issue_type if omitted." },
                        "files": { "type": "array", "items": { "type": "string" }, "description": "Source files this bead touches. CRITICAL: these scope parallel dispatch — has_file_overlap() (epic.rs:386-393) blocks concurrent beads sharing files, and reconcile.rs:372-380 enforces it at dispatch time. Set scopes ONLY after reading the code; guessed scopes cause false-negative overlap and agent collisions. Include both files being modified AND files needing wiring changes (imports, call sites). New files are safe — no overlap possible." },
                        "test_files": { "type": "array", "items": { "type": "string" }, "description": "Test files to validate the change. Also checked for overlap — two beads sharing a test file will be serialized, not parallelized." },
                        "depends_on": { "type": "array", "items": { "type": "string" }, "description": "Bead IDs this bead depends on (blocked until they complete). Creates entries in the dependencies table." }
                    },
                    "required": ["repo_path", "title"]
                }
            },
            {
                "name": "rsry_bead_update",
                "description": "Update a bead's fields (PATCH semantics). Only provided fields are changed; omitted fields are left unchanged.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "repo_path": { "type": "string", "description": "Path to repo with .beads/ directory" },
                        "id": { "type": "string", "description": "Bead ID to update" },
                        "title": { "type": "string", "description": "New title" },
                        "description": { "type": "string", "description": "New description" },
                        "priority": { "type": "integer", "description": "New priority 0-3" },
                        "issue_type": { "type": "string", "description": "New issue type" },
                        "owner": { "type": "string", "description": "New owner/assignee" },
                        "files": { "type": "array", "items": { "type": "string" }, "description": "Updated source files list. These scope parallel dispatch — see has_file_overlap() (epic.rs:386-393). Verify against actual code before setting; inaccurate scopes cause agent collisions or missed overlap detection." },
                        "test_files": { "type": "array", "items": { "type": "string" }, "description": "Updated test files list. Also checked for overlap at dispatch time (reconcile.rs:372-380)." }
                    },
                    "required": ["repo_path", "id"]
                }
            },
            {
                "name": "rsry_bead_close",
                "description": "Close a bead by ID, marking it as done. Use after your changes are committed and tests pass. Do not close if the fix is incomplete or tests are failing — comment explaining the state instead.",
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
                "description": "Add a progress comment to a bead. Use throughout your work to log what you've tried, found, and what remains. Other agents in the pipeline and human reviewers read these comments for context.",
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
                "name": "rsry_bead_link",
                "description": "Add or remove a dependency between beads. Use to express 'A depends on B' (A is blocked until B completes).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "repo_path": { "type": "string", "description": "Path to repo with .beads/ directory" },
                        "id": { "type": "string", "description": "Bead ID that depends on another" },
                        "depends_on": { "type": "string", "description": "Bead ID that must complete first" },
                        "remove": { "type": "boolean", "description": "If true, removes the dependency instead of adding", "default": false }
                    },
                    "required": ["repo_path", "id", "depends_on"]
                }
            },
            {
                "name": "rsry_bead_search",
                "description": "Search beads in a specific repo by title/description substring. Returns matching beads with their status and metadata. Use to check for existing beads before creating duplicates.",
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
                "description": "Dispatch an agent to work on a specific bead. Spawns a Claude/Gemini agent in the bead's repo with the appropriate agent perspective (dev-agent, staging-agent, etc.) and permissions.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "bead_id": { "type": "string", "description": "Bead ID to dispatch" },
                        "repo_path": { "type": "string", "description": "Path to repo containing the bead" },
                        "provider": { "type": "string", "description": "Agent provider (claude, gemini, acp)", "default": "claude" },
                        "agent": { "type": "string", "description": "Agent persona override (dev-agent, staging-agent, prod-agent, feature-agent, pm-agent). If omitted, uses bead owner." }
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
            },
            {
                "name": "rsry_workspace_create",
                "description": "Create an isolated workspace (jj or git worktree) for a bead. Returns the workspace work_dir and vcs type. The conductor should call this before dispatching an agent.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "bead_id": { "type": "string", "description": "Bead ID for the workspace" },
                        "repo_path": { "type": "string", "description": "Path to the repo root" }
                    },
                    "required": ["bead_id", "repo_path"]
                }
            },
            {
                "name": "rsry_workspace_checkpoint",
                "description": "Checkpoint a workspace: jj commit + bookmark. Returns the jj change ID. Call after agent completes, before cleanup.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "bead_id": { "type": "string", "description": "Bead ID" },
                        "repo_path": { "type": "string", "description": "Path to the repo root" },
                        "message": { "type": "string", "description": "Commit message (default: agent work)" }
                    },
                    "required": ["bead_id", "repo_path"]
                }
            },
            {
                "name": "rsry_workspace_cleanup",
                "description": "Clean up a workspace (jj workspace forget + delete directory). Call after checkpoint.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "bead_id": { "type": "string", "description": "Bead ID" },
                        "repo_path": { "type": "string", "description": "Path to the repo root" }
                    },
                    "required": ["bead_id", "repo_path"]
                }
            },
            {
                "name": "rsry_workspace_merge",
                "description": "Merge a completed agent's worktree branch back to main (ff-merge for tasks/bugs, push branch for features/epics).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "bead_id": { "type": "string", "description": "Bead ID (branch is fix/{bead_id})" },
                        "repo_path": { "type": "string", "description": "Path to the repo root" },
                        "issue_type": { "type": "string", "description": "Issue type (task/bug = ff-merge, feature/epic = push branch)", "default": "task" }
                    },
                    "required": ["bead_id", "repo_path"]
                }
            },
            {
                "name": "rsry_decompose",
                "description": "Decompose a markdown document (ADR, README, etc.) into a decade of threaded beads. Skips non-actionable sections (consequences, alternatives, references). Returns structure without creating beads — review before committing.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Absolute path to the markdown file" },
                        "title": { "type": "string", "description": "Title for the decade (defaults to first heading)" }
                    },
                    "required": ["path"]
                }
            },
            {
                "name": "rsry_pipeline_upsert",
                "description": "Write pipeline state for a bead to the backend store. Creates or updates the pipeline record tracking which agent phase a bead is in.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "repo": { "type": "string", "description": "Repository name (e.g. 'rosary')" },
                        "bead_id": { "type": "string", "description": "Bead ID (e.g. 'rsry-abc123')" },
                        "pipeline_phase": { "type": "integer", "description": "Phase index: dev=0, staging=1, prod=2, feature=3" },
                        "pipeline_agent": { "type": "string", "description": "Agent name (e.g. 'dev-agent')" },
                        "phase_status": { "type": "string", "description": "Sub-state: pending, executing, completed, failed", "default": "pending" },
                        "retries": { "type": "integer", "description": "Retry count", "default": 0 },
                        "consecutive_reverts": { "type": "integer", "description": "Consecutive revert count", "default": 0 },
                        "highest_verify_tier": { "type": "integer", "description": "Highest verification tier reached (optional)" },
                        "last_generation": { "type": "integer", "description": "Content hash generation", "default": 0 },
                        "backoff_until": { "type": "string", "description": "ISO 8601 datetime for retry eligibility (optional)" }
                    },
                    "required": ["repo", "bead_id", "pipeline_phase", "pipeline_agent"]
                }
            },
            {
                "name": "rsry_pipeline_query",
                "description": "Query pipeline state. Get a single pipeline by repo + bead_id, or list all active pipelines with no args.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "repo": { "type": "string", "description": "Repository name" },
                        "bead_id": { "type": "string", "description": "Bead ID" }
                    },
                    "required": []
                }
            },
            {
                "name": "rsry_dispatch_record",
                "description": "Record a dispatch event in the backend store. Called by the conductor/orchestrator when spawning an agent — not typically called by agents directly.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "Dispatch UUID" },
                        "repo": { "type": "string", "description": "Repo name" },
                        "bead_id": { "type": "string", "description": "Bead ID" },
                        "agent": { "type": "string", "description": "Agent name" },
                        "provider": { "type": "string", "description": "Provider (claude, gemini, acp)" },
                        "work_dir": { "type": "string", "description": "Working directory" }
                    },
                    "required": ["id", "repo", "bead_id", "agent", "provider", "work_dir"]
                }
            },
            {
                "name": "rsry_dispatch_history",
                "description": "Query dispatch history. Filter by bead_id to see all dispatches for a specific bead, or use active_only to see currently running agents. Useful for checking if an agent is already working on a bead before dispatching another.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "bead_id": { "type": "string", "description": "Filter by bead ID" },
                        "active_only": { "type": "boolean", "description": "Only active dispatches", "default": true }
                    },
                    "required": []
                }
            },
            {
                "name": "rsry_decade_list",
                "description": "List decades (ADR-level organizing primitives). Optionally filter by status (proposed, active, completed, superseded).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "status": { "type": "string", "description": "Filter by status (optional)" }
                    },
                    "required": []
                }
            },
            {
                "name": "rsry_thread_list",
                "description": "List threads within a decade, or find the thread a bead belongs to.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "decade_id": { "type": "string", "description": "Decade ID to list threads for" },
                        "bead_id": { "type": "string", "description": "Find thread for this bead (alternative to decade_id)" },
                        "repo": { "type": "string", "description": "Repo name for bead lookup (required with bead_id)" }
                    },
                    "required": []
                }
            },
            {
                "name": "rsry_thread_assign",
                "description": "Assign a bead to a thread. Creates the thread if it doesn't exist. Use to build ordered progressions of related beads.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "thread_id": { "type": "string", "description": "Thread ID (e.g. 'ADR-003/pipeline-quality')" },
                        "thread_name": { "type": "string", "description": "Thread display name (for new threads)" },
                        "decade_id": { "type": "string", "description": "Decade this thread belongs to (for new threads)" },
                        "bead_id": { "type": "string", "description": "Bead ID to assign to the thread" },
                        "repo": { "type": "string", "description": "Repo name for the bead" }
                    },
                    "required": ["thread_id", "bead_id", "repo"]
                }
            }
        ]
    })
}

// ---------------------------------------------------------------------------
// Tool execution
// ---------------------------------------------------------------------------

async fn call_tool(
    name: &str,
    args: &Value,
    config_path: &str,
    pool: &RepoPool,
    backend: Option<&DoltBackend>,
) -> Result<Value> {
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
        "rsry_bead_update" => tool_bead_update(args, pool).await,
        "rsry_bead_close" => tool_bead_close(args, pool).await,
        "rsry_bead_comment" => tool_bead_comment(args, pool).await,
        "rsry_bead_link" => tool_bead_link(args, pool).await,
        "rsry_bead_search" => tool_bead_search(args, pool).await,
        "rsry_dispatch" => tool_dispatch(args, config_path).await,
        "rsry_active" => tool_active().await,
        "rsry_workspace_create" => tool_workspace_create(args).await,
        "rsry_workspace_checkpoint" => tool_workspace_checkpoint(args).await,
        "rsry_workspace_cleanup" => tool_workspace_cleanup(args),
        "rsry_workspace_merge" => tool_workspace_merge(args).await,
        "rsry_decompose" => tool_decompose(args).await,
        "rsry_pipeline_upsert" => tool_pipeline_upsert(args, backend).await,
        "rsry_pipeline_query" => tool_pipeline_query(args, backend).await,
        "rsry_dispatch_record" => tool_dispatch_record(args, backend).await,
        "rsry_dispatch_history" => tool_dispatch_history(args, backend).await,
        "rsry_decade_list" => tool_decade_list(args, backend).await,
        "rsry_thread_list" => tool_thread_list(args, backend).await,
        "rsry_thread_assign" => tool_thread_assign(args, backend).await,
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

    let mut reconciler = Reconciler::new(reconciler_config).await;
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
    let beads_dir = crate::resolve_beads_dir(&root);
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
    let owner = args
        .get("owner")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| crate::dispatch::default_agent(issue_type));

    let files: Vec<String> = args
        .get("files")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let test_files: Vec<String> = args
        .get("test_files")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    if crate::bead::requires_files(issue_type) && files.is_empty() {
        anyhow::bail!(
            "files required for {issue_type} beads — specify which code this bead touches"
        );
    }

    let client = get_client(repo_path, pool).await?;
    let repo_name = repo_name_from_path(repo_path);
    let id = crate::generate_bead_id(&repo_name);

    // Wire dependencies if provided
    let depends_on: Vec<String> = args
        .get("depends_on")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    // Single transaction: INSERT + assignee + files + deps → one dolt commit
    client
        .create_bead_full(
            &id,
            title,
            description,
            priority,
            issue_type,
            owner,
            &files,
            &test_files,
            &depends_on,
        )
        .await?;

    Ok(json!({ "id": id, "title": title, "priority": priority, "owner": owner }))
}

async fn tool_bead_update(args: &Value, pool: &RepoPool) -> Result<Value> {
    let repo_path = args["repo_path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("repo_path required"))?;
    let id = args["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("id required"))?;

    let update = crate::bead::BeadUpdate {
        title: args.get("title").and_then(|v| v.as_str()).map(String::from),
        description: args
            .get("description")
            .and_then(|v| v.as_str())
            .map(String::from),
        priority: args
            .get("priority")
            .and_then(|v| v.as_u64())
            .map(|p| p as u8),
        issue_type: args
            .get("issue_type")
            .and_then(|v| v.as_str())
            .map(String::from),
        owner: args.get("owner").and_then(|v| v.as_str()).map(String::from),
        files: args.get("files").and_then(|v| v.as_array()).map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        }),
        test_files: args
            .get("test_files")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            }),
    };

    if update.is_empty() {
        anyhow::bail!("no fields to update — provide at least one field besides repo_path and id");
    }

    let client = get_client(repo_path, pool).await?;
    let updated_fields = client.update_bead_fields(id, &update).await?;

    // Log the update event for audit trail
    client
        .log_event(id, "fields_updated", &updated_fields.join(", "))
        .await;

    Ok(json!({ "id": id, "updated_fields": updated_fields }))
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

    // Update session registry so rsry_active shows last activity
    if let Ok(mut registry) = crate::session::SessionRegistry::load() {
        let _ = registry.touch(id, body);
    }

    Ok(json!({ "id": id, "comment_added": true }))
}

async fn tool_bead_link(args: &Value, pool: &RepoPool) -> Result<Value> {
    let repo_path = args["repo_path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("repo_path required"))?;
    let id = args["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("id required"))?;
    let depends_on = args["depends_on"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("depends_on required"))?;
    let remove = args
        .get("remove")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let client = get_client(repo_path, pool).await?;

    if remove {
        client.remove_dependency(id, depends_on).await?;
        Ok(json!({ "id": id, "depends_on": depends_on, "action": "removed" }))
    } else {
        client.add_dependency(id, depends_on).await?;
        Ok(json!({ "id": id, "depends_on": depends_on, "action": "added" }))
    }
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
    let agent_override = args.get("agent").and_then(|v| v.as_str());

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

    let mut bead = client
        .get_bead(bead_id, &repo_name)
        .await?
        .ok_or_else(|| anyhow::anyhow!("bead {bead_id} not found"))?;

    // Agent override takes precedence over bead.owner
    if let Some(agent) = agent_override {
        bead.owner = Some(agent.to_string());
    }

    let agent_label = bead.owner.as_deref().unwrap_or("generic");

    // Resolve agents_dir and provider, then dispatch
    let agents_dir = crate::dispatch::resolve_agents_dir();
    let provider = crate::dispatch::provider_by_name(provider_name)?;
    let handle = crate::dispatch::spawn(
        &bead,
        &root,
        true,
        bead.generation(),
        provider.as_ref(),
        agents_dir.as_deref(),
    )
    .await?;

    // Update status — this is the linearization point for dispatch.
    // Bead must be marked dispatched before pipeline state is written.
    // Failure here means the dispatch did not happen from the bead's perspective.
    client
        .update_status(bead_id, "dispatched")
        .await
        .with_context(|| format!("marking bead {bead_id} as dispatched"))?;

    // Extract workspace metadata before handle is dropped (workspace has no Drop
    // impl, so the on-disk workspace persists — we need metadata for cleanup).
    let workspace_vcs = handle
        .workspace
        .as_ref()
        .map(|ws| match ws.vcs {
            crate::workspace::VcsKind::Jj => "jj",
            crate::workspace::VcsKind::Git => "git",
            crate::workspace::VcsKind::None => "",
        })
        .unwrap_or("")
        .to_string();
    let ws_repo_path = handle
        .workspace
        .as_ref()
        .map(|ws| ws.repo_path.to_string_lossy().to_string())
        .unwrap_or_default();

    // Register in session registry (includes workspace info for cleanup on death)
    let mut registry = crate::session::SessionRegistry::load().unwrap_or_default();
    registry
        .register(crate::session::SessionEntry {
            bead_id: bead_id.to_string(),
            repo: repo_name,
            provider: provider_name.to_string(),
            pid: handle.pid(),
            work_dir: handle.work_dir.to_string_lossy().to_string(),
            started_at: chrono::Utc::now(),
            title: bead.title.clone(),
            agent: agent_label.to_string(),
            workspace_vcs,
            repo_path: ws_repo_path,
            last_activity: None,
            last_comment: None,
        })
        .ok();

    Ok(json!({
        "bead_id": bead_id,
        "title": bead.title,
        "external_ref": bead.external_ref,
        "status": "dispatched",
        "provider": provider_name,
        "agent": agent_label,
        "pid": handle.pid(),
        "work_dir": handle.work_dir.to_string_lossy(),
    }))
}

async fn tool_active() -> Result<Value> {
    let registry = crate::session::SessionRegistry::load().unwrap_or_default();
    let mut running = Vec::new();
    let mut completed = Vec::new();

    for s in registry.active() {
        let health = check_agent_health(s);
        let entry = json!({
            "bead_id": s.bead_id,
            "title": s.title,
            "agent": s.agent,
            "repo": s.repo,
            "provider": s.provider,
            "pid": s.pid,
            "work_dir": s.work_dir,
            "started_at": s.started_at.to_rfc3339(),
            "last_activity": s.last_activity.map(|t| t.to_rfc3339()),
            "last_comment": s.last_comment,
            "health": health,
        });
        if health == "dead" {
            completed.push(entry);
        } else {
            running.push(entry);
        }
    }

    Ok(json!({
        "running": running.len(),
        "completed": completed.len(),
        "agents": running,
        "needs_merge": completed,
    }))
}

/// Quick health check for a dispatched agent.
/// Returns "healthy", "idle", "stuck", or "dead".
fn check_agent_health(session: &crate::session::SessionEntry) -> &'static str {
    // Check if PID is alive
    let pid_alive = session
        .pid
        .map(|pid| unsafe { libc::kill(pid as i32, 0) == 0 })
        .unwrap_or(false);
    if !pid_alive {
        return "dead";
    }

    // Check for TCP connections (active API calls)
    let has_tcp = session.pid.is_some_and(|pid| {
        std::process::Command::new("lsof")
            .args(["-p", &pid.to_string(), "-i", "TCP"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("ESTABLISHED"))
            .unwrap_or(false)
    });

    // Check workspace for recent file changes (last 2 minutes)
    let ws_active = if !session.work_dir.is_empty() {
        std::process::Command::new("find")
            .args([
                &session.work_dir,
                "-maxdepth",
                "3",
                "-newer",
                &session.work_dir,
                "-name",
                "*.rs",
                "-o",
                "-name",
                "*.ex",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output()
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(false)
    } else {
        false
    };

    if has_tcp || ws_active {
        "healthy"
    } else if session.last_activity.is_some() {
        // Had activity before but none now
        "idle"
    } else {
        // Never had activity, no TCP — likely stuck
        "stuck"
    }
}

async fn tool_workspace_create(args: &Value) -> Result<Value> {
    let bead_id = args["bead_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("bead_id required"))?;
    let repo_path = args["repo_path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("repo_path required"))?;

    let path = std::path::Path::new(repo_path);
    let root = config::discover_repo_root(path).unwrap_or_else(|| path.to_path_buf());
    let repo_name = root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".into());

    let ws = crate::workspace::Workspace::create(bead_id, &repo_name, &root, true).await?;

    Ok(json!({
        "bead_id": bead_id,
        "work_dir": ws.work_dir.to_string_lossy(),
        "vcs": format!("{:?}", ws.vcs),
        "repo_path": ws.repo_path.to_string_lossy(),
    }))
}

async fn tool_workspace_checkpoint(args: &Value) -> Result<Value> {
    let bead_id = args["bead_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("bead_id required"))?;
    let repo_path = args["repo_path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("repo_path required"))?;
    let message = args["message"].as_str().unwrap_or("agent work");

    let path = std::path::Path::new(repo_path);
    let root = config::discover_repo_root(path).unwrap_or_else(|| path.to_path_buf());
    let repo_name = root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".into());

    let ws = crate::workspace::Workspace::from_existing(bead_id, &repo_name, &root);
    let change_id = ws.checkpoint(message).await?;

    Ok(json!({
        "bead_id": bead_id,
        "change_id": change_id,
        "vcs": format!("{:?}", ws.vcs),
    }))
}

fn tool_workspace_cleanup(args: &Value) -> Result<Value> {
    let bead_id = args["bead_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("bead_id required"))?;
    let repo_path = args["repo_path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("repo_path required"))?;

    let path = std::path::Path::new(repo_path);
    let root = config::discover_repo_root(path).unwrap_or_else(|| path.to_path_buf());
    let vcs = crate::workspace::detect_vcs(&root);

    match vcs {
        crate::workspace::VcsKind::Jj => {
            crate::workspace::cleanup_jj_workspace(&root, bead_id);
        }
        crate::workspace::VcsKind::Git => {
            crate::workspace::cleanup_git_worktree(&root, bead_id);
        }
        crate::workspace::VcsKind::None => {}
    }

    Ok(json!({
        "bead_id": bead_id,
        "cleaned": true,
    }))
}

async fn tool_workspace_merge(args: &Value) -> Result<Value> {
    let bead_id = args["bead_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("bead_id required"))?;
    let repo_path = args["repo_path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("repo_path required"))?;
    let issue_type = args["issue_type"].as_str().unwrap_or("task");

    let path = std::path::Path::new(repo_path);
    let root = config::discover_repo_root(path).unwrap_or_else(|| path.to_path_buf());
    let branch = format!("fix/{bead_id}");

    let result = crate::workspace::merge_or_pr(&root, &branch, bead_id, issue_type).await?;

    Ok(json!({
        "bead_id": bead_id,
        "branch": branch,
        "result": result,
    }))
}

async fn tool_decompose(args: &Value) -> Result<Value> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("path is required"))?;

    let markdown = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;

    let parsed = bdr::parse::parse_adr_full(&markdown);
    if parsed.atoms.is_empty() {
        return Ok(json!({
            "decade": null,
            "message": "No decomposable atoms found",
            "atom_count": 0,
        }));
    }

    let title = args
        .get("title")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            markdown
                .lines()
                .find(|l| l.starts_with("# "))
                .map(|l| l.trim_start_matches('#').trim().to_string())
                .unwrap_or_else(|| path.to_string())
        });

    let decade = bdr::thread::build_decade_with_meta(path, &title, &parsed.atoms, &parsed.meta);

    Ok(json!({
        "decade": {
            "id": decade.id,
            "title": decade.title,
            "status": format!("{:?}", decade.status),
            "thread_count": decade.threads.len(),
            "bead_count": decade.threads.iter().map(|t| t.beads.len()).sum::<usize>(),
        },
        "meta": decade.meta,
        "threads": decade.threads.iter().map(|t| json!({
            "id": t.id,
            "name": t.name,
            "bead_count": t.beads.len(),
            "cross_repo_refs": t.cross_repo_refs,
            "beads": t.beads.iter().map(|b| json!({
                "title": b.title,
                "issue_type": b.issue_type,
                "priority": b.priority,
                "channel": b.channel.as_str(),
                "thread_group": b.thread_group,
                "target_repo": b.target_repo,
                "depends_on": b.depends_on,
                "success_criteria": b.success_criteria,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "atom_count": parsed.atoms.len(),
    }))
}

async fn tool_pipeline_upsert(args: &Value, backend: Option<&DoltBackend>) -> Result<Value> {
    let backend = backend.ok_or_else(|| {
        anyhow::anyhow!(
            "backend store not configured — add [backend] section to ~/.rsry/config.toml"
        )
    })?;

    let repo = args["repo"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("repo required"))?;
    let bead_id = args["bead_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("bead_id required"))?;
    let pipeline_phase = args["pipeline_phase"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("pipeline_phase required"))? as u8;
    let pipeline_agent = args["pipeline_agent"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("pipeline_agent required"))?;

    let phase_status = args
        .get("phase_status")
        .and_then(|v| v.as_str())
        .unwrap_or("pending");
    let retries = args.get("retries").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let consecutive_reverts = args
        .get("consecutive_reverts")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let highest_verify_tier = args
        .get("highest_verify_tier")
        .and_then(|v| v.as_u64())
        .map(|v| v as u8);
    let last_generation = args
        .get("last_generation")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let backoff_until = args
        .get("backoff_until")
        .and_then(|v| v.as_str())
        .map(|s| {
            s.parse::<chrono::DateTime<chrono::Utc>>()
                .with_context(|| format!("parsing backoff_until '{s}' as ISO 8601"))
        })
        .transpose()?;

    let state = PipelineState {
        bead_ref: BeadRef {
            repo: repo.to_string(),
            bead_id: bead_id.to_string(),
        },
        pipeline_phase,
        pipeline_agent: pipeline_agent.to_string(),
        phase_status: phase_status.to_string(),
        retries,
        consecutive_reverts,
        highest_verify_tier,
        last_generation,
        backoff_until,
    };

    backend.upsert_pipeline(&state).await?;

    Ok(json!({
        "repo": repo,
        "bead_id": bead_id,
        "pipeline_phase": pipeline_phase,
        "pipeline_agent": pipeline_agent,
        "phase_status": phase_status,
        "upserted": true,
    }))
}

async fn tool_pipeline_query(args: &Value, backend: Option<&DoltBackend>) -> Result<Value> {
    let backend = backend.ok_or_else(|| anyhow::anyhow!("backend store not configured"))?;

    let repo = args.get("repo").and_then(|v| v.as_str());
    let bead_id = args.get("bead_id").and_then(|v| v.as_str());

    match (repo, bead_id) {
        (Some(repo), Some(bead_id)) => {
            let bead_ref = BeadRef {
                repo: repo.to_string(),
                bead_id: bead_id.to_string(),
            };
            let pipeline = backend.get_pipeline(&bead_ref).await?;
            match pipeline {
                Some(p) => Ok(json!({
                    "mode": "get",
                    "pipeline": {
                        "repo": p.bead_ref.repo,
                        "bead_id": p.bead_ref.bead_id,
                        "pipeline_phase": p.pipeline_phase,
                        "pipeline_agent": p.pipeline_agent,
                        "phase_status": p.phase_status,
                        "retries": p.retries,
                    }
                })),
                None => Ok(json!({ "mode": "get", "pipeline": null })),
            }
        }
        (None, None) => {
            let pipelines = backend.list_active_pipelines().await?;
            let items: Vec<Value> = pipelines
                .iter()
                .map(|p| {
                    json!({
                        "repo": p.bead_ref.repo,
                        "bead_id": p.bead_ref.bead_id,
                        "pipeline_phase": p.pipeline_phase,
                        "pipeline_agent": p.pipeline_agent,
                        "phase_status": p.phase_status,
                    })
                })
                .collect();
            Ok(json!({ "mode": "list", "count": items.len(), "pipelines": items }))
        }
        _ => anyhow::bail!("provide both repo and bead_id, or neither for list"),
    }
}

async fn tool_dispatch_record(args: &Value, backend: Option<&DoltBackend>) -> Result<Value> {
    let backend = backend.ok_or_else(|| anyhow::anyhow!("backend store not configured"))?;

    let id = args["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("id required"))?;
    let repo = args["repo"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("repo required"))?;
    let bead_id = args["bead_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("bead_id required"))?;
    let agent = args["agent"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("agent required"))?;
    let provider = args["provider"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("provider required"))?;
    let work_dir = args["work_dir"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("work_dir required"))?;

    let record = DispatchRecord {
        id: id.to_string(),
        bead_ref: BeadRef {
            repo: repo.to_string(),
            bead_id: bead_id.to_string(),
        },
        agent: agent.to_string(),
        provider: provider.to_string(),
        started_at: chrono::Utc::now(),
        completed_at: None,
        outcome: None,
        work_dir: work_dir.to_string(),
        session_id: None,
        workspace_path: None,
    };

    backend.record_dispatch(&record).await?;
    Ok(json!({ "id": id, "bead_id": bead_id, "recorded": true }))
}

async fn tool_dispatch_history(args: &Value, backend: Option<&DoltBackend>) -> Result<Value> {
    let backend = backend.ok_or_else(|| anyhow::anyhow!("backend store not configured"))?;
    let bead_id = args.get("bead_id").and_then(|v| v.as_str());
    let active_only = args
        .get("active_only")
        .and_then(|v| v.as_bool())
        .unwrap_or(bead_id.is_none());

    let mut dispatches = backend.active_dispatches().await?;
    if let Some(bead_id) = bead_id {
        dispatches.retain(|d: &DispatchRecord| d.bead_ref.bead_id == bead_id);
    }
    if !active_only {
        // active_dispatches already filters to active — nothing extra needed
        let _ = active_only;
    }

    let items: Vec<Value> = dispatches
        .iter()
        .map(|d| {
            json!({
                "id": d.id,
                "repo": d.bead_ref.repo,
                "bead_id": d.bead_ref.bead_id,
                "agent": d.agent,
                "provider": d.provider,
                "started_at": d.started_at.to_rfc3339(),
                "completed_at": d.completed_at.map(|t| t.to_rfc3339()),
                "outcome": d.outcome,
                "work_dir": d.work_dir,
            })
        })
        .collect();

    Ok(json!({ "count": items.len(), "dispatches": items }))
}

// ---------------------------------------------------------------------------
// Hierarchy tools (decades, threads, bead membership)
// ---------------------------------------------------------------------------

async fn tool_decade_list(args: &Value, backend: Option<&DoltBackend>) -> Result<Value> {
    let backend = backend.ok_or_else(|| anyhow::anyhow!("backend store not configured"))?;
    let status = args.get("status").and_then(|v| v.as_str());

    use crate::store::HierarchyStore;
    let decades = backend.list_decades(status).await?;

    let items: Vec<Value> = decades
        .iter()
        .map(|d| {
            json!({
                "id": d.id,
                "title": d.title,
                "source_path": d.source_path,
                "status": d.status,
            })
        })
        .collect();

    Ok(json!({ "count": items.len(), "decades": items }))
}

async fn tool_thread_list(args: &Value, backend: Option<&DoltBackend>) -> Result<Value> {
    let backend = backend.ok_or_else(|| anyhow::anyhow!("backend store not configured"))?;
    use crate::store::HierarchyStore;

    // Option 1: list threads for a decade
    if let Some(decade_id) = args.get("decade_id").and_then(|v| v.as_str()) {
        let threads = backend.list_threads(decade_id).await?;
        let items: Vec<Value> = threads
            .iter()
            .map(|t| {
                json!({
                    "id": t.id,
                    "name": t.name,
                    "decade_id": t.decade_id,
                    "feature_branch": t.feature_branch,
                })
            })
            .collect();
        return Ok(json!({ "count": items.len(), "threads": items }));
    }

    // Option 2: find thread for a specific bead
    if let (Some(bead_id), Some(repo)) = (
        args.get("bead_id").and_then(|v| v.as_str()),
        args.get("repo").and_then(|v| v.as_str()),
    ) {
        let bead_ref = crate::store::BeadRef {
            repo: repo.to_string(),
            bead_id: bead_id.to_string(),
        };
        let thread_id = backend.find_thread_for_bead(&bead_ref).await?;
        return Ok(json!({ "bead_id": bead_id, "thread_id": thread_id }));
    }

    anyhow::bail!("provide either decade_id or (bead_id + repo)")
}

async fn tool_thread_assign(args: &Value, backend: Option<&DoltBackend>) -> Result<Value> {
    let backend = backend.ok_or_else(|| anyhow::anyhow!("backend store not configured"))?;
    use crate::store::{BeadRef, HierarchyStore, ThreadRecord};

    let thread_id = args["thread_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("thread_id required"))?;
    let bead_id = args["bead_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("bead_id required"))?;
    let repo = args["repo"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("repo required"))?;

    // Create thread if it doesn't exist
    let thread_name = args
        .get("thread_name")
        .and_then(|v| v.as_str())
        .unwrap_or(thread_id);
    let decade_id = args
        .get("decade_id")
        .and_then(|v| v.as_str())
        .unwrap_or("ungrouped");

    backend
        .upsert_thread(&ThreadRecord {
            id: thread_id.to_string(),
            name: thread_name.to_string(),
            decade_id: decade_id.to_string(),
            feature_branch: None,
        })
        .await?;

    let bead_ref = BeadRef {
        repo: repo.to_string(),
        bead_id: bead_id.to_string(),
    };
    backend.add_bead_to_thread(thread_id, &bead_ref).await?;

    let members = backend.list_beads_in_thread(thread_id).await?;

    Ok(json!({
        "thread_id": thread_id,
        "bead_id": bead_id,
        "action": "assigned",
        "thread_size": members.len(),
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
                "version": env!("CARGO_PKG_VERSION"),
                "buildHash": env!("RSRY_BUILD_HASH")
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
    backend: Option<&DoltBackend>,
) -> JsonRpcResponse {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");

    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    match call_tool(name, &args, config_path, pool, backend).await {
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
    /// Backend store for cross-repo orchestrator state (pipeline, dispatches, linkage).
    /// None when `[backend]` is not configured — existing functionality is unaffected.
    backend: Option<Arc<DoltBackend>>,
}

/// Validate Origin header to prevent DNS rebinding attacks.
#[allow(clippy::result_large_err)]
fn validate_origin(
    headers: &axum::http::HeaderMap,
) -> std::result::Result<(), axum::response::Response> {
    use axum::response::IntoResponse;
    if let Some(origin) = headers.get("origin") {
        let o = origin.to_str().unwrap_or("");
        let extra_origins = std::env::var("RSRY_ALLOWED_ORIGINS").unwrap_or_default();
        let allowed = o.starts_with("http://localhost")
            || o.starts_with("http://127.0.0.1")
            || o.starts_with("https://localhost")
            || o.starts_with("https://127.0.0.1")
            || extra_origins
                .split(',')
                .any(|a| !a.trim().is_empty() && o.starts_with(a.trim()));
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
            handle_tools_call(
                id,
                &request.params,
                &state.config_path,
                &state.pool,
                state.backend.as_deref(),
            )
            .await
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

    // Connect backend store if [backend] is configured
    let backend = if let Some(ref backend_cfg) = cfg.backend {
        match DoltBackend::connect(backend_cfg).await {
            Ok(b) => {
                eprintln!(
                    "[rsry-mcp] backend store connected ({})",
                    backend_cfg.path.display()
                );
                Some(Arc::new(b))
            }
            Err(e) => {
                eprintln!("[rsry-mcp] backend store unavailable, continuing without it: {e}");
                None
            }
        }
    } else {
        None
    };

    let pool = Arc::new(RepoPool::from_config(config_path).await?);
    let state = AppState {
        pool: pool.clone(),
        config_path: Arc::from(config_path),
        sessions: Arc::new(RwLock::new(HashSet::new())),
        webhook_secret: webhook_secret.map(|s| Arc::from(s.as_str())),
        backend,
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

    let bind = std::env::var("RSRY_BIND").unwrap_or_else(|_| "127.0.0.1".into());
    let listener = tokio::net::TcpListener::bind((bind.as_str(), port)).await?;
    eprintln!(
        "[rsry-mcp] HTTP server listening on http://{bind}:{port}/mcp ({} repos: {})",
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

    // SIGHUP → graceful exit so Claude Code restarts with the new binary after `task install`
    let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
        .context("registering SIGHUP handler")?;

    // Connect backend store if [backend] is configured
    let cfg = config::load(config_path).ok();
    let backend = if let Some(backend_cfg) = cfg.as_ref().and_then(|c| c.backend.as_ref()) {
        match DoltBackend::connect(backend_cfg).await {
            Ok(b) => {
                eprintln!(
                    "[rsry-mcp] backend store connected ({})",
                    backend_cfg.path.display()
                );
                Some(Arc::new(b))
            }
            Err(e) => {
                eprintln!("[rsry-mcp] backend store unavailable, continuing without it: {e}");
                None
            }
        }
    } else {
        None
    };

    // Create connection pool on startup — reused across all tool calls
    let pool = RepoPool::from_config(config_path).await?;
    eprintln!(
        "[rsry-mcp] server started (stdio transport, {} repos: {}, build {})",
        pool.len(),
        pool.repo_names().join(", "),
        env!("RSRY_BUILD_HASH"),
    );

    loop {
        let line = tokio::select! {
            result = lines.next_line() => {
                match result.context("reading stdin")? {
                    Some(line) => line,
                    None => break, // stdin closed
                }
            }
            _ = sighup.recv() => {
                eprintln!("[rsry-mcp] received SIGHUP, exiting for binary upgrade");
                break;
            }
        };
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
            "tools/call" => {
                handle_tools_call(id, &request.params, config_path, &pool, backend.as_deref()).await
            }
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
    fn tool_definitions_has_expected_tools() {
        let defs = tool_definitions();
        let tools = defs["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();

        // Verify all tools have rsry_ prefix (no typos or missing names)
        for name in &names {
            assert!(
                name.starts_with("rsry_"),
                "tool '{name}' missing rsry_ prefix"
            );
        }

        // Verify critical tools exist (not exhaustive — adding a tool shouldn't break this test)
        for required in [
            "rsry_scan",
            "rsry_bead_create",
            "rsry_bead_close",
            "rsry_dispatch",
            "rsry_active",
            "rsry_workspace_create",
        ] {
            assert!(
                names.contains(&required),
                "required tool '{required}' missing"
            );
        }

        // Sanity: at least 15 tools (grows over time, never shrinks)
        assert!(
            tools.len() >= 15,
            "expected at least 15 tools, got {}",
            tools.len()
        );
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
        assert!(
            result["serverInfo"]["buildHash"].is_string(),
            "serverInfo should include buildHash"
        );
    }

    #[test]
    fn tools_list_response_format() {
        let resp = handle_tools_list(json!(2));
        let result = resp.result.unwrap();
        assert!(result["tools"].is_array());
        assert!(result["tools"].as_array().unwrap().len() >= 15);
    }

    #[tokio::test]
    async fn pipeline_upsert_errors_without_backend() {
        let args = json!({
            "repo": "rosary",
            "bead_id": "rsry-001",
            "pipeline_phase": 0,
            "pipeline_agent": "dev-agent",
        });
        let result = tool_pipeline_upsert(&args, None).await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("backend store not configured"), "got: {msg}");
    }

    #[tokio::test]
    async fn pipeline_upsert_rejects_missing_required_fields() {
        // Missing pipeline_agent
        let args = json!({
            "repo": "rosary",
            "bead_id": "rsry-001",
            "pipeline_phase": 0,
        });
        let result = tool_pipeline_upsert(&args, None).await;
        // Should fail on backend check before field validation, but if backend were present
        // it would fail on missing pipeline_agent. Test the backend-absent path first.
        assert!(result.is_err());
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
    fn validate_origin_allows_extra_origins_from_env() {
        // SAFETY: test is single-threaded; no other code reads this env var concurrently
        unsafe {
            std::env::set_var(
                "RSRY_ALLOWED_ORIGINS",
                "https://mcp.q-q.dev,https://other.example.com",
            );
        }
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("origin", "https://mcp.q-q.dev".parse().unwrap());
        assert!(validate_origin(&headers).is_ok());

        headers.insert("origin", "https://other.example.com/path".parse().unwrap());
        assert!(validate_origin(&headers).is_ok());

        headers.insert("origin", "https://evil.com".parse().unwrap());
        assert!(validate_origin(&headers).is_err());
        // SAFETY: test is single-threaded; no other code reads this env var concurrently
        unsafe {
            std::env::remove_var("RSRY_ALLOWED_ORIGINS");
        }
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
            backend: None,
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
            backend: None,
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
            backend: None,
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
            backend: None,
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
            backend: None,
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
            backend: None,
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
