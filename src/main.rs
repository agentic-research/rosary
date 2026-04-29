#![recursion_limit = "256"]
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

mod acp;
#[allow(dead_code)] // API surface — wired in rsry-e608bb (reconciler integration)
mod backend;
mod bdr_enrich;
mod bead;
mod bead_dolt;
mod bead_sqlite;
mod capture;
mod cli;
mod config;
mod decompose;
mod dispatch;
mod dolt;
#[allow(dead_code)] // API surface for PM agent (loom-w8c.4); is_dominated_by used by reconciler
mod epic;
#[allow(dead_code)] // API surface — PR creation from dispatch pipeline
mod github;
mod github_mirror;
#[allow(dead_code)] // API surface — wired into pipeline phase transitions
mod handoff;
mod import;
mod linear;
#[allow(dead_code)]
mod linear_tracker;
#[allow(dead_code)] // API surface — consumed by orchestrator after dispatch
mod manifest;
#[allow(dead_code)]
mod migrate;
mod notes;
mod orchestrate;
mod pipeline;
mod plugin;
mod pool;
mod queue;
mod reconcile;
mod repo_cache;
mod scan_assay;
mod scanner;
mod secrets;
mod serve;
mod session;
#[allow(dead_code)] // API surface — wired in rsry-e599fb (SpritesProvider)
mod sprites;
#[allow(dead_code)] // API surface — wired in rsry-e608bb (reconciler integration)
mod sprites_provider;
#[allow(dead_code)] // Phase 1: traits + impl, wired in Phase 2
mod store;
#[allow(dead_code)] // Phase 1: Dolt backend, wired in Phase 2
mod store_dolt;
#[allow(dead_code)] // Phase 1: SQLite backend, wired alongside Dolt
mod store_sqlite;
#[allow(dead_code)]
mod sync;
#[cfg(test)]
mod testutil;
mod vcs;
mod verify;
#[allow(dead_code)] // API surface — replaces dispatch.rs worktree logic
mod workspace;
mod xref;

