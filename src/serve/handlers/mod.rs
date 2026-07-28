//! Tool handler functions — implementation of each `rsry_*` MCP tool.

use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::collections::BTreeMap;

use crate::config;
use crate::dispatch::AgentSessionRef;
use crate::pool::RepoPool;
use crate::store::{
    AgentRunEvent, BackendStore, BeadStore, CrossRepoDep, DispatchRecord, EvidenceTier,
    PipelineState, WorkRef,
};

/// Default result limit for bead search (keeps MCP responses bounded).
const SEARCH_DEFAULT_LIMIT: u64 = 20;
/// Hard ceiling — even if the caller asks for more.
const SEARCH_MAX_LIMIT: u64 = 50;
/// Truncate bead descriptions in search results to this many bytes.
const SEARCH_DESC_TRUNCATE: usize = 200;
/// Maximum bytes for a bead title.
const TITLE_MAX_LEN: usize = 512;
/// Maximum bytes for a bead description or comment body.
// Single source of the limit lives in bead_ops (shared with the CLI path).
const BODY_MAX_LEN: usize = crate::bead_ops::BODY_MAX_LEN;
/// Maximum valid priority value (P0–P3).
const PRIORITY_MAX: u64 = 3;
// Canonical issue type list lives in crate::bead::VALID_ISSUE_TYPES.

// ---------------------------------------------------------------------------
// Argument parsing helpers
// ---------------------------------------------------------------------------

/// Parse a boolean arg from MCP JSON, with an explicit default.
/// Returns `default` if the key is missing, null, or not a bool.
fn parse_bool_arg(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
}

// ---------------------------------------------------------------------------
// Client helpers
// ---------------------------------------------------------------------------
/// Get a BeadStore — try the pool first (by name then path), fall back to fresh connect.
pub(crate) async fn get_client<'a>(repo_path: &str, pool: &'a RepoPool) -> Result<StoreRef<'a>> {
    let name = repo_name_from_path(repo_path);
    if let Some(store) = pool.get(&name) {
        return Ok(StoreRef::Pooled(store));
    }
    if let Some((_name, store)) = pool.get_by_path(repo_path) {
        return Ok(StoreRef::Pooled(store));
    }
    let root = crate::scanner::resolve_repo_path(std::path::Path::new(repo_path));
    let beads_dir = crate::resolve_beads_dir(&root);
    // The repo isn't in the pool, so we open its store ad hoc from the path.
    // If that fails (path missing, or read-only from this workspace — the
    // `repo:agents` friction #5), surface a DETERMINISTIC, actionable error
    // instead of leaking a raw filesystem errno up through MCP.
    let store = crate::bead_sqlite::connect_bead_store(&beads_dir)
        .await
        .with_context(|| {
            format!(
                "repo `{name}` is not registered in the pool and its bead store at {} could not \
                 be opened (the path may be missing, or read-only from this workspace). Register \
                 the repo with rsry_repo_register, or pass a repo_path whose `.beads/` is writable \
                 from here.",
                beads_dir.display()
            )
        })?;
    Ok(StoreRef::Owned(store))
}

pub(crate) enum StoreRef<'a> {
    Pooled(&'a dyn BeadStore),
    Owned(Box<dyn BeadStore>),
}

impl StoreRef<'_> {
    pub(crate) fn as_store(&self) -> &dyn BeadStore {
        match self {
            StoreRef::Pooled(s) => *s,
            StoreRef::Owned(s) => s.as_ref(),
        }
    }
}

pub(crate) fn repo_name_from_path(repo_path: &str) -> String {
    std::path::Path::new(repo_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".into())
}

/// Resolve the calling MCP args into a `(ScopeId, StoreRef)` pair for
/// any handler that needs a per-repo `BeadStore`. Centralizes the
/// scope-or-repo_path parsing pattern that bead_link uses (PR #212) so
/// converting the other 13 repo_path-taking handlers becomes a
/// one-liner — they all share this entry point.
///
/// Behavior:
/// - Args parse via [`resolve_scope`](crate::serve::scope_args::resolve_scope)
///   (`scope` takes precedence over `repo_path`; bare repo names parse
///   as `ScopeId::Repo`).
/// - For `ScopeId::Repo(name)`: try `pool.get(&name)` first, then fall
///   back to [`get_client`] with `repo_path` (which itself tries
///   `pool.get_by_path` then a fresh SQLite connection). This means a
///   caller can pass `scope: "repo:rosary"` without `repo_path` so
///   long as the pool already has the repo loaded.
/// - For `ScopeId::External(_)` or `ScopeId::Global`: errors. These
///   scopes are addressing-only — no per-repo Dolt store exists for
///   them. Personal-scope storage (the future `rosary-792ed6`
///   substrate) gets its own resolver when it lands.
///
/// rosary-b5da2f PR 5/N (the scope-abstraction series).
pub(crate) async fn resolve_repo_client<'a>(
    args: &Value,
    pool: &'a RepoPool,
) -> Result<(crate::scope::ScopeId, StoreRef<'a>)> {
    use crate::serve::scope_args::resolve_scope;
    let scope = resolve_scope(args)?;
    let repo_name = scope.as_repo_name().ok_or_else(|| {
        anyhow::anyhow!(
            "{scope} scope has no per-repo bead store; this operation is Repo-only. \
             External / Global addressing is supported for LinkageStore edges only \
             (see rsry_bead_link); Personal substrate is tracked under rosary-792ed6."
        )
    })?;
    // Guard against silent mis-attribution when caller passes BOTH
    // `scope` and `repo_path` but they name different repos. Without
    // this check, the resolver would write to repo A's store while
    // labeling output with scope B (Copilot #213 finding).
    if let Some(repo_path_arg) = args.get("repo_path").and_then(|v| v.as_str()) {
        let path_basename = repo_name_from_path(repo_path_arg);
        if path_basename != repo_name {
            anyhow::bail!(
                "scope `repo:{repo_name}` and repo_path basename `{path_basename}` disagree \
                 (scope-and-path mismatch); pick one — pass `scope` alone (and register the \
                 repo in the pool via rsry_repo_register) or pass `repo_path` alone (scope is \
                 inferred from the path)"
            );
        }
    }
    // Try the pool by name first (in case it's already connected).
    if let Some(store) = pool.get(repo_name) {
        return Ok((scope, StoreRef::Pooled(store)));
    }
    // Resolve the repo's path: an explicit `repo_path` arg, else the recorded
    // path for a registered repo (the scope-only, no-`repo_path` case). Then
    // open just THAT repo's store lazily — never all of them (rosary-31193d).
    let repo_path: String = match args.get("repo_path").and_then(|v| v.as_str()) {
        Some(p) => p.to_string(),
        None => pool
            .path_for(repo_name)
            .map(|p| p.to_string_lossy().to_string())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "scope `{repo_name}` is not a registered repo and no `repo_path` was passed; \
                     register the repo via `rsry_repo_register` or pass `repo_path` explicitly"
                )
            })?,
    };
    let store_ref = get_client(&repo_path, pool).await?;
    Ok((scope, store_ref))
}

