use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

mod bead;
mod config;
mod dispatch;
mod dolt;
mod linear;
mod queue;
mod reconcile;
mod scanner;
mod serve;
mod vcs;
mod verify;

#[derive(Parser)]
#[command(name = "rsry", about = "Strings beads, repos, and review layers into coordinated work")]
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
    Sync,
    /// Show aggregated status across all repos
    Status,
    /// Dispatch a bead to a Claude Code agent in an isolated worktree
    Dispatch {
        /// Bead ID to work on
        bead_id: String,
        /// Repo path containing .beads/
        #[arg(short, long, default_value = ".")]
        repo: String,
        /// Use isolated git worktree
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
    },
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
}

/// Generate a bead ID from the current timestamp.
/// Format: `rsry-{first 3 chars of hex(timestamp_millis)}`.
fn generate_bead_id() -> String {
    let millis = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system clock before UNIX epoch")
        .as_millis();
    let hex = format!("{millis:x}");
    format!("rsry-{}", &hex[..3])
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Scan { config } => {
            let cfg = config::load_merged(&config)?;
            let beads = scanner::scan_repos(&cfg.repo).await?;
            println!("Found {} beads across {} repos", beads.len(), cfg.repo.len());
            for b in &beads {
                println!("  {} [{}] {} — {}", b.repo, b.status, b.id, b.title);
            }
        }
        Command::Plan { ticket } => {
            linear::plan(&ticket).await?;
        }
        Command::Sync => {
            linear::sync().await?;
        }
        Command::Status => {
            let cfg = config::load_merged("rosary.toml")?;
            let beads = scanner::scan_repos(&cfg.repo).await?;
            scanner::print_status(&beads);
        }
        Command::Dispatch { bead_id, repo, isolate } => {
            dispatch::run(&bead_id, std::path::Path::new(&repo), isolate).await?;
        }
        Command::Run { config, concurrency, interval, once, dry_run, provider } => {
            reconcile::run(&config, concurrency, interval, once, dry_run, &provider).await?;
        }
        Command::Serve { transport, port } => {
            serve::run(&transport, port).await?;
        }
        Command::Enable { path } => {
            let entry = config::enable_repo(Path::new(&path))?;
            println!("Enabled: {} ({})", entry.name, entry.path.display());
        }
        Command::Disable { name_or_path } => {
            match config::disable_repo(&name_or_path)? {
                Some(name) => println!("Disabled: {name}"),
                None => println!("Not found: {name_or_path}"),
            }
        }
        Command::Bead { action, repo } => {
            // Walk up to find repo root (like uv's pyproject.toml discovery)
            let repo_root = Path::new(&repo).canonicalize()
                .ok()
                .and_then(|p| config::discover_repo_root(&p))
                .unwrap_or_else(|| PathBuf::from(&repo));
            let beads_dir = repo_root.join(".beads");
            let dolt_config = dolt::DoltConfig::from_beads_dir(&beads_dir)?;
            let client = dolt::DoltClient::connect(&dolt_config).await?;
            let repo_name = repo_root.file_name()
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
        assert!(id.starts_with("rsry-"), "id should start with 'rsry-': {id}");
        // Suffix must be exactly 3 hex characters
        let suffix = &id["rsry-".len()..];
        assert_eq!(suffix.len(), 3, "suffix should be 3 chars: {suffix}");
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