#[derive(Parser)]
#[command(
    name = "rsry",
    about = "Strings beads, repos, and review layers into coordinated work",
    version = concat!(
        env!("CARGO_PKG_VERSION"),
        " (",
        env!("RSRY_BUILD_HASH"),
        " ",
        env!("RSRY_BUILD_TIME"),
        ")"
    ),
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scan repos for issues, create beads (bottom-up discovery)
    Scan {
        /// Config file listing repos to scan
        #[arg(short, long, default_value = "rosary.toml")]
        config: String,
        /// Filter to specific repos (comma-separated)
        #[arg(long)]
        repo: Option<String>,
        /// Run assay.scan plugins and file P3 chore beads for stale refs
        #[arg(long)]
        assay: bool,
    },
    /// Capture beads from a session transcript or code provenance
    Capture {
        /// Read transcript from this path and extract beads via LLM ("-" for stdin)
        #[arg(long, value_name = "PATH")]
        from_session: Option<String>,
        /// LLM model for atom extraction (haiku, sonnet, or full model ID)
        #[arg(long, default_value = "haiku")]
        model: String,
        /// Repo path to write beads into (with --commit)
        #[arg(short, long, default_value = ".")]
        repo: String,
        /// Write proposed beads to .beads/ instead of printing to stdout
        #[arg(long)]
        commit: bool,
    },
    /// Decompose a Linear ticket into repo-scoped beads (top-down planning)
    Plan {
        /// Linear ticket ID or URL
        ticket: String,
    },
    /// Bidirectional sync: beads ↔ Linear status
    Sync {
        /// Preview changes without executing
        #[arg(long)]
        dry_run: bool,
        /// Filter to specific repos (comma-separated)
        #[arg(long)]
        repo: Option<String>,
        /// Mirror bead context to linked GitHub PRs/issues as structured comments
        #[arg(long)]
        github: bool,
    },
    /// Show aggregated status across all repos
    Status {
        /// Filter to specific repos (comma-separated)
        #[arg(long)]
        repo: Option<String>,
        /// Output as JSON (for scripts/statusline)
        #[arg(long)]
        json: bool,
    },
    /// Dispatch a bead to a Claude Code agent in an isolated worktree
    Dispatch {
        /// Bead ID to work on
        bead_id: String,
        /// Repo path containing .beads/
        #[arg(short, long, default_value = ".")]
        repo: String,
        /// Use isolated jj workspace
        #[arg(long, default_value_t = true)]
        isolate: bool,
    },
    /// Run the reconciliation loop (scan → triage → dispatch → verify → report)
    Run {
        /// Config file listing repos
        #[arg(short, long, default_value = "rosary.toml")]
        config: String,
        /// Max concurrent Claude Code agents
        #[arg(long, default_value_t = 3)]
        concurrency: usize,
        /// Seconds between scan iterations
        #[arg(long, default_value_t = 30)]
        interval: u64,
        /// Single pass (no loop)
        #[arg(long)]
        once: bool,
        /// Print what would be dispatched without actually spawning agents
        #[arg(long)]
        dry_run: bool,
        /// AI provider to use for dispatch (claude, gemini)
        #[arg(long, default_value = "claude")]
        provider: String,
        /// Overnight mode: prefer small/mechanical beads, concurrency=1, interval=120s
        #[arg(long)]
        overnight: bool,
        /// Target a specific bead (skip triage, dispatch only this bead)
        #[arg(long)]
        bead: Option<String>,
    },
    /// Start the reconciliation daemon in the background
    Start {
        /// Config file listing repos
        #[arg(short, long, default_value = "rosary.toml")]
        config: String,
        /// Max concurrent agents
        #[arg(long, default_value_t = 3)]
        concurrency: usize,
        /// Seconds between scan iterations
        #[arg(long, default_value_t = 30)]
        interval: u64,
        /// AI provider (claude, gemini)
        #[arg(long, default_value = "claude")]
        provider: String,
        /// Overnight mode
        #[arg(long)]
        overnight: bool,
    },
    /// Stop the running daemon
    Stop,
    /// Tail the daemon log
    Logs,
    /// Start MCP server exposing rosary as tools
    Serve {
        /// Transport: stdio or http
        #[arg(long, default_value = "stdio")]
        transport: String,
        /// Port for HTTP transport
        #[arg(long, default_value = "8383")]
        port: u16,
    },
    /// Register current repo (or path) in the global registry (~/.rsry/repos.toml)
    Enable {
        /// Path to repo root (defaults to current directory)
        #[arg(default_value = ".")]
        path: String,
    },
    /// Unregister a repo from the global registry by name or path
    Disable {
        /// Repo name or path to remove
        name_or_path: String,
    },
    /// Decompose a markdown document (ADR, README, etc.) into beads
    Decompose {
        /// Path to the markdown file
        path: String,
        /// Title for the decade (defaults to first heading)
        #[arg(short, long)]
        title: Option<String>,
        /// Repo path to create beads in
        #[arg(short, long, default_value = ".")]
        repo: String,
        /// Preview without creating beads
        #[arg(long)]
        dry_run: bool,
        /// LLM model for non-ADR docs (haiku, sonnet, or full model ID).
        /// When set and the document is not ADR-shaped, uses the Anthropic
        /// Messages API to extract atoms instead of the heuristic parser.
        /// Requires ANTHROPIC_API_KEY env var.
        #[arg(long)]
        model: Option<String>,
        /// Emit code stubs into this repo path instead of (or in addition to)
        /// creating beads. Generates `.rsry-stubs/<decade>.rs` from
        /// TechnicalSpec and Constraint atoms. Review the stub PR before
        /// implementing to validate the design.
        #[arg(long)]
        stub_output: Option<String>,
    },
    /// Manage beads directly
    Bead {
        #[command(subcommand)]
        action: BeadAction,
        /// Repo path containing .beads/
        #[arg(short, long, default_value = ".")]
        repo: String,
    },
    /// Garbage-collect merged agent branches from origin
    Sweep {
        /// Repo path (defaults to current directory)
        #[arg(short, long, default_value = ".")]
        repo: String,
        /// Preview what would be deleted without making changes
        #[arg(long)]
        dry_run: bool,
    },
    /// Export orchestrator backend state to JSON backup
    Backup {
        /// Output directory (default: ~/.rsry/backups/<timestamp>)
        #[arg(short, long)]
        output: Option<String>,
    },
    /// Migrate orchestrator backend between providers (e.g. dolt → sqlite)
    Migrate {
        /// Target provider: "sqlite" or "dolt"
        #[arg(long)]
        to: String,
        /// Target path (default: ~/.rsry/backend.db for sqlite)
        #[arg(long)]
        path: Option<String>,
        /// Skip post-migration verification
        #[arg(long)]
        skip_verify: bool,
    },
    /// Capture design atoms from a session transcript or source file
    Capture {
        /// Read transcript file (use `-` for stdin)
        #[arg(long, conflicts_with = "from_code")]
        from_session: Option<String>,
        /// Read source file: `<repo> <path>` (e.g. `rosary src/bead.rs`)
        #[arg(long, num_args = 2, conflicts_with = "from_session")]
        from_code: Vec<String>,
        /// Symbol to scope code capture (e.g. `BeadSpec`)
        #[arg(long)]
        symbol: Option<String>,
        /// LLM model: haiku (default), sonnet, or full model ID
        #[arg(long, default_value = "haiku")]
        model: String,
        /// Repo path for --commit (default: current directory)
        #[arg(short, long, default_value = ".")]
        repo: String,
        /// Write extracted BeadSpecs as beads (default: dry-run to stdout)
        #[arg(long)]
        commit: bool,
    },
    /// Manage encrypted notes (age-encrypted, scope-organized)
    Notes {
        #[command(subcommand)]
        action: NotesAction,
        /// Repo path containing `notes/`
        #[arg(short, long, default_value = ".")]
        repo: String,
    },
}

