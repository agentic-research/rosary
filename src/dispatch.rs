//! Dispatch beads to Claude Code agents for execution.
//!
//! Two entry points:
//! - `run()`: Original blocking dispatch (reads Dolt, spawns agent, waits).
//! - `spawn()`: Async dispatch returning an `AgentHandle` for the reconciliation loop.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::bead::Bead;
use crate::dolt::{DoltClient, DoltConfig};
use crate::scanner::expand_path;

/// Handle to a running Claude Code agent process.
pub struct AgentHandle {
    pub bead_id: String,
    pub generation: u64,
    pub child: tokio::process::Child,
    pub work_dir: PathBuf,
    pub started_at: chrono::DateTime<chrono::Utc>,
}

impl AgentHandle {
    /// Check if the agent process has exited (non-blocking).
    pub fn try_wait(&mut self) -> Result<Option<std::process::ExitStatus>> {
        Ok(self.child.try_wait()?)
    }

    /// Wait for the agent to complete.
    pub async fn wait(&mut self) -> Result<std::process::ExitStatus> {
        Ok(self.child.wait().await?)
    }

    /// Kill the agent process.
    pub fn kill(&mut self) -> Result<()> {
        self.child.start_kill()?;
        Ok(())
    }

    /// Elapsed time since dispatch.
    pub fn elapsed(&self) -> chrono::Duration {
        chrono::Utc::now() - self.started_at
    }
}

/// Build the prompt for a bead.
pub fn build_prompt(bead: &Bead) -> String {
    format!(
        "Fix this issue. Make the minimal change needed, scoped to a single file.\n\
         \n\
         Title: {}\n\
         Description: {}\n\
         \n\
         After fixing:\n\
         1. Run tests to verify\n\
         2. Create a commit with a descriptive message\n\
         3. Report what you changed",
        bead.title, bead.description
    )
}

/// Create a git worktree for isolated work. Returns the worktree path on success.
async fn create_worktree(
    repo_path: &Path,
    bead_id: &str,
) -> Result<PathBuf, ()> {
    let branch_name = format!("fix/{bead_id}");
    let worktree_path = repo_path.join(format!("../{branch_name}"));

    let output = tokio::process::Command::new("git")
        .args([
            "worktree",
            "add",
            "-b",
            &branch_name,
            &worktree_path.to_string_lossy(),
        ])
        .current_dir(repo_path)
        .output()
        .await;

    match output {
        Ok(o) if o.status.success() => {
            println!("Created worktree: {}", worktree_path.display());
            Ok(worktree_path)
        }
        _ => Err(()),
    }
}

/// Spawn a Claude Code agent for a bead. Returns a handle without waiting.
///
/// This is the async entry point for the reconciliation loop.
pub async fn spawn(
    bead: &Bead,
    repo_path: &Path,
    isolate: bool,
    generation: u64,
) -> Result<AgentHandle> {
    let path = expand_path(repo_path);
    let prompt = build_prompt(bead);

    let work_dir = if isolate {
        create_worktree(&path, &bead.id)
            .await
            .unwrap_or_else(|()| {
                eprintln!("warning: worktree creation failed, running in-place");
                path.clone()
            })
    } else {
        path.clone()
    };

    println!("Dispatching {} to Claude Code...", bead.id);

    let child = tokio::process::Command::new("claude")
        .args(["--print", &prompt])
        .current_dir(&work_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning claude CLI for {}", bead.id))?;

    Ok(AgentHandle {
        bead_id: bead.id.clone(),
        generation,
        child,
        work_dir,
        started_at: chrono::Utc::now(),
    })
}

/// Original blocking dispatch — reads Dolt, spawns agent, waits for completion.
/// Kept for `loom dispatch` CLI command.
pub async fn run(bead_id: &str, repo_path: &Path, isolate: bool) -> Result<()> {
    let path = expand_path(repo_path);
    let beads_dir = path.join(".beads");

    let config = DoltConfig::from_beads_dir(&beads_dir)?;
    let client = DoltClient::connect(&config).await?;

    let bead = client
        .get_bead(bead_id, &path.display().to_string())
        .await?
        .ok_or_else(|| anyhow::anyhow!("bead {bead_id} not found"))?;

    client.update_status(bead_id, "dispatched").await?;

    let mut handle = spawn(&bead, &path, isolate, bead.generation()).await?;
    let status = handle.wait().await?;

    if status.success() {
        println!("Claude Code completed successfully for {bead_id}");
    } else {
        eprintln!("warning: claude exited with {status} for {bead_id}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn dispatch_missing_beads_dir_errors() {
        let dir = TempDir::new().unwrap();
        let result = run("fake-id", dir.path(), false).await;
        assert!(result.is_err());
    }

    #[test]
    fn build_prompt_includes_title_and_description() {
        let bead = Bead {
            id: "test-1".into(),
            title: "Fix the widget".into(),
            description: "The widget is broken".into(),
            status: "open".into(),
            priority: 1,
            issue_type: "bug".into(),
            owner: None,
            repo: "test".into(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            dependency_count: 0,
            dependent_count: 0,
            comment_count: 0,
        };

        let prompt = build_prompt(&bead);
        assert!(prompt.contains("Fix the widget"));
        assert!(prompt.contains("The widget is broken"));
        assert!(prompt.contains("Run tests to verify"));
    }
}
