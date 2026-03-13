use anyhow::Result;
use clap::{Parser, Subcommand};

mod bead;
mod config;
mod dispatch;
mod dolt;
mod linear;
mod queue;
mod reconcile;
mod scanner;
mod verify;

#[derive(Parser)]
#[command(name = "loom", about = "Weaves beads, repos, and review layers into coordinated work")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scan repos for issues, create beads (bottom-up discovery)
    Scan {
        /// Config file listing repos to scan
        #[arg(short, long, default_value = "loom.toml")]
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
        #[arg(short, long, default_value = "loom.toml")]
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
    },
    /// Start MCP server exposing loom as tools
    Serve {
        /// Transport: stdio or http
        #[arg(long, default_value = "stdio")]
        transport: String,
        /// Port for HTTP transport
        #[arg(long, default_value = "8383")]
        port: u16,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Scan { config } => {
            let cfg = config::load(&config)?;
            let beads = scanner::scan_repos(&cfg.repos).await?;
            println!("Found {} beads across {} repos", beads.len(), cfg.repos.len());
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
            let cfg = config::load("loom.toml")?;
            let beads = scanner::scan_repos(&cfg.repos).await?;
            scanner::print_status(&beads);
        }
        Command::Dispatch { bead_id, repo, isolate } => {
            dispatch::run(&bead_id, std::path::Path::new(&repo), isolate).await?;
        }
        Command::Run { config, concurrency, interval, once, dry_run } => {
            reconcile::run(&config, concurrency, interval, once, dry_run).await?;
        }
        Command::Serve { transport, port } => {
            todo!("MCP server on {transport}:{port}")
        }
    }

    Ok(())
}