#[derive(Subcommand)]
enum NotesAction {
    /// Re-encrypt all notes in a scope after editing the recipient list
    Rotate {
        /// Scope name (becomes `notes/<scope>/`)
        #[arg(long)]
        scope: String,
        /// Recipient(s) to add (repeatable)
        #[arg(long = "add-recipient", value_name = "RECIPIENT")]
        add: Vec<String>,
        /// Recipient(s) to remove (repeatable)
        #[arg(long = "remove-recipient", value_name = "RECIPIENT")]
        remove: Vec<String>,
        /// Identity file for decryption (default: $HOME/.config/age/keys.txt)
        #[arg(long)]
        identity: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum BeadAction {
    /// Create a new bead
    Create {
        /// Bead title
        title: String,
        /// Description
        #[arg(short, long, default_value = "")]
        description: String,
        /// Priority (0=P0 highest, 3=P3 lowest)
        #[arg(short, long, default_value_t = 2)]
        priority: u8,
        /// Issue type
        #[arg(short = 't', long, default_value = "task")]
        issue_type: String,
        /// Source files this bead touches (comma-separated)
        #[arg(short, long, value_delimiter = ',')]
        files: Vec<String>,
        /// Test files to validate the change (comma-separated)
        #[arg(long, value_delimiter = ',')]
        test_files: Vec<String>,
    },
    /// Close a bead
    Close {
        /// Bead ID
        id: String,
    },
    /// List open beads
    List,
    /// Add a comment to a bead
    Comment {
        /// Bead ID
        id: String,
        /// Comment body
        body: String,
    },
    /// Search beads by title/description
    Search {
        /// Search query
        query: String,
    },
    /// Export beads as JSON (for import into another rsry instance)
    Export {
        /// Filter by status (open, blocked, all). Default: open
        #[arg(short, long, default_value = "open")]
        status: String,
    },
    /// Import beads from a JSON file or stdin
    Import {
        /// JSON file path (reads stdin if omitted)
        file: Option<String>,
    },
}

/// Generate a bead ID: `{prefix}-{lower 6 hex chars of millis}` (~16M values before collision).
pub fn generate_bead_id(prefix: &str) -> String {
    let millis = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("{prefix}-{:06x}", millis & 0xffffff)
}

/// Capture git username from `git config user.name` at bead creation time.
/// Returns None if git is not available or user.name is not set.
/// Imprecise (self-reported) but Good Enough for team attribution.
/// Build an enriched bead description from a `BeadSpec`, appending structured
/// success criteria, cross-references, and provenance as markdown sections.
fn enrich_bead_description(spec: &bdr::decompose::BeadSpec) -> String {
    let mut desc = spec.description.clone();

    if !spec.success_criteria.is_empty() {
        desc.push_str("\n\n## Success Criteria\n\n");
        for sc in &spec.success_criteria {
            match (&sc.command, &sc.threshold) {
                (Some(cmd), _) => {
                    desc.push_str(&format!("- `{cmd}` — {}\n", sc.description));
                }
                (None, Some(threshold)) => {
                    desc.push_str(&format!("- {} (threshold: {threshold})\n", sc.description));
                }
                (None, None) => {
                    desc.push_str(&format!("- {}\n", sc.description));
                }
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

fn git_config_user_name(repo_root: &Path) -> Option<String> {
    std::process::Command::new("git")
        .args(["config", "user.name"])
        .current_dir(repo_root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Resolve the .beads/ directory for a repo, handling git/jj worktrees.
/// In a worktree, .beads/ lives in the main worktree — resolve via git commondir.
pub fn resolve_beads_dir(repo_root: &Path) -> PathBuf {
    if repo_root.join(".beads").exists() {
        return repo_root.join(".beads");
    }
    // Try to find the main worktree's .beads/ via git commondir
    let git_common = std::process::Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(repo_root)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| PathBuf::from(s.trim()));
    if let Some(common) = git_common {
        let main_root = common.parent().unwrap_or(repo_root);
        main_root.join(".beads")
    } else {
        repo_root.join(".beads")
    }
}

fn daemon_pid_path() -> PathBuf {
    dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".rsry")
        .join("rsry.pid")
}

fn daemon_log_path() -> PathBuf {
    dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".rsry")
        .join("rsry.log")
}

fn read_daemon_pid() -> Option<u32> {
    let path = daemon_pid_path();
    let content = std::fs::read_to_string(&path).ok()?;
    let pid: u32 = content.trim().parse().ok()?;
    // Check if process is alive via kill -0
    let status = std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;
    if status.success() {
        Some(pid)
    } else {
        let _ = std::fs::remove_file(&path);
        None
    }
}

/// Resolve config path: if user passed "rosary.toml" (default), check global first.
fn resolve_config(config: &str) -> String {
    if config == "rosary.toml" {
        config::resolve_config_path()
    } else {
        config.to_string()
    }
}

/// Parse a comma-separated repo filter into a set of repo names.
fn parse_repo_filter(filter: &Option<String>) -> Option<Vec<String>> {
    filter.as_ref().map(|f| {
        f.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    })
}

/// Filter repo configs to only those matching the filter.
fn filter_repos(
    repos: &[config::RepoConfig],
    filter: &Option<Vec<String>>,
) -> Vec<config::RepoConfig> {
    match filter {
        Some(names) => repos
            .iter()
            .filter(|r| names.contains(&r.name))
            .cloned()
            .collect(),
        None => repos.to_vec(),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Scan {
            config,
            repo,
            assay,
        } => {
            let cfg = config::load_merged(&resolve_config(&config))?;
            let repo_filter = parse_repo_filter(&repo);
            let repos = filter_repos(&cfg.repo, &repo_filter);

            if assay {
                let all_plugins: Vec<_> = cfg
                    .plugins
                    .iter()
                    .cloned()
                    .chain(config::discover_plugins(None))
                    .collect();
                let registry = plugin::PluginRegistry::new(all_plugins);
                let n = scan_assay::run_assay_scan(&repos, &registry).await?;
                eprintln!("[assay] filed {n} chore bead(s) for stale refs");
            } else {
                let beads = scanner::scan_repos(&repos).await?;
                cli::scan_summary(&beads);
            }
        }
        Command::Capture {
            from_session,
            model,
            repo,
            commit,
        } => {
            let path = from_session
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("specify --from-session <path>"))?;

            let opts = capture::SessionCaptureOpts {
                transcript_path: path,
                model: &model,
            };
            let specs = capture::capture_from_session(&opts).await?;

            if commit {
                let repo_root = scanner::resolve_repo_path(Path::new(&repo));
                let beads_dir = resolve_beads_dir(&repo_root);
                let client = bead_sqlite::connect_bead_store(&beads_dir).await?;
                let repo_name = repo_root
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| repo.clone());
                let created_by = git_config_user_name(&repo_root);
                let mut n = 0u32;
                for spec in &specs {
                    let id = generate_bead_id(&repo_name);
                    let owner = dispatch::default_agent(&spec.issue_type);
                    let desc = enrich_bead_description(spec);
                    client
                        .create_bead_full(
                            &id,
                            &spec.title,
                            &desc,
                            spec.priority,
                            &spec.issue_type,
                            owner,
                            &[],
                            &[],
                            &[],
                            created_by.as_deref(),
                            "",
                            &spec.derived_from,
                        )
                        .await?;
                    eprintln!("[capture] created {id}: {}", spec.title);
                    n += 1;
                }
                eprintln!("[capture] wrote {n} bead(s) to {}", repo_root.display());
            } else {
                println!("{}", serde_json::to_string_pretty(&specs)?);
            }
        }
        Command::Plan { ticket } => {
            linear::plan(&ticket).await?;
        }
        Command::Sync {
            dry_run,
            repo,
            github,
        } => {
            let repo_filter = parse_repo_filter(&repo);

            if github {
                let cfg = config::load_merged(&config::resolve_config_path())?;
                let repos = filter_repos(&cfg.repo, &repo_filter);
                let beads = scanner::scan_repos(&repos).await?;
                let token = cfg
                    .github
                    .as_ref()
                    .and_then(|g| g.token.clone())
                    .or_else(|| std::env::var("GITHUB_TOKEN").ok())
                    .context("GITHUB_TOKEN not set and no github.token in config")?;
                let posted = github_mirror::sync_beads_to_github(&beads, &token).await?;
                println!("github: posted {posted} bead-context comment(s)");
                return Ok(());
            }

            // Connect hierarchy store for thread → sub-issue projection
            let sync_cfg = config::load_merged(&config::resolve_config_path())?;
            let hierarchy: Option<Box<dyn store::HierarchyStore>> =
                if let Some(ref backend_cfg) = sync_cfg.backend {
                    match backend_cfg.connect().await {
                        Ok(b) => Some(b as Box<dyn store::HierarchyStore>),
                        Err(e) => {
                            eprintln!("[sync] hierarchy unavailable ({e}), no sub-issue grouping");
                            None
                        }
                    }
                } else {
                    None
                };
            linear::sync(dry_run, repo_filter.as_deref(), hierarchy.as_deref()).await?;
        }
        Command::Status { repo, json } => {
            let cfg = config::load_merged(&config::resolve_config_path())?;
            let repo_filter = parse_repo_filter(&repo);
            let repos = filter_repos(&cfg.repo, &repo_filter);
            let beads = scanner::scan_repos(&repos).await?;
            if json {
                let count = |status: &[&str]| {
                    beads
                        .iter()
                        .filter(|b| status.contains(&b.status.as_str()))
                        .count()
                };
                let open = count(&["open"]);
                let in_progress = count(&["dispatched", "in_progress"]);
                let blocked = count(&["blocked"]);
                let done = count(&["done", "closed"]);

                // Per-repo breakdown
                let mut per_repo = std::collections::BTreeMap::new();
                for bead in &beads {
                    let entry = per_repo.entry(bead.repo.clone()).or_insert_with(
                        || serde_json::json!({"open": 0, "in_progress": 0, "blocked": 0}),
                    );
                    match bead.status.as_str() {
                        "open" => entry["open"] = json!(entry["open"].as_u64().unwrap_or(0) + 1),
                        "dispatched" | "in_progress" => {
                            entry["in_progress"] =
                                json!(entry["in_progress"].as_u64().unwrap_or(0) + 1)
                        }
                        "blocked" => {
                            entry["blocked"] = json!(entry["blocked"].as_u64().unwrap_or(0) + 1)
                        }
                        _ => {}
                    }
                }

                println!(
                    "{}",
                    serde_json::json!({
                        "total": beads.len(),
                        "open": open,
                        "in_progress": in_progress,
                        "blocked": blocked,
                        "done": done,
                        "repos": per_repo,
                    })
                );
            } else {
                cli::print_status_summary(&beads);
                cli::print_ready_beads(&beads, 10);
            }
        }
        Command::Dispatch {
            bead_id,
            repo,
            isolate,
        } => {
            dispatch::run(&bead_id, std::path::Path::new(&repo), isolate).await?;
        }
        Command::Run {
            config,
            concurrency,
            interval,
            once,
            dry_run,
            provider,
            overnight,
            bead,
        } => {
            // --overnight sets defaults, but explicit --concurrency/--interval override
            let concurrency = if overnight && concurrency == 3 {
                1
            } else {
                concurrency
            };
            let interval = if overnight && interval == 30 {
                120
            } else {
                interval
            };
            reconcile::run(
                &resolve_config(&config),
                concurrency,
                interval,
                once,
                dry_run,
                &provider,
                overnight,
                bead.as_deref(),
            )
            .await?;
        }
        Command::Start {
            config,
            concurrency,
            interval,
            provider,
            overnight,
        } => {
            if let Some(pid) = read_daemon_pid() {
                cli::daemon_already_running(pid);
                return Ok(());
            }

            let log_path = daemon_log_path();
            if let Some(parent) = log_path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            let mut args = vec![
                "run".to_string(),
                "--config".to_string(),
                resolve_config(&config),
                "--concurrency".to_string(),
                concurrency.to_string(),
                "--interval".to_string(),
                interval.to_string(),
                "--provider".to_string(),
                provider,
            ];
            if overnight {
                args.push("--overnight".to_string());
            }

            let log_file = std::fs::File::create(&log_path)?;
            // nosemgrep: blocking-subprocess-in-async — .spawn() returns immediately (non-blocking)
            let child = std::process::Command::new(std::env::current_exe()?)
                .args(&args)
                .stdout(log_file.try_clone()?)
                .stderr(log_file)
                .stdin(std::process::Stdio::null())
                .spawn()?;

            let pid = child.id();
            std::fs::write(daemon_pid_path(), pid.to_string())?;
            cli::daemon_started(pid, &log_path.to_string_lossy());
        }
        Command::Stop => {
            if let Some(pid) = read_daemon_pid() {
                unsafe {
                    libc::kill(pid as i32, libc::SIGTERM);
                }
                let _ = std::fs::remove_file(daemon_pid_path());
                cli::daemon_stopped(pid);
            } else {
                println!("No daemon running");
            }
        }
        Command::Logs => {
            let log_path = daemon_log_path();
            if log_path.exists() {
                // nosemgrep: blocking-subprocess-in-async — intentionally blocking: interactive tail -f
                let status = std::process::Command::new("tail")
                    .args(["-f", &log_path.to_string_lossy()])
                    .status()?;
                std::process::exit(status.code().unwrap_or(1));
            } else {
                println!("No log file at {}", log_path.display());
            }
        }
        Command::Serve { transport, port } => {
            serve::run(&transport, port).await?;
        }
        Command::Enable { path } => {
            let entry = config::enable_repo(Path::new(&path))?;
            // Init .beads/ Dolt DB if not present
            if !entry.path.join(".beads").exists() {
                dolt::init_beads_db(&entry.path).await?;
            }
            cli::repo_enabled(&entry.name, &entry.path.to_string_lossy());
        }
        Command::Disable { name_or_path } => match config::disable_repo(&name_or_path)? {
            Some(name) => cli::repo_disabled(&name),
            None => println!("Not found: {name_or_path}"),
        },
        Command::Decompose {
            path,
            title,
            repo,
            dry_run,
            model,
            stub_output,
        } => {
            let markdown =
                std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?;

            // Route: ADR-shaped docs use the heuristic parser.
            // Non-ADR docs with --model set use LLM extraction.
            let (atoms, meta) = if model.is_some() && !bdr::parse::is_adr_shaped(&markdown) {
                let model_name = model.as_deref().unwrap();
                let atoms = bdr_enrich::extract_atoms_with_llm(&markdown, model_name).await?;
                let meta = bdr::parse::DocMeta {
                    provenance: Some(bdr::provenance::ProvenanceRef::Doc { path: path.clone() }),
                    ..Default::default()
                };
                (atoms, meta)
            } else {
                let parsed = bdr::parse::parse_doc_full(&markdown, &path);
                (parsed.atoms, parsed.meta)
            };

            if atoms.is_empty() {
                println!("No decomposable atoms found in {path}");
                return Ok(());
            }

            let adr_title = title.unwrap_or_else(|| {
                markdown
                    .lines()
                    .find(|l: &&str| l.starts_with("# "))
                    .map(|l: &str| l.trim_start_matches('#').trim().to_string())
                    .unwrap_or_else(|| path.clone())
            });

            let mut decade = bdr::thread::build_decade_with_meta(&path, &adr_title, &atoms, &meta);

            // When LLM extraction was used, stamp inferred_from on every BeadSpec.
            if let Some(ref model_name) = model {
                let trace = bdr::provenance::InferenceTrace {
                    model: bdr_enrich::resolve_model_id(model_name).to_string(),
                    rationale: None,
                };
                for thread in &mut decade.threads {
                    for spec in &mut thread.beads {
                        spec.inferred_from = Some(trace.clone());
                    }
                }
            }

            cli::decompose_decade(
                &decade.title,
                &decade.id,
                &format!("{:?}", decade.status),
                decade.threads.len(),
            );
            for thread in &decade.threads {
                cli::decompose_thread(&thread.name, thread.beads.len());
                for bead_spec in &thread.beads {
                    cli::decompose_bead(
                        &bead_spec.channel.to_string(),
                        &bead_spec.title,
                        &bead_spec.issue_type,
                        bead_spec.priority,
                    );
                }
                if !thread.cross_repo_refs.is_empty() {
                    cli::decompose_refs(&thread.cross_repo_refs);
                }
            }

            if !dry_run {
                let repo_root = scanner::resolve_repo_path(Path::new(&repo));
                let beads_dir = repo_root.join(".beads");
                let client = bead_sqlite::connect_bead_store(&beads_dir).await?;
                let decompose_repo_name = repo_root
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| repo.clone());
                let created_by = git_config_user_name(&repo_root);

                // Connect to backend store for thread/decade assignment (best-effort).
                let backend: Option<Box<dyn store::BackendStore>> =
                    match config::load_global().ok().and_then(|c| c.backend) {
                        Some(cfg) => cfg.connect().await.ok(),
                        None => None,
                    };

                // Seed decade + threads in the backend so beads land in the lattice.
                if let Some(ref b) = backend {
                    let _ = b
                        .upsert_decade(&store::DecadeRecord {
                            id: decade.id.clone(),
                            title: decade.title.clone(),
                            source_path: path.clone(),
                            status: "active".to_string(),
                        })
                        .await;
                    for thread in &decade.threads {
                        let prefix = config::load_global()
                            .ok()
                            .and_then(|c| c.github)
                            .map(|g| g.agent_branch_prefix)
                            .unwrap_or_else(|| "rosary".to_string());
                        let feature_branch = workspace::thread_branch_name(&prefix, &thread.name);
                        let _ = b
                            .upsert_thread(&store::ThreadRecord {
                                id: thread.id.clone(),
                                name: thread.name.clone(),
                                decade_id: decade.id.clone(),
                                feature_branch: Some(feature_branch),
                            })
                            .await;
                    }
                }

                let mut created = 0;
                let mut skipped = 0;
                for thread in &decade.threads {
                    for spec in &thread.beads {
                        // Dedup: skip if a bead with the exact same title already exists.
                        let existing = client
                            .search_beads(&spec.title, &decompose_repo_name, 10)
                            .await
                            .unwrap_or_default();
                        if existing.iter().any(|b| b.title == spec.title) {
                            eprintln!("  [skip] '{}' — already exists", spec.title);
                            skipped += 1;
                            continue;
                        }

                        // Cross-repo routing: warn when target differs from --repo.
                        if let Some(ref target) = spec.target_repo
                            && target != &decompose_repo_name
                        {
                            eprintln!(
                                "  [route] '{}' → suggested repo: {target} \
                                 (creating in {decompose_repo_name})",
                                spec.title
                            );
                        }

                        // Enrich description with success criteria and references.
                        let desc = enrich_bead_description(spec);

                        let id = generate_bead_id(&decompose_repo_name);
                        let owner = dispatch::default_agent(&spec.issue_type);
                        client
                            .create_bead_full(
                                &id,
                                &spec.title,
                                &desc,
                                spec.priority,
                                &spec.issue_type,
                                owner,
                                &[], // file scopes set by code-reader agent post-dispatch
                                &[],
                                &[], // depends_on: ADR-level refs can't map to bead IDs yet
                                created_by.as_deref(),
                                "",
                                &spec.derived_from,
                            )
                            .await?;

                        // Assign to thread in backend lattice.
                        if let Some(ref b) = backend {
                            let _ = b
                                .add_bead_to_thread(
                                    &thread.id,
                                    &store::WorkRef {
                                        repo: decompose_repo_name.clone(),
                                        bead_id: id.clone(),
                                        scope: String::new(),
                                    },
                                )
                                .await;
                        }

                        created += 1;
                    }
                }
                if skipped > 0 {
                    eprintln!("  [dedup] skipped {skipped} already-existing beads");
                }
                cli::decompose_summary(created, &repo_root.to_string_lossy());
            } else {
                println!();
                println!(
                    "  {}",
                    owo_colors::OwoColorize::dimmed(&"(dry run — no beads created)")
                );
            }

            // Stub output: emit code skeletons for design review.
            if let Some(stub_repo) = stub_output {
                let target = scanner::resolve_repo_path(Path::new(&stub_repo));
                match decompose::write_stubs(&target, &decade.title, &atoms)? {
                    Some(stub_path) => {
                        println!("  stub output → {}", stub_path.display());
                        println!("  hint: commit, push, and open a draft PR for design review");
                    }
                    None => {
                        println!("  (no TechnicalSpec or Constraint atoms — nothing to stub)");
                    }
                }
            }
        }
        Command::Capture {
            from_session,
            from_code,
            symbol,
            model,
            repo,
            commit,
        } => {
            let specs = if let Some(ref transcript) = from_session {
                let opts = capture::SessionCaptureOpts {
                    transcript_path: transcript,
                    model: &model,
                };
                capture::capture_from_session(&opts).await?
            } else if from_code.len() == 2 {
                let repo_root = scanner::resolve_repo_path(Path::new(&repo));
                let opts = capture::CodeCaptureOpts {
                    repo: &from_code[0],
                    path: &from_code[1],
                    symbol: symbol.as_deref(),
                    model: &model,
                    repo_root: &repo_root,
                };
                capture::capture_from_code(&opts).await?
            } else {
                anyhow::bail!("use --from-session <path> or --from-code <repo> <path>");
            };

            if !commit {
                println!("{}", serde_json::to_string_pretty(&specs)?);
            } else {
                let repo_root = scanner::resolve_repo_path(Path::new(&repo));
                let beads_dir = resolve_beads_dir(&repo_root);
                let client = bead_sqlite::connect_bead_store(&beads_dir).await?;
                let repo_name = repo_root
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| repo.clone());
                let created_by = git_config_user_name(&repo_root);
                let mut created = 0;
                for spec in &specs {
                    let desc = enrich_bead_description(spec);
                    let id = generate_bead_id(&repo_name);
                    let owner = dispatch::default_agent(&spec.issue_type);
                    client
                        .create_bead_full(
                            &id,
                            &spec.title,
                            &desc,
                            spec.priority,
                            &spec.issue_type,
                            owner,
                            &[],
                            &[],
                            &[],
                            created_by.as_deref(),
                            "",
                            &spec.derived_from,
                        )
                        .await?;
                    created += 1;
                }
                cli::decompose_summary(created, &repo_root.to_string_lossy());
            }
        }
        Command::Bead { action, repo } => {
            let repo_root = scanner::resolve_repo_path(Path::new(&repo));
            let beads_dir = resolve_beads_dir(&repo_root);
            let client = bead_sqlite::connect_bead_store(&beads_dir).await?;
            let repo_name = repo_root
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| repo.clone());

            match action {
                BeadAction::Create {
                    title,
                    description,
                    priority,
                    issue_type,
                    files,
                    test_files,
                } => {
                    if bead::requires_files(&issue_type) && files.is_empty() {
                        anyhow::bail!(
                            "files required for {issue_type} beads — specify which code this bead touches"
                        );
                    }
                    let id = generate_bead_id(&repo_name);
                    let owner = dispatch::default_agent(&issue_type);
                    let created_by = git_config_user_name(&repo_root);
                    client
                        .create_bead_full(
                            &id,
                            &title,
                            &description,
                            priority,
                            &issue_type,
                            owner,
                            &files,
                            &test_files,
                            &[], // CLI doesn't support depends_on yet
                            created_by.as_deref(),
                            "",
                            &[],
                        )
                        .await?;
                    cli::bead_created(&id, &title);
                }
                BeadAction::Close { id } => {
                    client.close_bead(&id).await?;
                    cli::bead_closed(&id);
                }
                BeadAction::List => {
                    let beads = client.list_beads(&repo_name).await?;
                    cli::bead_list(&beads);
                }
                BeadAction::Comment { id, body } => {
                    client.add_comment(&id, &body, "rsry-cli").await?;
                    cli::bead_commented(&id);
                }
                BeadAction::Search { query } => {
                    let beads = client.search_beads(&query, &repo_name, 50).await?;
                    cli::bead_search_results(&beads, &query);
                }
                BeadAction::Export { status } => {
                    let beads = client.list_beads(&repo_name).await?;
                    let filtered: Vec<_> = match status.as_str() {
                        "all" => beads,
                        "blocked" => beads.into_iter().filter(|b| b.is_blocked()).collect(),
                        s => beads.into_iter().filter(|b| b.status == s).collect(),
                    };
                    let export = import::export_beads_json(&filtered);
                    println!("{}", serde_json::to_string_pretty(&export)?);
                }
                BeadAction::Import { file } => {
                    let beads_json = import::read_beads_json(file)?;
                    let r = import::import_beads(&beads_json, &*client, &repo_name).await?;
                    println!(
                        "Imported {}, skipped {} (duplicate titles)",
                        r.imported, r.skipped
                    );
                }
            }
        }
        Command::Sweep { repo, dry_run } => {
            let repo_path = scanner::resolve_repo_path(std::path::Path::new(&repo));
            let result = workspace::sweep_agent_branches(&repo_path, dry_run).await;
            if dry_run {
                println!(
                    "dry-run: {} checked, {} would delete, {} skipped (active), {} skipped (unmerged)",
                    result.checked, result.deleted, result.skipped_active, result.skipped_unmerged
                );
            } else {
                println!(
                    "sweep: {} checked, {} deleted, {} skipped (active), {} skipped (unmerged)",
                    result.checked, result.deleted, result.skipped_active, result.skipped_unmerged
                );
            }
        }
        Command::Backup { output } => {
            let cfg = config::load_merged(&config::resolve_config_path())?;
            let backend_cfg = cfg
                .backend
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("[backend] section missing from config"))?;
            let source = backend_cfg.connect_exportable().await?;
            let snapshot = migrate::export_snapshot(&*source, &backend_cfg.provider).await?;
            let rsry_dir = config::rsry_dir();
            let dir = output.unwrap_or_else(|| {
                rsry_dir
                    .join("backups")
                    .join(chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string())
                    .to_string_lossy()
                    .into_owned()
            });
            migrate::save_backup(&snapshot, std::path::Path::new(&dir))?;
            let counts = snapshot.counts();
            eprintln!("Backup saved to {dir}");
            eprintln!(
                "  decades={} threads={} members={} pipelines={} dispatches={} deps={} links={} repos={}",
                counts.decades,
                counts.threads,
                counts.thread_members,
                counts.pipelines,
                counts.dispatches,
                counts.dependencies,
                counts.linear_links,
                counts.user_repos
            );
        }
        Command::Migrate {
            to,
            path,
            skip_verify,
        } => {
            let cfg = config::load_merged(&config::resolve_config_path())?;
            let backend_cfg = cfg
                .backend
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("[backend] section missing from config"))?;
            let source = backend_cfg.connect_exportable().await?;

