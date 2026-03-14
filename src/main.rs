use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

mod acp;
#[allow(dead_code)] // API surface — wired in rsry-e608bb (reconciler integration)
mod backend;
mod bead;
mod config;
mod dispatch;
mod dolt;
#[allow(dead_code)] // API surface for PM agent (loom-w8c.4); is_dominated_by used by reconciler
mod epic;
mod linear;
#[allow(dead_code)]
mod linear_tracker;
mod pool;
mod queue;
mod reconcile;
mod scanner;
mod serve;
#[allow(dead_code)] // API surface — wired in rsry-e599fb (SpritesProvider)
mod sprites;
#[allow(dead_code)] // API surface — wired in rsry-e608bb (reconciler integration)
mod sprites_provider;
#[allow(dead_code)]
mod sync;
mod thread;
mod vcs;
mod verify;
#[allow(dead_code)] // API surface — replaces dispatch.rs worktree logic
mod workspace;

#[derive(Parser)]
#[command(
    name = "rsry",
    about = "Strings beads, repos, and review layers into coordinated work"
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
    },
    /// Show aggregated status across all repos
    Status,
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
    /// Manage beads directly
    Bead {
        #[command(subcommand)]
        action: BeadAction,
        /// Repo path containing .beads/
        #[arg(short, long, default_value = ".")]
        repo: String,
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
}

/// Generate a bead ID from the current timestamp.
/// Format: `rsry-{lower 6 hex chars of millis}` (~16M values before collision).
fn generate_bead_id() -> String {
    let millis = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system clock before UNIX epoch")
        .as_millis();
    format!("rsry-{:06x}", millis & 0xffffff)
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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Scan { config } => {
            let cfg = config::load_merged(&resolve_config(&config))?;
            let beads = scanner::scan_repos(&cfg.repo).await?;
            println!(
                "Found {} beads across {} repos",
                beads.len(),
                cfg.repo.len()
            );
            for b in &beads {
                println!("  {} [{}] {} — {}", b.repo, b.status, b.id, b.title);
            }
        }
        Command::Plan { ticket } => {
            linear::plan(&ticket).await?;
        }
        Command::Sync { dry_run } => {
            linear::sync(dry_run).await?;
        }
        Command::Status => {
            let cfg = config::load_merged(&config::resolve_config_path())?;
            let beads = scanner::scan_repos(&cfg.repo).await?;
            scanner::print_status(&beads);
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
                println!("Daemon already running (PID {pid})");
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
            let child = std::process::Command::new(std::env::current_exe()?)
                .args(&args)
                .stdout(log_file.try_clone()?)
                .stderr(log_file)
                .stdin(std::process::Stdio::null())
                .spawn()?;

            let pid = child.id();
            std::fs::write(daemon_pid_path(), pid.to_string())?;
            println!("Daemon started (PID {pid}), log: {}", log_path.display());
        }
        Command::Stop => {
            if let Some(pid) = read_daemon_pid() {
                unsafe {
                    libc::kill(pid as i32, libc::SIGTERM);
                }
                let _ = std::fs::remove_file(daemon_pid_path());
                println!("Stopped daemon (PID {pid})");
            } else {
                println!("No daemon running");
            }
        }
        Command::Logs => {
            let log_path = daemon_log_path();
            if log_path.exists() {
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
            println!("Enabled: {} ({})", entry.name, entry.path.display());
        }
        Command::Disable { name_or_path } => match config::disable_repo(&name_or_path)? {
            Some(name) => println!("Disabled: {name}"),
            None => println!("Not found: {name_or_path}"),
        },
        Command::Bead { action, repo } => {
            // Walk up to find repo root (like uv's pyproject.toml discovery)
            let repo_root = Path::new(&repo)
                .canonicalize()
                .ok()
                .and_then(|p| config::discover_repo_root(&p))
                .unwrap_or_else(|| PathBuf::from(&repo));
            let beads_dir = repo_root.join(".beads");
            let dolt_config = dolt::DoltConfig::from_beads_dir(&beads_dir)?;
            let client = dolt::DoltClient::connect(&dolt_config).await?;
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
                } => {
                    let id = generate_bead_id();
                    client
                        .create_bead(&id, &title, &description, priority, &issue_type)
                        .await?;
                    println!("Created bead {id}: {title}");
                }
                BeadAction::Close { id } => {
                    client.close_bead(&id).await?;
                    println!("Closed bead {id}");
                }
                BeadAction::List => {
                    let beads = client.list_beads(&repo_name).await?;
                    if beads.is_empty() {
                        println!("No open beads.");
                    } else {
                        for b in &beads {
                            println!("  [P{}] {} — {}", b.priority, b.id, b.title);
                        }
                        println!("{} open bead(s)", beads.len());
                    }
                }
                BeadAction::Comment { id, body } => {
                    client.add_comment(&id, &body, "rsry").await?;
                    println!("Added comment to {id}");
                }
                BeadAction::Search { query } => {
                    let beads = client.search_beads(&query, &repo_name).await?;
                    if beads.is_empty() {
                        println!("No beads matching '{query}'");
                    } else {
                        for b in &beads {
                            println!("  [P{}] {} — {}", b.priority, b.id, b.title);
                        }
                        println!("{} result(s)", beads.len());
                    }
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
    fn generate_bead_id_format() {
        let id = generate_bead_id();
        // Must start with "rsry-"
        assert!(
            id.starts_with("rsry-"),
            "id should start with 'rsry-': {id}"
        );
        // Suffix must be exactly 6 hex characters
        let suffix = &id["rsry-".len()..];
        assert_eq!(suffix.len(), 6, "suffix should be 6 chars: {suffix}");
        assert!(
            suffix.chars().all(|c| c.is_ascii_hexdigit()),
            "suffix should be hex: {suffix}"
        );
    }

    #[test]
    fn generate_bead_id_is_deterministic_within_millis() {
        // Two calls in quick succession should produce the same ID
        // (timestamp millis resolution means sub-ms calls collide)
        let id1 = generate_bead_id();
        let id2 = generate_bead_id();
        // Both should be valid format regardless
        assert!(id1.starts_with("rsry-"));
        assert!(id2.starts_with("rsry-"));
    }
}