// ---------------------------------------------------------------------------
// Tool router
// ---------------------------------------------------------------------------
pub(crate) async fn call_tool(
    name: &str,
    args: &Value,
    config_path: &str,
    pool: &RepoPool,
    backend: Option<&dyn BackendStore>,
    caller: &super::CallerIdentity,
    repo_cache: &crate::repo_cache::RepoCache,
) -> Result<Value> {
    let user_scope = caller.user_scope();

    // Audit log: record every MCP call with caller identity
    if let Some(uid) = user_scope {
        eprintln!("[mcp] {name} (user={uid})");
    }

    match name {
        "rsry_scan" => tool_scan(config_path).await,
        "rsry_expand_ref" => tool_expand_ref(args).await,
        "rsry_status" => tool_status(config_path).await,
        "rsry_list_beads" => {
            let status = args
                .get("status")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let repo = args
                .get("repo")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(50)
                .min(200) as usize;
            let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            tool_list_beads(
                config_path,
                status.as_deref(),
                repo.as_deref(),
                limit,
                offset,
                user_scope,
            )
            .await
        }
        "rsry_run_once" => {
            let dry_run = parse_bool_arg(args, "dry_run", false);
            let bead_id = args.get("bead_id").and_then(|v| v.as_str());
            tool_run_once(config_path, dry_run, bead_id, user_scope).await
        }
        "rsry_bead_create" => tool_bead_create(args, pool, user_scope).await,
        "rsry_bead_update" => tool_bead_update(args, pool, user_scope).await,
        "rsry_bead_close" => tool_bead_close(args, pool, user_scope).await,
        "rsry_bead_correct" => tool_bead_correct(args, pool, user_scope).await,
        "rsry_bead_comment" => tool_bead_comment(args, pool, user_scope).await,
        "rsry_bead_comment_list" => tool_bead_comment_list(args, pool, user_scope).await,
        "rsry_bead_comment_update" => tool_bead_comment_update(args, pool, user_scope).await,
        "rsry_bead_comment_delete" => tool_bead_comment_delete(args, pool, user_scope).await,
        "rsry_bead_link" => tool_bead_link(args, pool, backend).await,
        "rsry_bead_search" => tool_bead_search(args, pool, user_scope).await,
        "rsry_bead_history" => tool_bead_history(args, pool).await,
        "rsry_dispatch" => tool_dispatch(args, config_path).await,
        "rsry_active" => tool_active(backend).await,
        "rsry_workspace_create" => tool_workspace_create(args, config_path, repo_cache).await,
        "rsry_workspace_checkpoint" => tool_workspace_checkpoint(args).await,
        "rsry_workspace_cleanup" => tool_workspace_cleanup(args),
        "rsry_workspace_merge" => tool_workspace_merge(args).await,
        "rsry_decompose" => tool_decompose(args).await,
        "rsry_pipeline_upsert" => tool_pipeline_upsert(args, backend).await,
        "rsry_pipeline_query" => tool_pipeline_query(args, backend).await,
        "rsry_dispatch_record" => tool_dispatch_record(args, backend).await,
        "rsry_dispatch_history" => tool_dispatch_history(args, backend).await,
        "rsry_agent_run_event_record" => tool_agent_run_event_record(args, backend).await,
        "rsry_agent_run_events" => tool_agent_run_events(args, backend).await,
        "rsry_agent_session_addresses" => tool_agent_session_addresses(args, backend).await,
        "rsry_agent_session_message_record" => {
            tool_agent_session_message_record(args, backend).await
        }
        "rsry_decade_list" => tool_decade_list(args, backend).await,
        "rsry_decade_create" => tool_decade_create(args, backend).await,
        "rsry_thread_list" => tool_thread_list(args, backend).await,
        "rsry_thread_create" => tool_thread_create(args, backend).await,
        "rsry_thread_assign" => tool_thread_assign(args, backend).await,
        "rsry_thread_reparent" => tool_thread_reparent(args, backend).await,
        "rsry_repo_register" => tool_repo_register(args, backend, user_scope).await,
        "rsry_repo_list" => tool_repo_list(backend, user_scope).await,
        "rsry_bead_import" => tool_bead_import(args, config_path, pool, user_scope).await,
        "rsry_review" => tool_review(args, backend).await,
        "rsry_ticket_load" => tool_ticket_load(args, pool).await,
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

/// Fetch a demoted context blob by its hex content hash (warm-resume,
/// rosary-dd5828). `content` is null on a clean miss; verify-on-read in the
/// blob store turns a tampered blob into an error. `cas_dir` is a test-only
/// override; production reads `~/.rsry/cas`.
async fn tool_expand_ref(args: &Value) -> Result<Value> {
    let hash = args
        .get("hash")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("expand_ref requires `hash`"))?;
    let cas_dir = match args.get("cas_dir").and_then(|v| v.as_str()) {
        Some(d) => std::path::PathBuf::from(d),
        None => crate::vcs::state_dir()?.join("cas"),
    };
    let rs = crate::context::ref_store::RefStore::new(leyline_core::FsBlobStore::open(&cas_dir)?);
    let body = rs.expand(hash)?;
    Ok(json!({
        "content": body.map(|b| String::from_utf8_lossy(&b).to_string()),
    }))
}

async fn tool_status(config_path: &str) -> Result<Value> {
    let cfg = config::load(config_path)?;

    // Self-heal stale dispatch state in every repo before counting
    // (rosary-67c43d). The statusline polls this — running the sweep
    // here means a stuck `in_progress` count fixes itself within one
    // statusline refresh, no manual intervention required.
    for repo in &cfg.repo {
        let root = crate::scanner::resolve_repo_path(&repo.path);
        let beads_dir = crate::resolve_beads_dir(&root);
        if let Ok(client) = crate::bead_sqlite::connect_bead_store(&beads_dir).await {
            let repo_name = repo
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown");
            let _ =
                crate::dispatch::sweep::sweep_orphan_dispatches(client.as_ref(), &root, repo_name)
                    .await;
        }
    }

    // scan_repos_all (terminal beads included) + the shared rollup, so
    // `rsry_status` emits the SAME JSON as `rsry status --json` — `done` and
    // per-repo included (previously `status_counts` omitted both and the scan
    // was open-only). Single source: crate::status::status_json (ADR-0021/0006).
    let beads = crate::scanner::scan_repos_all(&cfg.repo).await?;
    Ok(crate::status::status_json(&beads))
}

async fn tool_list_beads(
    config_path: &str,
    status: Option<&str>,
    repo: Option<&str>,
    limit: usize,
    offset: usize,
    _user_scope: Option<&str>,
) -> Result<Value> {
    let cfg = config::load(config_path)?;
    let beads = crate::scanner::scan_repos(&cfg.repo).await?;

    let filtered: Vec<_> = beads
        .into_iter()
        .filter(|b| match repo {
            Some(r) => b.repo == r,
            None => true,
        })
        .filter(|b| bead_matches_status(b, status))
        .collect();

    let total = filtered.len();
    let page: Vec<_> = filtered.into_iter().skip(offset).take(limit).collect();

    Ok(json!({
        "total": total,
        "count": page.len(),
        "offset": offset,
        "limit": limit,
        "beads": page,
    }))
}

fn bead_matches_status(bead: &crate::bead::Bead, status: Option<&str>) -> bool {
    match status {
        Some("blocked") => bead.is_blocked(),
        Some("ready") => bead.is_ready(),
        Some("dispatchable") => bead.is_dispatchable(),
        Some(s) => bead.status == s,
        None => true,
    }
}

async fn tool_run_once(
    config_path: &str,
    dry_run: bool,
    bead_id: Option<&str>,
    user_id: Option<&str>,
) -> Result<Value> {
    use crate::reconcile::{Reconciler, ReconcilerConfig};
    use std::time::Duration;

    let cfg = config::load(config_path)?;

    let reconciler_config = ReconcilerConfig {
        max_concurrent: 1,
        scan_interval: Duration::from_secs(5),
        repo: cfg.repo,
        once: true,
        dry_run,
        compute: cfg.compute,
        backend: cfg.backend,
        target_bead: bead_id.map(|s| s.to_string()),
        pipelines: cfg.pipelines,
        max_pipeline_depth: cfg.max_pipeline_depth,
        user_id: user_id.map(|s| s.to_string()),
        default_branch: cfg
            .github
            .as_ref()
            .map(|g| g.base.clone())
            .unwrap_or_else(|| "main".to_string()),
        ..Default::default()
    };

    if let Some(target) = bead_id {
        if dry_run {
            // Dry run: single synchronous pass — no background task needed.
            // Avoids infinite loop (dry-run increments dispatched but never
            // reaches terminal state, so run() loops forever).
            let mut reconciler = Reconciler::new(reconciler_config).await;
            let summary = reconciler.iterate().await?;
            return Ok(json!({
                "targeted_bead": target,
                "pipeline": true,
                "status": "dry_run",
                "dispatched": summary.dispatched,
                "triaged": summary.triaged,
                "dry_run": true,
            }));
        }

        // Async hand-off: spawn the full pipeline in the background and return
        // immediately. The MCP HTTP client has a ~60s timeout — the pipeline
        // takes minutes. Use the backend-backed active/pipeline views to poll.
        let target_id = target.to_string();
        tokio::spawn(async move {
            let mut reconciler = Reconciler::new(reconciler_config).await;
            match reconciler.run().await {
                Ok(summary) => {
                    eprintln!(
                        "[run_once] pipeline for {target_id} finished: dispatched={} passed={} failed={} deadlettered={}",
                        summary.dispatched, summary.passed, summary.failed, summary.deadlettered
                    );
                }
                Err(e) => {
                    eprintln!("[run_once] pipeline for {target_id} failed: {e}");
                }
            }
        });

        Ok(json!({
            "targeted_bead": target,
            "pipeline": true,
            "status": "started",
            "message": "Pipeline running in background. Use rsry_active for the merged active view, or rsry_pipeline_query/rsry_dispatch_history for per-bead details.",
        }))
    } else {
        // Single pass (no bead_id): fast enough to stay synchronous.
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
}

// ---------------------------------------------------------------------------
// Bead CRUD
// ---------------------------------------------------------------------------
async fn tool_bead_create(
    args: &Value,
    pool: &RepoPool,
    user_scope: Option<&str>,
) -> Result<Value> {
    // rosary-b5da2f PR 6: accept `scope` or `repo_path` via the shared
    // resolver. Validation of other args runs BEFORE resolve_repo_client
    // so test fixtures + real callers get the expected error class
    // (e.g. "title must not be blank") rather than an FS error from a
    // nonexistent repo_path that wouldn't have mattered.
    let title = args["title"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("title required"))?;
    if title.trim().is_empty() {
        anyhow::bail!("title must not be blank");
    }
    if title.len() > TITLE_MAX_LEN {
        anyhow::bail!("title exceeds {TITLE_MAX_LEN} bytes (got {})", title.len());
    }
    // description: optional, but must be a string if present (not a number/array)
    let description = match args.get("description") {
        None | Some(Value::Null) => "",
        Some(v) => v
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("description must be a string, got {:?}", v))?,
    };
    if description.len() > BODY_MAX_LEN {
        anyhow::bail!(
            "description exceeds {BODY_MAX_LEN} bytes (got {})",
            description.len()
        );
    }
    // priority: optional, but must be a non-negative integer if present
    let priority_raw = match args.get("priority") {
        None | Some(Value::Null) => 2u64,
        Some(v) => v.as_u64().ok_or_else(|| {
            anyhow::anyhow!("priority must be an integer 0–{PRIORITY_MAX}, got {:?}", v)
        })?,
    };
    if priority_raw > PRIORITY_MAX {
        anyhow::bail!(
            "priority must be 0–{PRIORITY_MAX} (P0=critical … P3=low), got {priority_raw}"
        );
    }
    let priority = priority_raw as u8;
    // work_mode: optional secondary intent axis. It is not persisted yet; it
    // only selects a canonical issue_type default when issue_type is omitted.
    let work_mode = match args.get("work_mode") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_str()
                .ok_or_else(|| anyhow::anyhow!("work_mode must be a string, got {:?}", v))?,
        ),
    };
    if let Some(mode) = work_mode
        && !crate::bead::is_valid_work_mode(mode)
    {
        anyhow::bail!(
            "unknown work_mode {:?} — valid values: {}",
            mode,
            crate::bead::VALID_WORK_MODES.join(", ")
        );
    }
    // issue_type: optional, but must be one of the known values if present
    let issue_type = match args.get("issue_type") {
        None | Some(Value::Null) => work_mode
            .and_then(crate::bead::default_issue_type_for_work_mode)
            .unwrap_or("task"),
        Some(v) => v
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("issue_type must be a string, got {:?}", v))?,
    };
    if !crate::bead::VALID_ISSUE_TYPES.contains(&issue_type) {
        anyhow::bail!(
            "unknown issue_type {:?} — valid values: {}",
            issue_type,
            crate::bead::VALID_ISSUE_TYPES.join(", ")
        );
    }
    let owner = args
        .get("owner")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let str_array = |key: &str| -> Vec<String> {
        args.get(key)
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    };
    let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
    let acceptance_criteria = args
        .get("acceptance_criteria")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    // ACI adapter: parse the JSON surface into the shared op core (bead_ops).
    let create_args = crate::bead_ops::BeadCreateArgs {
        title: title.to_string(),
        description: description.to_string(),
        priority,
        issue_type: issue_type.to_string(),
        owner,
        files: str_array("files"),
        test_files: str_array("test_files"),
        depends_on: str_array("depends_on"),
        acceptance_criteria,
        force,
        role: crate::bead_ops::parse_role(
            args.get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("canonical"),
        )?,
    };
    // Validate BEFORE resolve_repo_client so arg errors surface as arg errors
    // (not an FS/scope error) regardless of repo validity — the error-class
    // contract the input-validation tests rely on. `create_bead` re-validates,
    // so the gate stays enforced-by-construction on the create path itself.
    create_args.validate()?;

    let (scope, client_ref) = resolve_repo_client(args, pool).await?;
    let client = client_ref.as_store();
    let repo_name = scope
        .as_repo_name()
        .expect("Repo-only scope verified by resolve_repo_client");
    // rosary-3fcd02: route prefix selection through the resolver (the single
    // chokepoint). explicit=None until RepoConfig.bead_prefix is plumbed to
    // this MCP path (needs a config-load decision); git_remote also pending.
    // Behaviour today == sanitized repo name.
    let id = crate::generate_bead_id(&crate::resolve_bead_prefix(
        None, repo_name, None, repo_name,
    ));

    // Capture git username from the repo's git config for creator
    // attribution — best-effort, only when `repo_path` is passed
    // explicitly. Pure scope-only callers get `created_by = None`
    // until repo-name → path resolution lands in a follow-up.
    let created_by = args
        .get("repo_path")
        .and_then(|v| v.as_str())
        .and_then(|p| crate::git_config_user_name(std::path::Path::new(p)));

    // ADR-0022 routing needs a filesystem root for the coordination tier
    // (refs live in a repo, not in a store). Scope-only callers have no path
    // yet, so a coordination create is refused rather than silently filed as
    // canonical — the whole point of the role is that it must not land in the
    // git-tracked record.
    let repo_root = args
        .get("repo_path")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from);
    if create_args.role == crate::bead_genesis::Role::Coordination && repo_root.is_none() {
        anyhow::bail!(
            "role `coordination` needs `repo_path` — coordination beads live in \
             that repo's refs/agents/*, and a scope-only call has no path to write to. \
             Refusing rather than filing it as canonical."
        );
    }
    crate::bead_ops::create_bead(
        client,
        repo_root.as_deref().unwrap_or(std::path::Path::new(".")),
        &id,
        &create_args,
        created_by.as_deref(),
    )
    .await?;

    // Set user_id for multi-tenant scoping
    if let Some(uid) = user_scope {
        if let Err(e) = client.set_user_id(&id, uid).await {
            eprintln!("[mcp] failed to set user_id on {id}: {e}");
        }
        client.log_event(&id, "created_by", uid).await;
    }

    // Publish only after all create-time metadata is applied so the tracked
    // projection cannot expose an intermediate record.
    let repo_root = args
        .get("repo_path")
        .and_then(Value::as_str)
        .map(|path| crate::scanner::resolve_repo_path(std::path::Path::new(path)))
        .or_else(|| pool.path_for(repo_name).map(std::path::Path::to_path_buf));
    if let Some(repo_root) = repo_root {
        crate::jsonl_sync::publish_created_bead_to_tracked_jsonl(
            client, &id, repo_name, &repo_root,
        )
        .await
        .with_context(|| {
            format!("bead {id} created locally, but publishing it to tracked .beads/beads.jsonl")
        })?;
    }

    let owner = create_args.resolved_owner();
    Ok(json!({ "id": id, "title": title, "priority": priority, "owner": owner }))
}