            let rsry_dir = config::rsry_dir();
            let target_path = path.unwrap_or_else(|| match to.as_str() {
                "sqlite" => rsry_dir.join("backend.db").to_string_lossy().into_owned(),
                _ => rsry_dir.join("dolt/rosary").to_string_lossy().into_owned(),
            });
            let target_cfg = config::BackendConfig {
                provider: to.clone(),
                path: target_path.clone().into(),
            };
            let target = target_cfg.connect_or_create().await?;

            // Auto-backup before migration
            let backup_dir = rsry_dir
                .join("backups")
                .join(format!(
                    "pre-migrate-{}",
                    chrono::Utc::now().format("%Y%m%d-%H%M%S")
                ))
                .to_string_lossy()
                .into_owned();

            eprintln!("Migrating {} → {} ...", backend_cfg.provider, to);
            let report = migrate::migrate(
                &*source,
                &*target,
                &backend_cfg.provider,
                Some(std::path::Path::new(&backup_dir)),
            )
            .await?;

            eprintln!("Source: {:?}", report.source_counts);
            eprintln!("Target: {:?}", report.target_counts);

            if skip_verify {
                eprintln!("Verification: SKIPPED (--skip-verify)");
                eprintln!();
                eprintln!("To switch, edit ~/.rsry/config.toml:");
                eprintln!("  [backend]");
                eprintln!("  provider = \"{}\"", to);
                eprintln!("  path = \"{}\"", target_path);
            } else if report.verified {
                eprintln!("Verification: PASSED");
                eprintln!();
                eprintln!("To switch, edit ~/.rsry/config.toml:");
                eprintln!("  [backend]");
                eprintln!("  provider = \"{}\"", to);
                eprintln!("  path = \"{}\"", target_path);
            } else {
                eprintln!("Verification: FAILED — counts mismatch!");
                eprintln!("Backup at: {backup_dir}");
                std::process::exit(1);
            }
        }
        Command::Notes { action, repo } => {
            let repo_root = scanner::resolve_repo_path(Path::new(&repo));
            match action {
                NotesAction::Rotate {
                    scope,
                    add,
                    remove,
                    identity,
                } => {
                    let opts = notes::RotateOpts {
                        repo_root: &repo_root,
                        scope: &scope,
                        add_recipients: &add,
                        remove_recipients: &remove,
                        identity: identity.as_deref(),
                    };
                    let result = notes::rotate_scope(&opts).await?;
                    println!(
                        "rotated {} file(s) in notes/{} (recipients: {})",
                        result.files_rotated,
                        scope,
                        result.final_recipients.len()
                    );
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bead_action_variants_construct() {
        // Verify each BeadAction variant can be constructed with expected fields
        let create = BeadAction::Create {
            title: "Fix the widget".to_string(),
            description: "It is broken".to_string(),
            priority: 1,
            issue_type: "bug".to_string(),
            files: vec!["src/widget.rs".to_string()],
            test_files: vec![],
        };
        assert!(matches!(create, BeadAction::Create { priority: 1, .. }));

        let close = BeadAction::Close {
            id: "rsry-abc".to_string(),
        };
        assert!(matches!(close, BeadAction::Close { .. }));

        let list = BeadAction::List;
        assert!(matches!(list, BeadAction::List));

        let comment = BeadAction::Comment {
            id: "rsry-abc".to_string(),
            body: "looking into this".to_string(),
        };
        assert!(matches!(comment, BeadAction::Comment { .. }));
    }

    #[test]
    fn generate_bead_id_uses_repo_prefix() {
        let id = generate_bead_id("mache");
        assert!(
            id.starts_with("mache-"),
            "id should start with 'mache-': {id}"
        );
        // Suffix must be exactly 6 hex characters
        let suffix = &id["mache-".len()..];
        assert_eq!(suffix.len(), 6, "suffix should be 6 chars: {suffix}");
        assert!(
            suffix.chars().all(|c| c.is_ascii_hexdigit()),
            "suffix should be hex: {suffix}"
        );
    }

    #[test]
    fn generate_bead_id_different_repos() {
        let id1 = generate_bead_id("rosary");
        let id2 = generate_bead_id("mache");
        assert!(id1.starts_with("rosary-"));
        assert!(id2.starts_with("mache-"));
    }
}
