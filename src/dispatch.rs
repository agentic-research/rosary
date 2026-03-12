use anyhow::{Context, Result};
use std::process::Command;

/// Dispatch a bead to a Claude Code agent for execution.
///
/// Spawns `claude` CLI as a subprocess with the bead context.
/// If `isolate` is true, creates a git worktree for the work.
pub async fn run(bead_id: &str, isolate: bool) -> Result<()> {
    // Step 1: Read the bead details
    let bead_json = Command::new("bd")
        .args(["show", bead_id, "--json"])
        .output()
        .with_context(|| format!("reading bead {bead_id}"))?;

    if !bead_json.status.success() {
        anyhow::bail!(
            "bead {bead_id} not found: {}",
            String::from_utf8_lossy(&bead_json.stderr)
        );
    }

    let bead: serde_json::Value = serde_json::from_slice(&bead_json.stdout)?;
    let title = bead["title"].as_str().unwrap_or(bead_id);
    let description = bead["description"].as_str().unwrap_or("");

    // Step 2: Build the prompt
    let prompt = format!(
        "Fix this issue. Make the minimal change needed, scoped to a single file.\n\
         \n\
         Title: {title}\n\
         Description: {description}\n\
         \n\
         After fixing:\n\
         1. Run tests to verify\n\
         2. Create a commit with a descriptive message\n\
         3. Report what you changed"
    );

    // Step 3: Optionally create worktree
    let branch_name = format!("fix/{bead_id}");
    if isolate {
        let worktree_result = Command::new("git")
            .args(["worktree", "add", "-b", &branch_name, &format!("../{branch_name}")])
            .output();

        match worktree_result {
            Ok(output) if output.status.success() => {
                println!("Created worktree: ../{branch_name}");
            }
            _ => {
                eprintln!("warning: worktree creation failed, running in-place");
            }
        }
    }

    // Step 4: Spawn Claude Code
    println!("Dispatching {bead_id} to Claude Code...");
    println!("Prompt: {prompt}");

    // TODO: actually spawn claude CLI
    // Command::new("claude")
    //     .args(["--print", &prompt])
    //     .current_dir(worktree_path)
    //     .status()?;

    // Step 5: Update bead status
    // bd update {bead_id} --status in_progress

    Ok(())
}

#[cfg(test)]
mod tests {
    // Dispatch tests need a mock for `bd` and `claude` CLIs
}