async fn tool_bead_update(
    args: &Value,
    pool: &RepoPool,
    _user_scope: Option<&str>,
) -> Result<Value> {
    let id = args["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("id required"))?;

    // Validate optional fields before constructing the update.
    // Use presence check (args.get(...).is_some()) rather than as_str()/as_u64()
    // so that wrong-type values (e.g. priority: -1 / "2" / 1.5) are rejected
    // instead of silently ignored.
    if let Some(tv) = args.get("title").filter(|v| !v.is_null()) {
        let t = tv
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("title must be a string, got {:?}", tv))?;
        if t.trim().is_empty() {
            anyhow::bail!("title must not be blank");
        }
        if t.len() > TITLE_MAX_LEN {
            anyhow::bail!("title exceeds {TITLE_MAX_LEN} bytes (got {})", t.len());
        }
    }
    if let Some(dv) = args.get("description").filter(|v| !v.is_null()) {
        let d = dv
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("description must be a string, got {:?}", dv))?;
        anyhow::ensure!(
            d.len() <= BODY_MAX_LEN,
            "description exceeds {BODY_MAX_LEN} bytes (got {})",
            d.len()
        );
    }
    if let Some(pv) = args.get("priority").filter(|v| !v.is_null()) {
        let p = pv.as_u64().ok_or_else(|| {
            anyhow::anyhow!("priority must be an integer 0–{PRIORITY_MAX}, got {:?}", pv)
        })?;
        anyhow::ensure!(
            p <= PRIORITY_MAX,
            "priority must be 0–{PRIORITY_MAX} (P0=critical … P3=low), got {p}"
        );
    }
    if let Some(itv) = args.get("issue_type").filter(|v| !v.is_null()) {
        let it = itv
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("issue_type must be a string, got {:?}", itv))?;
        anyhow::ensure!(
            crate::bead::VALID_ISSUE_TYPES.contains(&it),
            "unknown issue_type {:?} — valid values: {}",
            it,
            crate::bead::VALID_ISSUE_TYPES.join(", ")
        );
    }

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
        acceptance_criteria: args
            .get("acceptance_criteria")
            .and_then(|v| v.as_str())
            .map(String::from),
    };

    if update.is_empty() {
        anyhow::bail!(
            "no fields to update — provide at least one field besides scope/repo_path and id"
        );
    }

    let (_scope, client_ref) = resolve_repo_client(args, pool).await?;
    let client = client_ref.as_store();
    let updated_fields = client.update_bead_fields(id, &update).await?;

    // Log the update event for audit trail
    client
        .log_event(id, "fields_updated", &updated_fields.join(", "))
        .await;

    Ok(json!({ "id": id, "updated_fields": updated_fields }))
}

/// Correct a wrongly-recorded status (rosary-e0e19f). Not a transition.
///
/// The MCP half of the recovery path. Before this, an agent that NOTICED a
/// wrongly-closed bead could not fix it: `reopen` is CLI-only and refuses
/// `done`, and `rsry_bead_update` carries no status field — a gap `field_drift`
/// records. Recovery meant a raw UPDATE on beads.db.
async fn tool_bead_correct(
    args: &Value,
    pool: &RepoPool,
    _user_scope: Option<&str>,
) -> Result<Value> {
    let id = args["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("id required"))?;
    let status = args["status"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("status required — the value the bead should have had"))?;
    let reason = args["reason"].as_str().unwrap_or_default();

    let (_scope, client_ref) = resolve_repo_client(args, pool).await?;
    let client = client_ref.as_store();
    crate::bead_correct::correct_status(client, id, status, reason).await?;
    Ok(serde_json::json!({
        "id": id,
        "status": status,
        "corrected": true,
        "note": "recorded status corrected; the reason is on the bead as a comment"
    }))
}

async fn tool_bead_close(
    args: &Value,
    pool: &RepoPool,
    _user_scope: Option<&str>,
) -> Result<Value> {
    let id = args["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("id required"))?;
    let force = parse_bool_arg(args, "force", false);

    let (scope, client_ref) = resolve_repo_client(args, pool).await?;
    let client = client_ref.as_store();
    let repo = scope
        .as_repo_name()
        .expect("Repo-only scope verified by resolve_repo_client");
    let repo_root = args
        .get("repo_path")
        .and_then(Value::as_str)
        .map(|path| crate::scanner::resolve_repo_path(std::path::Path::new(path)))
        .or_else(|| pool.path_for(repo).map(std::path::Path::to_path_buf))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "bead {id} closed locally, but repo path for `repo:{repo}` is unavailable; \
                 pass repo_path or register the repo so tracked JSONL can be refreshed"
            )
        })?;
    crate::bead_ops::close_bead(client, id, repo, force).await?;
    crate::jsonl_sync::refresh_tracked_beads_jsonl(client, repo, &repo_root)
        .await
        .with_context(|| {
            format!("bead {id} closed locally, but refreshing tracked .beads/beads.jsonl")
        })?;

    // Unregister the session so rsry_active stops showing it.
    // Best-effort — session may not exist if bead was closed manually.
    if let Ok(mut registry) = crate::session::SessionRegistry::load() {
        let _ = registry.unregister(id, repo);
    }

    Ok(json!({ "id": id, "status": "closed" }))
}

async fn tool_bead_comment(
    args: &Value,
    pool: &RepoPool,
    _user_scope: Option<&str>,
) -> Result<Value> {
    let id = args["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("id required"))?;
    let body = args["body"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("body required"))?;
    // Shared with the CLI comment path via bead_ops (enforced 1:1).
    crate::bead_ops::validate_comment_body(body)?;

    let (scope, client_ref) = resolve_repo_client(args, pool).await?;
    let client = client_ref.as_store();
    client.add_comment(id, body, "rsry-mcp").await?;

    // Update session registry so rsry_active shows last activity
    if let Ok(mut registry) = crate::session::SessionRegistry::load() {
        // `resolve_repo_client` guarantees Repo(_); see parallel
        // comment in `tool_bead_close` (Copilot #214 finding).
        let repo = scope
            .as_repo_name()
            .expect("Repo-only scope verified by resolve_repo_client");
        let _ = registry.touch(id, repo, body);
    }

    Ok(json!({ "id": id, "comment_added": true }))
}

async fn tool_bead_comment_list(
    args: &Value,
    pool: &RepoPool,
    _user_scope: Option<&str>,
) -> Result<Value> {
    let id = args["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("id required"))?;
    let include_deleted = args
        .get("include_deleted")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let (_scope, client_ref) = resolve_repo_client(args, pool).await?;
    let client = client_ref.as_store();
    let comments = client.list_comments(id, include_deleted).await?;

    Ok(json!({
        "id": id,
        "comments": comments,
        "count": comments.len(),
    }))
}

async fn tool_bead_comment_update(
    args: &Value,
    pool: &RepoPool,
    _user_scope: Option<&str>,
) -> Result<Value> {
    // comment_id is an opaque string — Dolt produces UUIDs (char(36)),
    // SQLite produces stringified integers. Accept both via JSON string;
    // also accept JSON numbers for backward compatibility.
    let comment_id_owned: String = match args.get("comment_id") {
        Some(v) if v.is_string() => v.as_str().unwrap().to_string(),
        Some(v) if v.is_i64() => v.as_i64().unwrap().to_string(),
        Some(v) if v.is_u64() => v.as_u64().unwrap().to_string(),
        _ => anyhow::bail!("comment_id required (string)"),
    };
    let comment_id = comment_id_owned.as_str();
    let body = args["body"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("body required"))?;
    if body.trim().is_empty() {
        anyhow::bail!("body must not be blank");
    }
    if body.len() > BODY_MAX_LEN {
        anyhow::bail!("body exceeds {BODY_MAX_LEN} bytes (got {})", body.len());
    }
    let reason = args.get("reason").and_then(|v| v.as_str());

    let (_scope, client_ref) = resolve_repo_client(args, pool).await?;
    let client = client_ref.as_store();
    let updated = client.update_comment(comment_id, body, reason).await?;

    Ok(json!({
        "comment_id": comment_id,
        "updated": true,
        "comment": updated,
    }))
}

async fn tool_bead_comment_delete(
    args: &Value,
    pool: &RepoPool,
    _user_scope: Option<&str>,
) -> Result<Value> {
    // comment_id is an opaque string — Dolt produces UUIDs (char(36)),
    // SQLite produces stringified integers. Accept both via JSON string;
    // also accept JSON numbers for backward compatibility.
    let comment_id_owned: String = match args.get("comment_id") {
        Some(v) if v.is_string() => v.as_str().unwrap().to_string(),
        Some(v) if v.is_i64() => v.as_i64().unwrap().to_string(),
        Some(v) if v.is_u64() => v.as_u64().unwrap().to_string(),
        _ => anyhow::bail!("comment_id required (string)"),
    };
    let comment_id = comment_id_owned.as_str();
    let reason = args.get("reason").and_then(|v| v.as_str());

    // MCP path is soft-delete only — never hard. Hard-delete is CLI-only by
    // design (see rosary-a96b06: audit-trail preservation).
    let (_scope, client_ref) = resolve_repo_client(args, pool).await?;
    let client = client_ref.as_store();
    client.delete_comment(comment_id, reason).await?;

    Ok(json!({
        "comment_id": comment_id,
        "soft_deleted": true,
    }))
}

/// Infer the cross-repo target from a `depends_on` bead id and the
/// calling repo name. Returns `Some("repo/bead-id")` when the bead id
/// matches the canonical `<repo>-<6hex>` shape and the `<repo>` prefix
/// names a repo other than the calling one — meaning the dep is
/// cross-repo even though the caller didn't pass `cross_repo`
/// explicitly.
///
/// Returns None when:
/// - the bead id is malformed (no recognizable `<repo>-<6hex>` shape),
/// - the inferred prefix matches the calling repo (so the dep is same-repo),
/// - no calling repo name is available (e.g. `External` / `Global` scope —
///   auto-routing has nothing to compare against).
///
/// rosary-98ee93: lets callers express cross-repo deps using only the
/// canonical bead id without needing to remember the `cross_repo` arg
/// shape. Explicit `cross_repo` still takes precedence when provided.
///
/// rosary-b5da2f PR 4: takes the calling repo name directly (was:
/// `repo_path: &str` + internal `repo_name_from_path` call). The new
/// signature works for any `ScopeId` — Repo callers pass the name;
/// External/Global callers pass None (via `ScopeId::as_repo_name`).
fn infer_cross_repo_target(calling_repo: Option<&str>, depends_on: &str) -> Option<String> {
    let calling_repo = calling_repo?;
    // Bead IDs follow `<repo>-<6hex>` (e.g. `signet-9605a3`). Repo names may
    // contain `-` themselves (e.g. `ley-line-open`), so split on the LAST
    // `-` and check whether the suffix is a 6-char hex tag.
    let (prefix, suffix) = depends_on.rsplit_once('-')?;
    if prefix.is_empty() || suffix.len() != 6 || !suffix.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    if calling_repo == prefix {
        return None;
    }
    Some(format!("{prefix}/{depends_on}"))
}

async fn tool_bead_link(
    args: &Value,
    pool: &RepoPool,
    backend: Option<&dyn BackendStore>,
) -> Result<Value> {
    use crate::serve::scope_args::resolve_scope;
    // rosary-b5da2f PR 4: accept canonical `scope` arg alongside `repo_path`.
    // resolve_scope handles the precedence + error for "neither provided".
    let scope = resolve_scope(args)?;
    // Accept `id` (canonical). When the caller passes the common-but-wrong
    // shorthand `from_id`/`to_id`/`link_type`, the error names both
    // canonical params so the caller doesn't have to guess (rosary-98b11d).
    let id = args["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!(
            "id required — got args without `id`. Expected canonical params: `id` (the dependent bead) + `depends_on` (the prerequisite). \
             If you tried `from_id`/`to_id`/`link_type`, those aren't accepted; use `id`/`depends_on` instead."
        ))?;
    let depends_on = args["depends_on"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!(
            "depends_on required — got `id` but no `depends_on`. \
             For cross-repo deps, you may set `depends_on` to either `<bead-id>` (auto-routed by id prefix) or pass `cross_repo` as `<repo>/<bead-id>` explicitly."
        ))?;
    let remove = args
        .get("remove")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    // Typed edge (rosary-649660): blocks (default) | related | parent-child |
    // discovered-from. Containment types (parent-child/discovered-from) drive
    // the close-merged gate — a parent won't auto-close while children are open.
    let dep_type = args
        .get("dep_type")
        .or_else(|| args.get("link_type"))
        .and_then(|v| v.as_str())
        .unwrap_or("blocks");
    // cross_repo: "other-repo/bead-id" — writes to backend LinkageStore instead of per-repo Dolt.
    // Auto-detect (rosary-98ee93): if `depends_on` carries a `<repo>-<id>` prefix that doesn't
    // match the calling repo name, route through LinkageStore without requiring the
    // caller to remember the explicit `cross_repo` argument shape. Auto-detect only
    // engages when scope is `Repo(_)`; External/Global have no bare-name prefix.
    let cross_repo_target: Option<String> = args
        .get("cross_repo")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| infer_cross_repo_target(scope.as_repo_name(), depends_on));

    if id == depends_on {
        anyhow::bail!("a bead cannot depend on itself ({id})");
    }

    if let Some(target) = cross_repo_target.as_deref() {
        // Cross-repo dep: parse "repo/bead-id" and write to backend LinkageStore.
        let (to_repo, to_bead) = target
            .split_once('/')
            .ok_or_else(|| anyhow::anyhow!("cross_repo must be 'repo-name/bead-id'"))?;
        // Parse `to_repo` as a ScopeId so reserved-namespace strings
        // (`"global"`, `"external:..."`) are rejected — they could
        // otherwise create rows in the reserved namespace via the
        // wrong-side arg, breaking the invariant that Global/External
        // rows are only produced via the matching `ScopeId` variant
        // (Copilot #212 finding).
        let to_scope: crate::scope::ScopeId = to_repo
            .parse()
            .with_context(|| format!("parse cross_repo target repo `{to_repo}` as ScopeId"))?;
        if to_scope.as_repo_name().is_none() {
            anyhow::bail!(
                "cross_repo target repo `{to_repo}` is in a reserved namespace ({to_scope}); \
                 cross_repo is repo-to-repo only. For External/Global targets, use the \
                 `scope` arg on the matching side."
            );
        }
        // Build the `from` WorkRef via the ScopeId bridge so External / Global
        // scopes encode correctly into the LinkageStore schema.
        let from_wr = scope
            .work_ref(id)
            .with_context(|| format!("encode from-scope for bead `{id}`"))?;
        let to_wr = to_scope
            .work_ref(to_bead)
            .with_context(|| format!("encode cross_repo target `{target}`"))?;
        let dep = CrossRepoDep {
            from: from_wr,
            to: to_wr,
            dep_type: "blocks".to_string(),
            evidence_tier: EvidenceTier::Asserted,
            source: "human".to_string(),
            observed_at: chrono::Utc::now(),
        };
        let linkage = backend
            .ok_or_else(|| anyhow::anyhow!("backend store required for cross-repo links"))?;
        if remove {
            linkage.remove_cross_repo_dep(&dep.from, &dep.to).await?;
            Ok(json!({ "id": id, "cross_repo": target, "action": "removed" }))
        } else {
            linkage.add_dependency(&dep).await?;
            Ok(json!({
                "id": id,
                "cross_repo": target,
                "action": "added",
                "evidence_tier": "asserted"
            }))
        }
    } else {
        // Same-repo dep: needs a per-repo Dolt store. Only `Repo` scope has one;
        // External / Global have no backing per-repo store and must route via
        // `cross_repo` instead (rosary-b5da2f PR 4).
        match scope.as_repo_name() {
            Some(_repo_name) => {
                // Resolve the per-repo store from `scope` alone (pool lookup by
                // name) OR an explicit `repo_path`. `resolve_repo_client` owns
                // the precedence, the scope/path-mismatch guard, and the
                // repo_path fallback — so `rsry_bead_link(scope="repo:rosary")`
                // works without also passing `repo_path` (rosary-d7a98e).
                let (_scope, client_ref) = resolve_repo_client(args, pool).await?;
                let client = client_ref.as_store();
                if remove {
                    client.remove_dependency(id, depends_on).await?;
                    Ok(json!({ "id": id, "depends_on": depends_on, "action": "removed" }))
                } else {
                    client
                        .add_dependency_typed(id, depends_on, dep_type)
                        .await?;
                    Ok(
                        json!({ "id": id, "depends_on": depends_on, "dep_type": dep_type, "action": "added" }),
                    )
                }
            }
            None => {
                // External / Global scope has no per-repo Dolt store; same-repo
                // semantics don't apply. Point the caller at the LinkageStore
                // path via `cross_repo`.
                anyhow::bail!(
                    "same-repo deps are not supported from {scope} scope — \
                     {scope} has no per-repo Dolt store. Pass `cross_repo` to \
                     write the dep through LinkageStore instead."
                );
            }
        }
    }
}

