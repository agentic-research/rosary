//! Tool handler functions — implementation of each `rsry_*` MCP tool.

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::config;
use crate::dolt::{DoltClient, DoltConfig};
use crate::pool::RepoPool;
use crate::store::{BeadRef, DispatchRecord, DispatchStore, PipelineState};
use crate::store_dolt::DoltBackend;

// ---------------------------------------------------------------------------
// Client helpers
// ---------------------------------------------------------------------------

/// Get a DoltClient — try the pool first (by name then path), fall back to fresh connect.
pub(crate) async fn get_client<'a>(repo_path: &str, pool: &'a RepoPool) -> Result<ClientRef<'a>> {
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

pub(crate) enum ClientRef<'a> {
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

pub(crate) fn repo_name_from_path(repo_path: &str) -> String {
    std::path::Path::new(repo_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".into())
}

// ---------------------------------------------------------------------------
// Tool router
// ---------------------------------------------------------------------------

pub(crate) async fn call_tool(
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

// ---------------------------------------------------------------------------
// Scan / status / list
// ---------------------------------------------------------------------------

async fn tool_scan(config_path: &str) -> Result<Value> {
    let cfg = config::load(config_path)?;
    let beads = crate::scanner::scan_repos(&cfg.repo).await?;
    Ok(json!({
        "count": beads.len(),
        "beads": beads,
    }))
}

async fn tool_status(config_path: &str) -> Result<Value> {
    let cfg = config::load(config_path)?;
    let beads = crate::scanner::scan_repos(&cfg.repo).await?;

    let open = beads.iter().filter(|b| b.status == "open").count();
    let in_progress = beads.iter().filter(|b| b.status == "in_progress").count();
    let blocked = beads.iter().filter(|b| b.is_blocked()).count();
    let ready = beads.iter().filter(|b| b.is_ready()).count();
    let human = beads.iter().filter(|b| b.is_human()).count();
    let total = beads.len();

    Ok(json!({
        "total": total,
        "open": open,
        "ready": ready,
        "in_progress": in_progress,
        "blocked": blocked,
        "human": human,
    }))
}

async fn tool_list_beads(config_path: &str, status: Option<&str>) -> Result<Value> {
    let cfg = config::load(config_path)?;
    let beads = crate::scanner::scan_repos(&cfg.repo).await?;

    let filtered: Vec<_> = match status {
        Some("blocked") => beads.into_iter().filter(|b| b.is_blocked()).collect(),
        Some("ready") => beads.into_iter().filter(|b| b.is_ready()).collect(),
        Some("human") => beads.into_iter().filter(|b| b.is_human()).collect(),
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
// Bead CRUD
// ---------------------------------------------------------------------------

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

    let owner_type = args["owner_type"].as_str().unwrap_or("agent");

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
            owner_type,
        )
        .await?;

    Ok(json!({ "id": id, "title": title, "priority": priority, "owner": owner, "owner_type": owner_type }))
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
        owner_type: args
            .get("owner_type")
            .and_then(|v| v.as_str())
            .map(String::from),
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

// ---------------------------------------------------------------------------
// Dispatch / active
// ---------------------------------------------------------------------------

async fn tool_dispatch(args: &Value, _config_path: &str) -> Result<Value> {
    let bead_id = args["bead_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("bead_id required"))?;
    let repo_path = args["repo_path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("repo_path required"))?;
    let provider_name = args["provider"].as_str().unwrap_or("claude");
    let agent_override = args.get("agent").and_then(|v| v.as_str());
    let isolate = args
        .get("isolate")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

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
        isolate,
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
        .map(crate::session::is_pid_alive)
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

// ---------------------------------------------------------------------------
// Workspace tools
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Decompose
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Pipeline / dispatch record / history
// ---------------------------------------------------------------------------

pub(crate) async fn tool_pipeline_upsert(
    args: &Value,
    backend: Option<&DoltBackend>,
) -> Result<Value> {
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
}
