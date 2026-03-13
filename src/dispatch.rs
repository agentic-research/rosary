use anyhow::Result;
use std::process::Command;

use crate::dolt::{DoltClient, DoltConfig};
use crate::scanner::expand_path;

/// Dispatch a bead to a Claude Code agent for execution.
///
/// Reads bead details via native MySQL to Dolt, builds a prompt,
/// optionally creates a git worktree, and spawns Claude Code.
pub async fn run(bead_id: &str, repo_path: &std::path::Path, isolate: bool) -> Result<()> {
    let path = expand_path(repo_path);
    let beads_dir = path.join(".beads");

    // Step 1: Read the bead details via Dolt
    let config = DoltConfig::from_beads_dir(&beads_dir)?;
    let client = DoltClient::connect(&config).await?;

    let bead = client
        .get_bead(bead_id, &path.display().to_string())
        .await?
        .ok_or_else(|| anyhow::anyhow!("bead {bead_id} not found"))?;

    // Step 2: Build the prompt
    let prompt = format!(
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
    );

    // Step 3: Optionally create worktree
    let branch_name = format!("fix/{bead_id}");
    let work_dir = if isolate {
        let worktree_path = path.join(format!("../{branch_name}"));
        let worktree_result = Command::new("git")
            .args(["worktree", "add", "-b", &branch_name, &worktree_path.to_string_lossy()])
            .current_dir(&path)
            .output();

        match worktree_result {
            Ok(output) if output.status.success() => {
                println!("Created worktree: {}", worktree_path.display());
                worktree_path
            }
            _ => {
                eprintln!("warning: worktree creation failed, running in-place");
                path.clone()
            }
        }
    } else {
        path.clone()
    };

    // Step 4: Spawn Claude Code
    println!("Dispatching {bead_id} to Claude Code...");

    // TODO: actually spawn claude CLI
    // Command::new("claude")
    //     .args(["--print", &prompt])
    //     .current_dir(&work_dir)
    //     .status()?;
    let _ = (&prompt, &work_dir); // suppress unused warnings until spawn is wired

    // Step 5: Update bead status
    client.update_status(bead_id, "in_progress").await?;

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
}