/// Rosary-owned observation history for a bead (rosary-d18be8 / rosary-d298a3):
/// the append-only review + verify verdicts folded through the lattice. GitHub
/// PR threads / CI checks are a *projection* — this survives without them and is
/// queryable from rosary directly.
async fn tool_bead_history(args: &Value, pool: &RepoPool) -> Result<Value> {
    let (scope, client_ref) = resolve_repo_client(args, pool).await?;
    let client = client_ref.as_store();
    let id = args["id"].as_str().ok_or_else(|| {
        anyhow::anyhow!("id required — the bead whose observation history to read")
    })?;

    let events = client.list_event_details(id, "observation").await?;
    let work = scope.work_ref(id)?;
    let observations = crate::observation::shadow::parse_events_for(&events, &work);
    let folded = crate::observation::shadow::folded_pipeline_verdict(&observations, &work);

    // Parse the raw envelopes so we can surface the recorded commit SHA + human
    // detail alongside each verdict (d298a3: CI/verify results carry the
    // reviewed SHA, not just the verdict).
    let history: Vec<Value> = events
        .iter()
        .filter_map(|e| {
            let env: Value = serde_json::from_str(e).ok()?;
            let o = &env.get("observation")?;
            let verdict = o.get("value").and_then(|v| v.get("value")).cloned();
            Some(json!({
                "source": o.get("source"),
                "event": o.get("source_event_id"),
                "verdict": verdict,
                "observed_at": o.get("observed_at"),
                "git_sha": env.get("git_sha"),
                "detail": env.get("detail"),
            }))
        })
        .collect();

    Ok(json!({
        "id": id,
        "scope": scope.to_string(),
        "folded_status": folded.map(|v| format!("{v:?}")),
        "observation_count": observations.len(),
        "history": history,
        "note": "Rosary-owned observation history (review + verify verdicts, ADR-0010). \
                 GitHub PR/CI state is a projection of this, not the source of truth.",
    }))
}

async fn tool_bead_search(
    args: &Value,
    pool: &RepoPool,
    _user_scope: Option<&str>,
) -> Result<Value> {
    let query_str = args["query"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("query required"))?;
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(SEARCH_DEFAULT_LIMIT)
        .min(SEARCH_MAX_LIMIT) as u32;

    let (scope, client_ref) = resolve_repo_client(args, pool).await?;
    let client = client_ref.as_store();
    let repo_name = scope
        .as_repo_name()
        .expect("Repo-only scope verified by resolve_repo_client");
    let beads = client.search_beads(query_str, repo_name, limit).await?;

    // Truncate descriptions to keep response size bounded
    let beads: Vec<Value> = beads
        .iter()
        .map(|b| {
            let mut v = serde_json::to_value(b).context("serializing bead for search results")?;
            if let Some(desc) = v.get("description").and_then(|d| d.as_str())
                && desc.len() > SEARCH_DESC_TRUNCATE
            {
                // Truncate at char boundary to avoid panic on multi-byte UTF-8
                let end = desc
                    .char_indices()
                    .map(|(i, _)| i)
                    .find(|&i| i >= SEARCH_DESC_TRUNCATE)
                    .unwrap_or(desc.len());
                let truncated = format!("{}...", &desc[..end]);
                v["description"] = Value::String(truncated);
            }
            Ok(v)
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(json!({ "count": beads.len(), "beads": beads }))
}

// ---------------------------------------------------------------------------
// Dispatch / active
// ---------------------------------------------------------------------------

/// MCP dispatch: prepares workspace + prompt + spawns the agent as a
/// detached subprocess. Returns the pid + stream-log path.
///
/// Rationale (rosary-748f07): the previous "return a command string for
/// the MCP caller to spawn themselves" design was blocked by Claude Code's
/// safety classifier ("Create Unsafe Agents"), so the dispatched agent
/// never actually ran. Server-side spawn with `setsid` puts the worker in
/// its own session — the classifier doesn't fire (the spawn happens
/// outside the calling harness) and the worker survives even if the MCP
/// caller exits.
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

    let root = crate::scanner::resolve_repo_path(std::path::Path::new(repo_path));
    let beads_dir = crate::resolve_beads_dir(&root);
    let client = crate::bead_sqlite::connect_bead_store(&beads_dir).await?;
    let repo_name = repo_name_from_path(repo_path);

    // Self-heal stale state before staging a new dispatch (rosary-67c43d).
    // Reverts any prior `Dispatched` bead in this repo that has no live
    // session and no worktree on disk — typically caused by an MCP caller
    // that never spawned the agent process. Conservative: never touches
    // beads with live sessions or existing worktrees.
    let _ =
        crate::dispatch::sweep::sweep_orphan_dispatches(client.as_ref(), &root, &repo_name).await;

    let mut bead = client
        .get_bead(bead_id, &repo_name)
        .await?
        .ok_or_else(|| anyhow::anyhow!("bead {bead_id} not found"))?;

    // Agent override takes precedence over bead.owner
    if let Some(agent) = agent_override {
        bead.owner = Some(agent.to_string());
    }

    crate::dispatch::ensure_dispatch_close_condition(&bead)?;

    let agent_label = bead
        .owner
        .as_deref()
        .unwrap_or_else(|| crate::dispatch::default_agent(&bead.issue_type));

    // Create isolated workspace (worktree/jj workspace) — this is safe to do
    // from the server because it's just git operations, no process spawning.
    let workspace = if isolate {
        match crate::workspace::Workspace::create(bead_id, &repo_name, &root, true).await {
            Ok(ws) => Some(ws),
            Err(e) => {
                eprintln!("[dispatch] workspace creation failed: {e}");
                None
            }
        }
    } else {
        None
    };

    let work_dir = workspace
        .as_ref()
        .map(|ws| ws.work_dir.to_string_lossy().to_string())
        .unwrap_or_else(|| root.to_string_lossy().to_string());

    // Build the prompt + system_prompt the agent will see. system_prompt is
    // assembled from the agent definition + golden rules.
    let agents_dir = crate::dispatch::resolve_agents_dir();
    let prompt = crate::dispatch::build_prompt(
        &bead,
        &work_dir,
        workspace.as_ref().map(|ws| ws.work_dir.as_path()),
        bead.owner.as_deref(),
    );
    let system_prompt = crate::dispatch::build_system_prompt(
        bead.owner.as_deref(),
        agents_dir.as_deref(),
        crate::dispatch::permission_profile(&bead.issue_type),
    );

    // Agent-specific permission override (mirrors the orchestrator path).
    let perms = match bead.owner.as_deref() {
        Some("scoping-agent") => crate::dispatch::PermissionProfile::ReadOnly,
        Some("staging-agent") => crate::dispatch::PermissionProfile::ReadOnly,
        Some("pm-agent") => crate::dispatch::PermissionProfile::Plan,
        Some("architect-agent") => crate::dispatch::PermissionProfile::Plan,
        _ => crate::dispatch::permission_profile(&bead.issue_type),
    };
    let allowed_tools = perms.claude_allowed_tools().to_string();

    // Trust model: rosary's MCP transports are all post-auth by deployment
    // topology — stdio is the local user's own shell, IPC (UDS) is gated by
    // filesystem permissions, and HTTP sits behind cloister (the MCP
    // gateway, per ADR-0005). Authentication is cloister's job, not
    // rosary's. The earlier `allow_mcp_spawn` config flag (PR #200) was
    // rosary reinventing auth one level too deep; it was removed in this
    // commit. If you publicly expose rosary's HTTP transport you've broken
    // the deployment contract — fix that, don't ask rosary to compensate.
    let cfg = crate::config::load(_config_path).ok();

    let binaries = cfg
        .as_ref()
        .and_then(|c| c.dispatch.as_ref())
        .map(|d| d.binaries.clone())
        .unwrap_or_default();
    let provider = crate::dispatch::providers::provider_by_name(provider_name, &binaries)
        .with_context(|| format!("resolving provider {provider_name}"))?;

    // Pre-flight: load the registry now and validate it's writable so we
    // don't spawn a worker we can't track. Loading is best-effort
    // (corrupted file → start fresh); saving is the real test.
    let mut registry = crate::session::SessionRegistry::load().unwrap_or_default();

    let work_dir_path = std::path::Path::new(&work_dir);
    let (command_bin, _) = provider.build_command(&prompt, &perms, &system_prompt);
    let mut native_session: Option<Box<dyn crate::dispatch::AgentSession>> = None;
    let (pid, session_ref, stream_path) = if command_bin.is_empty() {
        let run_spec = crate::dispatch::providers::AgentRunSpec::new(
            prompt.clone(),
            work_dir_path.to_path_buf(),
            perms,
            system_prompt.clone(),
        )
        .with_bead_context(bead_id.to_string(), bead.owner.clone());
        let session = provider
            .spawn_run(&run_spec)
            .with_context(|| format!("spawning native {provider_name} session for {bead_id}"))?;
        let pid = session.pid();
        let session_ref = session.session_ref();
        anyhow::ensure!(
            pid.is_some() || session_ref.is_some(),
            "native provider {provider_name} returned no pid or session_ref for {bead_id}"
        );
        let stream_path = work_dir_path.join(crate::dispatch::STREAM_LOG_FILENAME);
        native_session = Some(session);
        (pid, session_ref, stream_path)
    } else {
        // Server-side spawn: setsid + detached so the worker survives the MCP
        // request returning AND isn't subject to the caller's safety classifier.
        let spawned = crate::dispatch::spawn_detached(
            provider.as_ref(),
            &prompt,
            work_dir_path,
            &perms,
            &system_prompt,
        )
        .await
        .with_context(|| format!("spawning agent for {bead_id}"))?;
        (Some(spawned.pid), None, spawned.stream_log)
    };

    let workspace_vcs = workspace
        .as_ref()
        .map(|ws| match ws.vcs {
            crate::workspace::VcsKind::Jj => "jj",
            crate::workspace::VcsKind::Git => "git",
            crate::workspace::VcsKind::None => "",
        })
        .unwrap_or("")
        .to_string();
    let session_entry = crate::session::SessionEntry {
        bead_id: bead_id.to_string(),
        repo: repo_name.clone(),
        provider: provider_name.to_string(),
        pid,
        session_ref: session_ref.clone(),
        work_dir: work_dir.clone(),
        started_at: chrono::Utc::now(),
        title: bead.title.clone(),
        agent: agent_label.to_string(),
        workspace_vcs,
        repo_path: repo_path.to_string(),
        last_activity: None,
        last_comment: None,
    };

    // Register the live session so `rsry_active` / health-check can see it.
    // If registry.register fails (permissions, disk full, concurrent write)
    // we have to choose: leak an untracked worker, or kill it. We kill,
    // because an untracked worker is worse than no worker — it leaves the
    // bead in an indeterminate state and gives the operator no handle to
    // recover.
    if let Err(e) = registry.register(session_entry) {
        if let Some(pid) = pid {
            eprintln!("[dispatch] registry write failed; killing untracked worker pid={pid}: {e}");
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
        }
        if let Some(session) = native_session.as_mut() {
            let _ = session.kill();
        }
        return Err(e).with_context(|| {
            format!("persisting session for {bead_id} (untracked worker killed to keep state consistent)")
        });
    }

    // Mark bead in-progress: the worker is actually running now (behavior
    // change from the pre-748f07 design, which marked `dispatched` and
    // waited for the caller to spawn the process). Status update failing
    // here is recoverable (operator can mark out-of-band) so we don't kill
    // the worker — just surface the error.
    let worker_address = session_ref
        .as_ref()
        .map(|r| format!("{}:{}", r.provider, r.id))
        .or_else(|| pid.map(|pid| format!("pid {pid}")))
        .unwrap_or_else(|| "unknown address".to_string());
    client
        .update_status(bead_id, "in_progress")
        .await
        .with_context(|| {
            format!(
                "marking bead {bead_id} in_progress (worker is RUNNING at {worker_address}; \
                 bead status update failed but the agent is tracked in sessions.json)",
            )
        })?;

    Ok(json!({
        "bead_id": bead_id,
        "title": bead.title,
        "status": "in_progress",
        "agent": agent_label,
        "provider": provider_name,
        "work_dir": work_dir,
        "pid": pid,
        "session_ref": session_ref.as_ref().map(|r| json!({
            "provider": r.provider,
            "id": r.id,
        })),
        "stream": stream_path.display().to_string(),
        "allowed_tools": allowed_tools,
        "instructions": "Worker spawned server-side. Tail `stream` when present, or call rsry_active to list live sessions.",
    }))
}

async fn tool_active(backend: Option<&dyn BackendStore>) -> Result<Value> {
    let registry = crate::session::SessionRegistry::load().unwrap_or_default();
    tool_active_with_registry(backend, registry).await
}

async fn tool_active_with_registry(
    backend: Option<&dyn BackendStore>,
    registry: crate::session::SessionRegistry,
) -> Result<Value> {
    let mut running = Vec::new();
    let mut completed = Vec::new();
    let mut seen = std::collections::BTreeSet::new();

    for s in registry.active() {
        let health = check_agent_health(s);
        seen.insert((s.repo.clone(), s.bead_id.clone()));
        let entry = json!({
            "source": "session_registry",
            "bead_id": s.bead_id,
            "title": s.title,
            "agent": s.agent,
            "repo": s.repo,
            "provider": s.provider,
            "pid": s.pid,
            "session_ref": s.session_ref.as_ref().map(|r| json!({
                "provider": r.provider,
                "id": r.id,
            })),
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

    let mut backend_active_dispatches = 0;
    let mut backend_active_pipelines = 0;
    if let Some(backend) = backend {
        let dispatches = backend.active_dispatches().await?;
        backend_active_dispatches = dispatches.len();
        let pipelines = backend.list_active_pipelines().await?;
        backend_active_pipelines = pipelines.len();

        let pipeline_by_bead: BTreeMap<_, _> = pipelines
            .iter()
            .map(|p| ((p.bead_ref.repo.clone(), p.bead_ref.bead_id.clone()), p))
            .collect();

        for d in &dispatches {
            let key = (d.bead_ref.repo.clone(), d.bead_ref.bead_id.clone());
            if seen.contains(&key) {
                continue;
            }
            seen.insert(key.clone());
            let pipeline = pipeline_by_bead.get(&key);
            running.push(json!({
                "source": "backend",
                "bead_id": d.bead_ref.bead_id,
                "agent": d.agent,
                "repo": d.bead_ref.repo,
                "provider": d.provider,
                "dispatch_id": d.id,
                "pid": Value::Null,
                "session_id": d.session_id,
                "session_ref": d.session_ref.as_ref().map(|r| json!({
                    "provider": r.provider,
                    "id": r.id,
                })),
                "work_dir": d.work_dir,
                "started_at": d.started_at.to_rfc3339(),
                "last_activity": Value::Null,
                "last_comment": Value::Null,
                "health": "persisted_active",
                "pipeline": pipeline.map(|p| json!({
                    "phase": p.pipeline_phase,
                    "agent": p.pipeline_agent,
                    "phase_status": p.phase_status,
                    "retries": p.retries,
                })),
            }));
        }

        for p in pipelines {
            let key = (p.bead_ref.repo.clone(), p.bead_ref.bead_id.clone());
            if seen.contains(&key) {
                continue;
            }
            seen.insert(key);
            running.push(json!({
                "source": "backend_pipeline",
                "bead_id": p.bead_ref.bead_id,
                "agent": p.pipeline_agent,
                "repo": p.bead_ref.repo,
                "provider": Value::Null,
                "pid": Value::Null,
                "session_ref": Value::Null,
                "work_dir": Value::Null,
                "started_at": Value::Null,
                "last_activity": Value::Null,
                "last_comment": Value::Null,
                "health": "pipeline_state",
                "pipeline": {
                    "phase": p.pipeline_phase,
                    "agent": p.pipeline_agent,
                    "phase_status": p.phase_status,
                    "retries": p.retries,
                },
            }));
        }
    }

    Ok(json!({
        "running": running.len(),
        "completed": completed.len(),
        "agents": running,
        "needs_merge": completed,
        "backend": {
            "active_dispatches": backend_active_dispatches,
            "active_pipelines": backend_active_pipelines,
        },
    }))
}

/// Quick health check for a dispatched agent.
/// Returns "healthy", "idle", "stuck", or "dead".
fn check_agent_health(session: &crate::session::SessionEntry) -> &'static str {
    if session.pid.is_none() && session.session_ref.is_some() {
        return "healthy";
    }

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

async fn tool_workspace_create(
    args: &Value,
    config_path: &str,
    repo_cache: &crate::repo_cache::RepoCache,
) -> Result<Value> {
    let bead_id = args["bead_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("bead_id required"))?;
    let repo_path = args["repo_path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("repo_path required"))?;

    // Remote repo URL: clone on demand, then proceed with the local path.
    // Only https:// is accepted — http:// is rejected to avoid credential leakage.
    if repo_path.starts_with("http://") {
        anyhow::bail!("insecure http:// URLs not allowed — use https://");
    }
    let root = if repo_path.starts_with("https://") {
        let github_token = resolve_github_token(config_path).await;
        repo_cache
            .ensure_local(repo_path, github_token.as_deref())
            .await?
    } else {
        crate::scanner::resolve_repo_path(std::path::Path::new(repo_path))
    };

    let repo_name = repo_name_from_path(&root.to_string_lossy());
    let ws = crate::workspace::Workspace::create(bead_id, &repo_name, &root, true).await?;

    Ok(json!({
        "bead_id": bead_id,
        "work_dir": ws.work_dir.to_string_lossy(),
        "vcs": format!("{:?}", ws.vcs),
        "repo_path": ws.repo_path.to_string_lossy(),
    }))
}

/// Resolve a GitHub token for cloning private repos.
/// Tries GitHub App installation token first, then GITHUB_TOKEN env var.
async fn resolve_github_token(config_path: &str) -> Option<String> {
    if let Ok(cfg) = config::load(config_path)
        && let Some(gh_cfg) = cfg.github
        && let Ok(client) = crate::github::GitHubClient::from_config(&gh_cfg)
        && let Ok(token) = client.bearer_token().await
    {
        return Some(token);
    }
    std::env::var("GITHUB_TOKEN").ok()
}

async fn tool_workspace_checkpoint(args: &Value) -> Result<Value> {
    let bead_id = args["bead_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("bead_id required"))?;
    let repo_path = args["repo_path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("repo_path required"))?;
    let message = args["message"].as_str().unwrap_or("agent work");

    let root = crate::scanner::resolve_repo_path(std::path::Path::new(repo_path));
    let repo_name = repo_name_from_path(repo_path);

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

    let root = crate::scanner::resolve_repo_path(std::path::Path::new(repo_path));
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
    let base_branch = args.get("base_branch").and_then(|v| v.as_str());
    if let Some(b) = base_branch {
        anyhow::ensure!(!b.trim().is_empty(), "base_branch must not be blank");
    }

    let root = crate::scanner::resolve_repo_path(std::path::Path::new(repo_path));
    let branch = format!("fix/{bead_id}");

    let result =
        crate::workspace::merge_or_pr_with_base(&root, &branch, bead_id, issue_type, base_branch)
            .await?;

    // Unregister the session after merge — agent is done, work is landed.
    if let Ok(mut registry) = crate::session::SessionRegistry::load() {
        let repo = std::path::Path::new(repo_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let _ = registry.unregister(bead_id, repo);
    }

    Ok(json!({
        "bead_id": bead_id,
        "branch": branch,
        "result": result.message,
        "pr_url": result.pr_url,
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

    let model = args
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let (atoms, meta) = if model.is_some() && !bdr::parse::is_adr_shaped(&markdown) {
        let model_name = model.as_deref().unwrap();
        let atoms = crate::bdr_enrich::extract_atoms_with_llm(&markdown, model_name).await?;
        let meta = bdr::parse::DocMeta {
            provenance: Some(bdr::provenance::ProvenanceRef::Doc {
                path: path.to_string(),
            }),
            ..Default::default()
        };
        (atoms, meta)
    } else {
        let parsed = bdr::parse::parse_doc_full(&markdown, path);
        (parsed.atoms, parsed.meta)
    };

    if atoms.is_empty() {
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

    let mut decade = bdr::thread::build_decade_with_meta(path, &title, &atoms, &meta);

    // Stamp inferred_from on every BeadSpec when LLM extraction was used.
    if let Some(ref model_name) = model {
        let trace = bdr::provenance::InferenceTrace {
            model: crate::bdr_enrich::resolve_model_id(model_name).to_string(),
            rationale: None,
        };
        for thread in &mut decade.threads {
            for spec in &mut thread.beads {
                spec.inferred_from = Some(trace.clone());
            }
        }
    }

    let commit = args
        .get("commit")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut created = 0usize;
    let mut skipped = 0usize;

    if commit {
        let repo_path = args
            .get("repo_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("repo_path is required when commit=true"))?;

        let repo_root = std::path::Path::new(repo_path);
        let beads_dir = repo_root.join(".beads");
        let client = crate::bead_sqlite::connect_bead_store(&beads_dir).await?;
        let repo_name = repo_root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| repo_path.to_string());

        // Connect to backend for lattice assignment (best-effort).
        let backend: Option<Box<dyn BackendStore>> =
            match config::load_global().ok().and_then(|c| c.backend) {
                Some(cfg) => cfg.connect().await.ok(),
                None => None,
            };

        if let Some(ref b) = backend {
            let _ = b
                .upsert_decade(&crate::store::DecadeRecord {
                    id: decade.id.clone(),
                    title: decade.title.clone(),
                    source_path: path.to_string(),
                    status: "active".to_string(),
                })
                .await;
            for thread in &decade.threads {
                let prefix = config::load_global()
                    .ok()
                    .and_then(|c| c.github)
                    .map(|g| g.agent_branch_prefix)
                    .unwrap_or_else(|| "rosary".to_string());
                let feature_branch = crate::workspace::thread_branch_name(&prefix, &thread.name);
                let _ = b
                    .upsert_thread(&crate::store::ThreadRecord {
                        id: thread.id.clone(),
                        name: thread.name.clone(),
                        decade_id: decade.id.clone(),
                        feature_branch: Some(feature_branch),
                    })
                    .await;
            }
        }

        for thread in &decade.threads {
            for spec in &thread.beads {
                // Dedup: skip exact title matches.
                let existing = client
                    .search_beads(&spec.title, &repo_name, 10)
                    .await
                    .unwrap_or_default();
                if existing.iter().any(|b| b.title == spec.title) {
                    skipped += 1;
                    continue;
                }

                let desc = enrich_decompose_description(spec);
                let id = crate::generate_bead_id(&repo_name);
                let owner = crate::dispatch::default_agent(&spec.issue_type);
                client
                    .create_bead_full(crate::store::NewBead {
                        id: id.clone(),
                        title: spec.title.clone(),
                        description: desc,
                        priority: spec.priority,
                        issue_type: spec.issue_type.clone(),
                        owner: owner.to_string(),
                        derived_from: spec.derived_from.clone(),
                        acceptance_criteria: spec.close_condition_text(),
                        ..Default::default()
                    })
                    .await?;

                if let Some(ref b) = backend {
                    let _ = b
                        .add_bead_to_thread(
                            &thread.id,
                            &WorkRef {
                                repo: repo_name.clone(),
                                bead_id: id,
                                scope: String::new(),
                            },
                        )
                        .await;
                }
                created += 1;
            }
        }
    }

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
                "references": b.references,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "atom_count": atoms.len(),
        "committed": commit,
        "beads_created": created,
        "beads_skipped": skipped,
    }))
}

/// Enrich a `BeadSpec` description with success criteria and references.
fn enrich_decompose_description(spec: &bdr::decompose::BeadSpec) -> String {
    let mut desc = spec.description.clone();

    if !spec.success_criteria.is_empty() {
        desc.push_str("\n\n## Success Criteria\n\n");
        for sc in &spec.success_criteria {
            match (&sc.command, &sc.threshold) {
                (Some(cmd), _) => desc.push_str(&format!("- `{cmd}` — {}\n", sc.description)),
                (None, Some(t)) => {
                    desc.push_str(&format!("- {} (threshold: {t})\n", sc.description))
                }
                (None, None) => desc.push_str(&format!("- {}\n", sc.description)),
            }
        }
    }

    if !spec.references.is_empty() {
        desc.push_str("\n\n## References\n\n");
        for r in &spec.references {
            desc.push_str(&format!("- {r}\n"));
        }
    }

    if !spec.derived_from.is_empty() {
        desc.push_str("\n\n## Derived From\n\n");
        for src in &spec.derived_from {
            desc.push_str(&format!("- {}\n", src.label()));
        }
        if let Some(ref trace) = spec.inferred_from {
            desc.push_str(&format!(
                "\n_Classification assisted by `{}`{}_\n",
                trace.model,
                trace
                    .rationale
                    .as_deref()
                    .map(|r| format!(": {r}"))
                    .unwrap_or_default()
            ));
        }
    }

    desc
}

// ---------------------------------------------------------------------------
// Pipeline / dispatch record / history
// ---------------------------------------------------------------------------

pub(crate) async fn tool_pipeline_upsert(
    args: &Value,
    backend: Option<&dyn BackendStore>,
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
        bead_ref: WorkRef {
            repo: repo.to_string(),
            scope: String::new(),
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

async fn tool_pipeline_query(args: &Value, backend: Option<&dyn BackendStore>) -> Result<Value> {
    let backend = backend.ok_or_else(|| anyhow::anyhow!("backend store not configured"))?;

    let repo = args.get("repo").and_then(|v| v.as_str());
    let bead_id = args.get("bead_id").and_then(|v| v.as_str());

    match (repo, bead_id) {
        (Some(repo), Some(bead_id)) => {
            let bead_ref = WorkRef {
                repo: repo.to_string(),
                scope: String::new(),
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

async fn tool_dispatch_record(args: &Value, backend: Option<&dyn BackendStore>) -> Result<Value> {
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
    let session_id = args
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let session_ref = parse_session_ref_arg(args)?;

    let record = DispatchRecord {
        id: id.to_string(),
        bead_ref: WorkRef {
            repo: repo.to_string(),
            scope: String::new(),
            bead_id: bead_id.to_string(),
        },
        agent: agent.to_string(),
        provider: provider.to_string(),
        started_at: chrono::Utc::now(),
        completed_at: None,
        outcome: None,
        work_dir: work_dir.to_string(),
        session_id,
        session_ref,
        workspace_path: None,
        chain_hash: None,
    };

    backend.record_dispatch(&record).await?;
    Ok(json!({ "id": id, "bead_id": bead_id, "recorded": true }))
}

async fn tool_dispatch_history(args: &Value, backend: Option<&dyn BackendStore>) -> Result<Value> {
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
                "session_id": d.session_id,
                "session_ref": d.session_ref.as_ref().map(|r| json!({
                    "provider": r.provider,
                    "id": r.id,
                })),
            })
        })
        .collect();

    Ok(json!({ "count": items.len(), "dispatches": items }))
}

fn parse_session_ref_arg(args: &Value) -> Result<Option<AgentSessionRef>> {
    match args.get("session_ref") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let obj = v
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("session_ref must be an object, got {:?}", v))?;
            let provider = obj
                .get("provider")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("session_ref.provider required"))?;
            let id = obj
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("session_ref.id required"))?;
            Ok(Some(AgentSessionRef::new(provider, id)))
        }
    }
}

fn parse_agent_event_payload(args: &Value) -> Result<Value> {
    match args.get("payload") {
        None | Some(Value::Null) => Ok(json!({})),
        Some(v) if v.is_object() => Ok(v.clone()),
        Some(v) => anyhow::bail!("payload must be an object, got {:?}", v),
    }
}

fn parse_agent_event_created_at(args: &Value) -> Result<chrono::DateTime<chrono::Utc>> {
    match args.get("created_at") {
        None | Some(Value::Null) => Ok(chrono::Utc::now()),
        Some(v) => {
            let raw = v
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("created_at must be an RFC3339 string"))?;
            chrono::DateTime::parse_from_rfc3339(raw)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .with_context(|| format!("parsing created_at `{raw}` as RFC3339"))
        }
    }
}

async fn tool_agent_run_event_record(
    args: &Value,
    backend: Option<&dyn BackendStore>,
) -> Result<Value> {
    let backend = backend.ok_or_else(|| anyhow::anyhow!("backend store not configured"))?;

    let id = args["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("id required"))?;
    let dispatch_id = args["dispatch_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("dispatch_id required"))?;
    let repo = args["repo"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("repo required"))?;
    let bead_id = args["bead_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("bead_id required"))?;
    let scope = args.get("scope").and_then(|v| v.as_str()).unwrap_or("");
    let event_type = args["event_type"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("event_type required"))?;
    let summary = args["summary"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("summary required"))?;

    let event = AgentRunEvent {
        id: id.to_string(),
        dispatch_id: dispatch_id.to_string(),
        bead_ref: WorkRef {
            repo: repo.to_string(),
            scope: scope.to_string(),
            bead_id: bead_id.to_string(),
        },
        session_ref: parse_session_ref_arg(args)?,
        event_type: event_type.to_string(),
        summary: summary.to_string(),
        payload: parse_agent_event_payload(args)?,
        created_at: parse_agent_event_created_at(args)?,
    };

    backend.record_agent_run_event(&event).await?;
    Ok(json!({ "id": id, "bead_id": bead_id, "recorded": true }))
}

async fn tool_agent_run_events(args: &Value, backend: Option<&dyn BackendStore>) -> Result<Value> {
    let backend = backend.ok_or_else(|| anyhow::anyhow!("backend store not configured"))?;
    let repo = args["repo"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("repo required"))?;
    let bead_id = args["bead_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("bead_id required"))?;
    let scope = args.get("scope").and_then(|v| v.as_str()).unwrap_or("");

    let bead = WorkRef {
        repo: repo.to_string(),
        scope: scope.to_string(),
        bead_id: bead_id.to_string(),
    };
    let events = backend.agent_run_events_for_bead(&bead).await?;
    Ok(json!({ "count": events.len(), "events": events }))
}

#[derive(Debug, Clone)]
struct AgentSessionAddressAccumulator {
    session_ref: AgentSessionRef,
    bead_ref: WorkRef,
    active: bool,
    dispatch_id: Option<String>,
    agent: Option<String>,
    work_dir: Option<String>,
    dispatch_source: bool,
    event_count: usize,
    latest_event_type: Option<String>,
    latest_event_summary: Option<String>,
    latest_event_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl AgentSessionAddressAccumulator {
    fn new(session_ref: AgentSessionRef, bead_ref: WorkRef) -> Self {
        Self {
            session_ref,
            bead_ref,
            active: false,
            dispatch_id: None,
            agent: None,
            work_dir: None,
            dispatch_source: false,
            event_count: 0,
            latest_event_type: None,
            latest_event_summary: None,
            latest_event_at: None,
        }
    }

    fn absorb_dispatch(&mut self, dispatch: &DispatchRecord) {
        self.active |= dispatch.completed_at.is_none();
        self.dispatch_id = Some(dispatch.id.clone());
        self.agent = Some(dispatch.agent.clone());
        self.work_dir = Some(dispatch.work_dir.clone());
        self.dispatch_source = true;
    }

    fn absorb_event(&mut self, event: &AgentRunEvent) {
        self.event_count += 1;
        if self.dispatch_id.is_none() {
            self.dispatch_id = Some(event.dispatch_id.clone());
        }
        if self
            .latest_event_at
            .map(|existing| event.created_at >= existing)
            .unwrap_or(true)
        {
            self.latest_event_at = Some(event.created_at);
            self.latest_event_type = Some(event.event_type.clone());
            self.latest_event_summary = Some(event.summary.clone());
        }
    }

    fn into_json(self) -> Value {
        let mut sources = Vec::new();
        if self.dispatch_source {
            sources.push("dispatch");
        }
        if self.event_count > 0 {
            sources.push("agent_run_event");
        }

        json!({
            "provider": self.session_ref.provider,
            "id": self.session_ref.id,
            "repo": self.bead_ref.repo,
            "scope": self.bead_ref.scope,
            "bead_id": self.bead_ref.bead_id,
            "active": self.active,
            "dispatch_id": self.dispatch_id,
            "agent": self.agent,
            "work_dir": self.work_dir,
            "event_count": self.event_count,
            "latest_event_type": self.latest_event_type,
            "latest_event_summary": self.latest_event_summary,
            "latest_event_at": self.latest_event_at.map(|t| t.to_rfc3339()),
            "sources": sources,
        })
    }
}

fn dispatch_session_address(dispatch: &DispatchRecord) -> Option<AgentSessionRef> {
    dispatch.session_ref.clone().or_else(|| {
        dispatch
            .session_id
            .as_ref()
            .map(|id| AgentSessionRef::new(dispatch.provider.as_str(), id.as_str()))
    })
}

async fn tool_agent_session_addresses(
    args: &Value,
    backend: Option<&dyn BackendStore>,
) -> Result<Value> {
    let backend = backend.ok_or_else(|| anyhow::anyhow!("backend store not configured"))?;
    let repo = args["repo"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("repo required"))?;
    let bead_id = args["bead_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("bead_id required"))?;
    let scope = args.get("scope").and_then(|v| v.as_str()).unwrap_or("");
    let bead = WorkRef {
        repo: repo.to_string(),
        scope: scope.to_string(),
        bead_id: bead_id.to_string(),
    };

    let mut addresses: BTreeMap<(String, String), AgentSessionAddressAccumulator> = BTreeMap::new();

    for dispatch in backend.dispatches_for_bead(&bead).await? {
        let Some(session_ref) = dispatch_session_address(&dispatch) else {
            continue;
        };
        let key = (session_ref.provider.clone(), session_ref.id.clone());
        addresses
            .entry(key)
            .or_insert_with(|| AgentSessionAddressAccumulator::new(session_ref, bead.clone()))
            .absorb_dispatch(&dispatch);
    }

    for event in backend.agent_run_events_for_bead(&bead).await? {
        let Some(session_ref) = event.session_ref.clone() else {
            continue;
        };
        let key = (session_ref.provider.clone(), session_ref.id.clone());
        addresses
            .entry(key)
            .or_insert_with(|| AgentSessionAddressAccumulator::new(session_ref, bead.clone()))
            .absorb_event(&event);
    }

    let items: Vec<Value> = addresses
        .into_values()
        .map(AgentSessionAddressAccumulator::into_json)
        .collect();

    Ok(json!({ "count": items.len(), "addresses": items }))
}

fn matching_dispatch_id_for_session(
    dispatches: &[DispatchRecord],
    session_ref: &AgentSessionRef,
) -> Option<String> {
    dispatches
        .iter()
        .rev()
        .find(|dispatch| {
            dispatch_session_address(dispatch)
                .as_ref()
                .map(|candidate| candidate == session_ref)
                .unwrap_or(false)
        })
        .map(|dispatch| dispatch.id.clone())
}

async fn tool_agent_session_message_record(
    args: &Value,
    backend: Option<&dyn BackendStore>,
) -> Result<Value> {
    let backend = backend.ok_or_else(|| anyhow::anyhow!("backend store not configured"))?;
    let repo = args["repo"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("repo required"))?;
    let bead_id = args["bead_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("bead_id required"))?;
    let scope = args.get("scope").and_then(|v| v.as_str()).unwrap_or("");
    let session_ref = parse_session_ref_arg(args)?
        .ok_or_else(|| anyhow::anyhow!("session_ref required for addressed message"))?;
    let message = args["message"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("message required"))?;
    if message.trim().is_empty() {
        anyhow::bail!("message must not be blank");
    }
    if message.len() > BODY_MAX_LEN {
        anyhow::bail!(
            "message exceeds {BODY_MAX_LEN} bytes (got {})",
            message.len()
        );
    }

    let bead = WorkRef {
        repo: repo.to_string(),
        scope: scope.to_string(),
        bead_id: bead_id.to_string(),
    };
    let dispatches = backend.dispatches_for_bead(&bead).await?;
    let dispatch_id = match args.get("dispatch_id").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => matching_dispatch_id_for_session(&dispatches, &session_ref).ok_or_else(|| {
            anyhow::anyhow!(
                "no dispatch found for session_ref {}:{} on {repo}/{bead_id}; pass dispatch_id explicitly",
                session_ref.provider,
                session_ref.id,
            )
        })?,
    };

    let mut payload = parse_agent_event_payload(args)?;
    let payload_obj = payload
        .as_object_mut()
        .expect("parse_agent_event_payload returns an object");
    payload_obj.insert("direction".to_string(), json!("outbound"));
    payload_obj.insert("message".to_string(), json!(message));

    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| format!("msg-{}", uuid::Uuid::new_v4()));
    let event_type = args
        .get("event_type")
        .and_then(|v| v.as_str())
        .unwrap_or("handoff_message");
    let event = AgentRunEvent {
        id: id.clone(),
        dispatch_id: dispatch_id.clone(),
        bead_ref: bead,
        session_ref: Some(session_ref),
        event_type: event_type.to_string(),
        summary: message.to_string(),
        payload,
        created_at: parse_agent_event_created_at(args)?,
    };

    backend.record_agent_run_event(&event).await?;
    Ok(json!({
        "id": id,
        "dispatch_id": dispatch_id,
        "bead_id": bead_id,
        "recorded": true,
    }))
}

// ---------------------------------------------------------------------------
// Hierarchy tools (decades, threads, bead membership)
// ---------------------------------------------------------------------------

/// `rsry_decade_create` — dedicated MCP tool for creating decades.
///
/// `tool_thread_assign` already auto-creates decades as a side effect,
/// but agents that want to populate the BDR hierarchy explicitly (file
/// N beads → group into M threads → roll up into 1 decade in one
/// session) need an idempotent dedicated entry point that returns the
/// created record. rosary-992e79.
///
/// Idempotency rule: re-creating with the same title + source_path is
/// a no-op success (`action: "existed"`). Conflict (same id, different
/// title) errors loudly so agents can't accidentally stomp curated
/// decade names.
async fn tool_decade_create(args: &Value, backend: Option<&dyn BackendStore>) -> Result<Value> {
    use crate::store::DecadeRecord;
    let backend = backend.ok_or_else(|| anyhow::anyhow!("backend store not configured"))?;

    let id = args["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("id required (decade slug)"))?;
    let title = args["title"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("title required"))?;
    let source_path = args
        .get("source_path")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let status = args
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("active");

    if let Some(existing) = backend.get_decade(id).await? {
        if existing.title == title && existing.source_path == source_path {
            return Ok(json!({
                "id": existing.id,
                "title": existing.title,
                "source_path": existing.source_path,
                "status": existing.status,
                "action": "existed",
            }));
        }
        // Name the actually-diverging field(s) so the operator can see
        // what changed. Reporting "conflicting title" when only
        // source_path differs misleads readers (Copilot #205 finding).
        let mut diffs = Vec::new();
        if existing.title != title {
            diffs.push(format!(
                "title (`{}` vs requested `{title}`)",
                existing.title
            ));
        }
        if existing.source_path != source_path {
            diffs.push(format!(
                "source_path (`{}` vs requested `{source_path}`)",
                existing.source_path
            ));
        }
        anyhow::bail!(
            "decade `{id}` already exists with conflicting {}; refusing silent overwrite",
            diffs.join(" and ")
        );
    }

    let decade = DecadeRecord {
        id: id.to_string(),
        title: title.to_string(),
        source_path: source_path.to_string(),
        status: status.to_string(),
    };
    backend.upsert_decade(&decade).await?;
    Ok(json!({
        "id": decade.id,
        "title": decade.title,
        "source_path": decade.source_path,
        "status": decade.status,
        "action": "created",
    }))
}

/// `rsry_thread_create` — dedicated MCP tool for creating threads under
/// a named decade. Refuses to land an orphan thread if the parent
/// decade doesn't exist (unlike `thread_assign`, which auto-creates an
/// `ungrouped` decade as a fall-through). rosary-992e79.
async fn tool_thread_create(args: &Value, backend: Option<&dyn BackendStore>) -> Result<Value> {
    use crate::store::ThreadRecord;
    let backend = backend.ok_or_else(|| anyhow::anyhow!("backend store not configured"))?;

    let decade_id = args["decade_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("decade_id required (parent decade)"))?;
    let id = args["id"].as_str().ok_or_else(|| {
        anyhow::anyhow!("id required (thread slug, conventionally `<decade>/<name>`)")
    })?;
    let name = args["name"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("name required (human-readable thread title)"))?;

    if backend.get_decade(decade_id).await?.is_none() {
        anyhow::bail!(
            "parent decade `{decade_id}` does not exist; create it first via rsry_decade_create"
        );
    }

    // Thread IDs are globally unique — the `threads` table primary key is
    // just `id`, not `(decade_id, id)`. Scan every decade for an existing
    // thread with this id; otherwise a same-id thread under a different
    // decade would silently get re-parented by upsert (Copilot #205 finding).
    let mut existing: Option<crate::store::ThreadRecord> = None;
    for decade in backend.list_decades(None).await? {
        if let Some(t) = backend
            .list_threads(&decade.id)
            .await?
            .into_iter()
            .find(|t| t.id == id)
        {
            existing = Some(t);
            break;
        }
    }
    if let Some(existing) = existing {
        if existing.name == name && existing.decade_id == decade_id {
            return Ok(json!({
                "id": existing.id,
                "name": existing.name,
                "decade_id": existing.decade_id,
                "feature_branch": existing.feature_branch,
                "action": "existed",
            }));
        }
        if existing.decade_id != decade_id {
            anyhow::bail!(
                "thread `{id}` already exists under decade `{}` (requested `{decade_id}`); thread ids are globally unique — re-parent via rsry_thread_reparent instead",
                existing.decade_id
            );
        }
        anyhow::bail!(
            "thread `{id}` already exists under decade `{decade_id}` with a conflicting name (`{}` vs requested `{name}`)",
            existing.name
        );
    }

    let prefix = crate::config::load_global()
        .ok()
        .and_then(|c| c.github)
        .map(|g| g.agent_branch_prefix)
        .unwrap_or_else(|| "rosary".to_string());
    let feature_branch = crate::workspace::thread_branch_name(&prefix, name);

    let thread = ThreadRecord {
        id: id.to_string(),
        name: name.to_string(),
        decade_id: decade_id.to_string(),
        feature_branch: Some(feature_branch.clone()),
    };
    backend.upsert_thread(&thread).await?;

    Ok(json!({
        "id": thread.id,
        "name": thread.name,
        "decade_id": thread.decade_id,
        "feature_branch": feature_branch,
        "action": "created",
    }))
}

async fn tool_decade_list(args: &Value, backend: Option<&dyn BackendStore>) -> Result<Value> {
    let backend = backend.ok_or_else(|| anyhow::anyhow!("backend store not configured"))?;
    let status = args.get("status").and_then(|v| v.as_str());

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

async fn tool_thread_list(args: &Value, backend: Option<&dyn BackendStore>) -> Result<Value> {
    let backend = backend.ok_or_else(|| anyhow::anyhow!("backend store not configured"))?;

    // Option 1: list threads for a decade
    if let Some(decade_id) = args.get("decade_id").and_then(|v| v.as_str()) {
        let threads = backend.list_threads(decade_id).await?;
        let mut items: Vec<Value> = Vec::with_capacity(threads.len());
        for t in &threads {
            // Include members — thread membership lives in the orchestrator
            // lattice, not the per-repo bead store, so rsry_bead_search can't
            // surface it. Without this, every thread renders as an empty shell
            // and the hierarchy looks skipped even when it's fully populated.
            let bead_ids: Vec<String> = backend
                .list_beads_in_thread(&t.id)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|b| b.bead_id)
                .collect();
            items.push(json!({
                "id": t.id,
                "name": t.name,
                "decade_id": t.decade_id,
                "feature_branch": t.feature_branch,
                "bead_count": bead_ids.len(),
                "beads": bead_ids,
            }));
        }
        return Ok(json!({ "count": items.len(), "threads": items }));
    }

    // Option 2: find thread for a specific bead
    if let (Some(bead_id), Some(repo)) = (
        args.get("bead_id").and_then(|v| v.as_str()),
        args.get("repo").and_then(|v| v.as_str()),
    ) {
        let bead_ref = crate::store::WorkRef {
            repo: repo.to_string(),
            scope: String::new(),
            bead_id: bead_id.to_string(),
        };
        let thread_id = backend.find_thread_for_bead(&bead_ref).await?;
        return Ok(json!({ "bead_id": bead_id, "thread_id": thread_id }));
    }

    anyhow::bail!("provide either decade_id or (bead_id + repo)")
}

async fn tool_thread_assign(args: &Value, backend: Option<&dyn BackendStore>) -> Result<Value> {
    let backend = backend.ok_or_else(|| anyhow::anyhow!("backend store not configured"))?;
    use crate::store::{ThreadRecord, WorkRef};

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

    // Only create the thread when it's genuinely new. thread_assign assigns a
    // BEAD — it must NEVER re-upsert an existing thread, which would clobber the
    // decade_id a prior thread_create set (decade_id here defaults to
    // "ungrouped" when the caller doesn't re-pass it). (rosary-427446)
    if backend.get_thread(thread_id).await?.is_none() {
        use crate::store::DecadeRecord;
        // Auto-create the parent decade if it doesn't exist.
        if backend.get_decade(decade_id).await?.is_none() {
            backend
                .upsert_decade(&DecadeRecord {
                    id: decade_id.to_string(),
                    title: decade_id.to_string(),
                    source_path: String::new(),
                    status: "active".to_string(),
                })
                .await?;
        }

        // Derive feature branch from config prefix + thread name.
        let prefix = crate::config::load_global()
            .ok()
            .and_then(|c| c.github)
            .map(|g| g.agent_branch_prefix)
            .unwrap_or_else(|| "rosary".to_string());
        let feature_branch = crate::workspace::thread_branch_name(&prefix, thread_name);

        backend
            .upsert_thread(&ThreadRecord {
                id: thread_id.to_string(),
                name: thread_name.to_string(),
                decade_id: decade_id.to_string(),
                feature_branch: Some(feature_branch),
            })
            .await?;
    }

    let bead_ref = WorkRef {
        repo: repo.to_string(),
        scope: String::new(),
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
// Bead import (cross-instance migration)
// ---------------------------------------------------------------------------

async fn tool_bead_import(
    args: &Value,
    config_path: &str,
    pool: &RepoPool,
    user_scope: Option<&str>,
) -> Result<Value> {
    let default_repo_path = args.get("repo_path").and_then(|v| v.as_str());
    let beads = args["beads"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("beads array required"))?;

    // Build repo name → path lookup from config for routing by repo name
    let cfg = config::load_merged(config_path)?;
    let repo_paths: std::collections::HashMap<String, String> = cfg
        .repo
        .iter()
        .map(|r| {
            let path = crate::scanner::expand_path(&r.path);
            (r.name.clone(), path.to_string_lossy().to_string())
        })
        .collect();

    let mut imported = 0;
    let mut skipped = 0;
    let mut errors = Vec::new();
    let mut ids = Vec::new();

    for bead in beads {
        let title = bead["title"].as_str().unwrap_or("(untitled)");

        // Resolve target repo: per-bead "repo" field, then fallback to repo_path param
        let resolved_repo_path = bead
            .get("repo")
            .and_then(|v| v.as_str())
            .and_then(|name| repo_paths.get(name).map(|s| s.as_str()))
            .or(default_repo_path);

        let repo_path = match resolved_repo_path {
            Some(p) => p,
            None => {
                errors.push(format!(
                    "no repo for bead '{title}' — set repo field or repo_path param"
                ));
                continue;
            }
        };

        let client_ref = get_client(repo_path, pool).await?;
        let client = client_ref.as_store();
        let repo_name = repo_name_from_path(repo_path);

        match crate::import::import_bead(bead, client, &repo_name).await? {
            Some(id) => {
                if let Some(uid) = user_scope {
                    let _ = client.set_user_id(&id, uid).await;
                }
                ids.push(id);
                imported += 1;
            }
            None => skipped += 1,
        }
    }

    let mut result = json!({
        "imported": imported,
        "skipped": skipped,
        "ids": ids,
    });
    if !errors.is_empty() {
        result["errors"] = json!(errors);
    }
    Ok(result)
}

/// Re-parent an existing thread under a different decade.
///
/// Used to clean up the bead lattice when threads land in `ungrouped` or
/// `auto-discovered` and should belong to a real decade. Calls `upsert_thread`
/// with the new `decade_id`. Auto-creates the target decade if missing.
async fn tool_thread_reparent(args: &Value, backend: Option<&dyn BackendStore>) -> Result<Value> {
    let backend = backend.ok_or_else(|| anyhow::anyhow!("backend store not configured"))?;

    let thread_id = args["thread_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("thread_id required"))?;
    let decade_id = args["decade_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("decade_id required"))?;
    let new_name = args.get("name").and_then(|v| v.as_str());

    crate::store::reparent_thread(backend, thread_id, decade_id, new_name).await?;

    Ok(serde_json::json!({
        "thread_id": thread_id,
        "decade_id": decade_id,
        "reparented": true,
    }))
}

// ---------------------------------------------------------------------------
// User repo registration (multi-tenant)
// ---------------------------------------------------------------------------

async fn tool_repo_register(
    args: &Value,
    backend: Option<&dyn BackendStore>,
    user_scope: Option<&str>,
) -> Result<Value> {
    let backend = backend.ok_or_else(|| {
        anyhow::anyhow!(
            "backend store not configured — add [backend] section to ~/.rsry/config.toml"
        )
    })?;
    let user_id = user_scope.ok_or_else(|| {
        anyhow::anyhow!("repo registration requires user identity (connect via mcp.rosary.bot)")
    })?;

    let repo_url = args["repo_url"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("repo_url required"))?;

    // Derive repo_name from URL if not provided
    let repo_name = args
        .get("repo_name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            repo_url
                .trim_end_matches('/')
                .trim_end_matches(".git")
                .rsplit('/')
                .next()
                .unwrap_or("repo")
                .to_string()
        });

    use crate::store::UserRepo;
    let repo = UserRepo {
        user_id: user_id.to_string(),
        repo_url: repo_url.to_string(),
        repo_name: repo_name.clone(),
        github_token_ref: None, // TODO: accept token ref from dashboard settings
    };

    backend.register_repo(&repo).await?;

    Ok(json!({
        "user_id": user_id,
        "repo_name": repo_name,
        "repo_url": repo_url,
        "registered": true,
    }))
}

async fn tool_repo_list(
    backend: Option<&dyn BackendStore>,
    user_scope: Option<&str>,
) -> Result<Value> {
    let backend = backend.ok_or_else(|| anyhow::anyhow!("backend store not configured"))?;
    let user_id =
        user_scope.ok_or_else(|| anyhow::anyhow!("repo listing requires user identity"))?;

    let repos = backend.list_user_repos(user_id).await?;

    Ok(json!({
        "user_id": user_id,
        "count": repos.len(),
        "repos": repos.iter().map(|r| json!({
            "repo_name": r.repo_name,
            "repo_url": r.repo_url,
        })).collect::<Vec<_>>(),
    }))
}

// ---------------------------------------------------------------------------
// rsry_ticket_load — Phase 0 of rosary-5d7141 (rosary-5dc9b0)
// ---------------------------------------------------------------------------

/// Compose the agent-native review panel for a bead (summary, comments,
/// workspace state, change-set, evidence rollup) into one MCP response.
/// Phase 0 of rosary-ccd5a2 (`rsry review` substrate). The real composition
/// lives in `serve::review::collect_review_for_bead`; this thin orchestrator
/// validates args, resolves the repo's bead store, and forwards.
async fn tool_review(args: &Value, backend: Option<&dyn BackendStore>) -> Result<Value> {
    let bead_id = args
        .get("bead_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("bead_id is required (e.g. \"rosary-cd5d2a\")"))?;
    let repo_path = args
        .get("repo_path")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "repo_path is required (Phase 0 — scope→path resolution lands in a follow-up)"
            )
        })?;

    let root = crate::scanner::resolve_repo_path(std::path::Path::new(repo_path));
    let beads_dir = crate::resolve_beads_dir(&root);
    let store = crate::bead_sqlite::connect_bead_store(&beads_dir)
        .await
        .with_context(|| format!("connecting to bead store at {}", beads_dir.display()))?;
    // Derive repo_name from the CANONICAL root, not the raw arg. Matches
    // the CLI's behavior in `main.rs` (Command::Bead) and prevents
    // `bead.repo == subdir_basename` when callers pass a subdirectory or a
    // trailing-slash path. (Copilot review on PR #220.)
    let repo_name = root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".into());

    let event_scope = args.get("scope").and_then(|v| v.as_str()).unwrap_or("");
    let agent_run_events = match backend {
        Some(backend) => {
            let bead = WorkRef {
                repo: repo_name.clone(),
                bead_id: bead_id.to_string(),
                scope: event_scope.to_string(),
            };
            backend.agent_run_events_for_bead(&bead).await?
        }
        None => vec![],
    };

    super::review::collect_review_for_bead(
        store.as_ref(),
        &repo_name,
        &root,
        bead_id,
        agent_run_events,
    )
    .await
}

/// Consolidate Linear + (linked GH/Zendesk URLs) + existing-bead context for
/// a single ticket into one MCP response. Replaces the 4-5 manual lookups the
/// user performs per escalation. See `serve::ticket_load` for the pure-fn
/// helpers; this orchestrator does the Linear I/O and stitches the pieces.
async fn tool_ticket_load(args: &Value, pool: &RepoPool) -> Result<Value> {
    use super::ticket_load::{
        assemble_context, extract_github_link, extract_zendesk_link, find_triage_bead,
    };

    let ticket_id = args
        .get("ticket_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("ticket_id is required (e.g. \"CUS-495\")"))?;

    let Some(client) = crate::linear::try_client() else {
        anyhow::bail!(
            "Linear not configured — set LINEAR_API_KEY or [linear].api_key in ~/.rsry/config.toml"
        );
    };

    let linear_issue = crate::linear::get_ticket(&client, ticket_id)
        .await
        .with_context(|| format!("fetching Linear ticket {ticket_id}"))?;

    // Linear's `id` is the internal UUID required for the comments query.
    // Propagate comments-fetch errors instead of swallowing them — a transient
    // Linear/GraphQL failure here would otherwise produce a successful-looking
    // response with an empty `comments` array, which is indistinguishable from
    // "the ticket has no comments" and silently degrades URL discovery + bead
    // matching that depend on comment text. (Copilot review on PR #215.)
    let comments = match linear_issue["id"].as_str() {
        Some(internal_id) => crate::linear::get_ticket_comments(&client, internal_id)
            .await
            .with_context(|| format!("fetching comments for Linear ticket {ticket_id}"))?,
        None => Vec::new(),
    };

    // Concatenate body + comments for URL extraction so a GH link anywhere in
    // the conversation is discoverable, not just in the ticket body.
    let body_text = linear_issue["description"].as_str().unwrap_or("");
    let combined_text: String = std::iter::once(body_text.to_string())
        .chain(
            comments
                .iter()
                .filter_map(|c| c["body"].as_str().map(String::from)),
        )
        .collect::<Vec<_>>()
        .join("\n");

    let linked_github = extract_github_link(&combined_text).map(|url| {
        // Phase 0: URL only. The body + state fields wait on a follow-up bead
        // (fetching GH issue bodies via `gh api` adds a new I/O dep).
        json!({ "url": url, "body": null, "state": null })
    });
    let linked_zendesk = extract_zendesk_link(&combined_text);
    // Bead lookup uses the canonical Linear identifier (`CUS-495`) returned by
    // `get_ticket`, NOT the raw caller-supplied `ticket_id` — which may be a
    // full URL. Existing beads consistently reference the identifier form, so
    // this normalization is what makes URL-shaped inputs match real beads.
    // (Copilot review on PR #215.)
    let lookup_id = linear_issue["identifier"].as_str().unwrap_or(ticket_id);
    let existing_bead = find_triage_bead(pool, lookup_id).await;

    Ok(assemble_context(
        linear_issue,
        comments,
        linked_github,
        linked_zendesk,
        existing_bead,
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod input_validation_tests;
#[cfg(test)]
mod tests;
