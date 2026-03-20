//! Workspace helpers: directory resolution, VCS creation/cleanup, orphan sweep, merge.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Directory resolution
// ---------------------------------------------------------------------------

/// Resolve the workspace directory for a bead, creating the parent if needed.
/// Uses `~/.rsry/worktrees/{repo}/{id}` — user-scoped, survives repo cleans,
/// doesn't collide with CC's .claude/worktrees/ or other tools.
pub fn workspace_dir(repo_path: &Path, id: &str) -> PathBuf {
    let repo_name = repo_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let home = dirs_next::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let ws_root = home.join(".rsry").join("worktrees").join(&repo_name);
    // Best-effort mkdir — callers handle the actual VCS error if this fails
    let _ = std::fs::create_dir_all(&ws_root);
    ws_root.join(id)
}

// ---------------------------------------------------------------------------
// jj isolation
// ---------------------------------------------------------------------------

/// Create a jj workspace for isolated agent work.
pub(super) async fn create_jj_workspace(repo_path: &Path, id: &str) -> Result<PathBuf> {
    let workspace_name = format!("fix-{id}");
    let workspace_path = workspace_dir(repo_path, id);

    let output = tokio::process::Command::new("jj")
        .args([
            "workspace",
            "add",
            &workspace_path.to_string_lossy(),
            "--name",
            &workspace_name,
        ])
        .current_dir(repo_path)
        .output()
        .await
        .context("jj workspace add")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("jj workspace add failed: {stderr}");
    }

    // Set jj description so workspace shows bead context in `jj workspace list`
    let _ = tokio::process::Command::new("jj")
        .args(["describe", "-m", &format!("bead:{id}")])
        .current_dir(&workspace_path)
        .output()
        .await;

    eprintln!(
        "[workspace] created jj workspace: {}",
        workspace_path.display()
    );
    Ok(workspace_path)
}

/// Clean up a jj workspace. Best-effort — don't propagate errors.
pub fn cleanup_jj_workspace(repo_path: &Path, id: &str) {
    let workspace_name = format!("fix-{id}");
    let _ = std::process::Command::new("jj")
        .args(["workspace", "forget", &workspace_name])
        .current_dir(repo_path)
        .output();

    let ws_dir = workspace_dir(repo_path, id);
    let _ = std::fs::remove_dir_all(ws_dir);
}

// ---------------------------------------------------------------------------
// git worktree isolation (fallback)
// ---------------------------------------------------------------------------

/// Create a git worktree for isolated work.
///
/// Handles the common case where a previous dispatch left behind a
/// `fix/{id}` branch — deletes the stale branch and retries.
pub(super) async fn create_git_worktree(repo_path: &Path, id: &str) -> Result<PathBuf> {
    let branch_name = format!("fix/{id}");
    let worktree_path = workspace_dir(repo_path, id);

    // Fetch latest origin/main so the worktree branches from current remote HEAD,
    // not stale local HEAD. Without this, agents include already-merged changes
    // in their diffs — the root cause of every duplicate PR in the overnight session.
    let _ = tokio::process::Command::new("git")
        .args(["fetch", "origin", "main"])
        .current_dir(repo_path)
        .output()
        .await;

    let output = tokio::process::Command::new("git")
        .args([
            "worktree",
            "add",
            &worktree_path.to_string_lossy(),
            "-b",
            &branch_name,
            "origin/main",
        ])
        .current_dir(repo_path)
        .output()
        .await
        .context("git worktree add")?;

    if output.status.success() {
        eprintln!(
            "[workspace] created git worktree: {}",
            worktree_path.display()
        );
        return Ok(worktree_path);
    }

    let stderr = String::from_utf8_lossy(&output.stderr);

    // Branch already exists from a previous (failed) dispatch — clean up and retry.
    if stderr.contains("already exists") {
        eprintln!("[workspace] branch {branch_name} already exists, cleaning up stale state");
        // Remove stale worktree dir if it exists
        let _ = std::fs::remove_dir_all(&worktree_path);
        // Prune worktree references that point to missing directories
        let _ = tokio::process::Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(repo_path)
            .output()
            .await;
        // Delete the stale branch
        let _ = tokio::process::Command::new("git")
            .args(["branch", "-D", &branch_name])
            .current_dir(repo_path)
            .output()
            .await;

        // Retry
        let retry = tokio::process::Command::new("git")
            .args([
                "worktree",
                "add",
                &worktree_path.to_string_lossy(),
                "-b",
                &branch_name,
            ])
            .current_dir(repo_path)
            .output()
            .await
            .context("git worktree add (retry)")?;

        if retry.status.success() {
            eprintln!(
                "[workspace] created git worktree (after cleanup): {}",
                worktree_path.display()
            );
            return Ok(worktree_path);
        }
        let retry_err = String::from_utf8_lossy(&retry.stderr);
        anyhow::bail!("git worktree add failed after cleanup: {retry_err}");
    }

    anyhow::bail!("git worktree add failed: {stderr}");
}

/// Clean up a git worktree. Best-effort.
pub fn cleanup_git_worktree(repo_path: &Path, id: &str) {
    let worktree_path = workspace_dir(repo_path, id);
    let _ = std::process::Command::new("git")
        .args([
            "worktree",
            "remove",
            &worktree_path.to_string_lossy(),
            "--force",
        ])
        .current_dir(repo_path)
        .output();

    // Also delete the branch
    let branch_name = format!("fix/{id}");
    let _ = std::process::Command::new("git")
        .args(["branch", "-D", &branch_name])
        .current_dir(repo_path)
        .output();
}

// ---------------------------------------------------------------------------
// Orphan sweep
// ---------------------------------------------------------------------------

/// Scan `.rsry-workspaces/` directories and clean up any that don't
/// correspond to active bead IDs. Call on startup to reclaim leaked workspaces.
pub fn sweep_orphaned(repo_paths: &[PathBuf], active_bead_ids: &[String]) {
    let home = dirs_next::home_dir().unwrap_or_else(|| PathBuf::from("."));
    for repo_path in repo_paths {
        let repo_name = repo_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let ws_root = home.join(".rsry").join("worktrees").join(&repo_name);
        if !ws_root.exists() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&ws_root) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !active_bead_ids.contains(&name) {
                eprintln!("[sweep] cleaning orphaned workspace: {name}");
                cleanup_jj_workspace(repo_path, &name);
                cleanup_git_worktree(repo_path, &name);
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Terminal step: merge or PR
// ---------------------------------------------------------------------------

/// Terminal step: ff-merge small beads to main, push branch for features/epics.
///
/// Called after an agent completes work in a worktree branch. For tasks/bugs/chores,
/// fast-forward merges to main and pushes. For features/epics, pushes the branch
/// for manual PR creation.
///
/// `repo_path` should be the MAIN repo (not the worktree).
pub async fn merge_or_pr(
    repo_path: &Path,
    branch: &str,
    bead_id: &str,
    issue_type: &str,
) -> Result<String> {
    // Golden Rule 11: every commit must reference a bead.
    // Check all commits on this branch that aren't on main.
    let log_output = tokio::process::Command::new("git")
        .args(["log", "main..HEAD", "--format=%H %s"])
        .current_dir(repo_path)
        .output()
        .await
        .context("checking commit messages for bead refs")?;
    if log_output.status.success() {
        let log = String::from_utf8_lossy(&log_output.stdout);
        let missing_refs: Vec<&str> = log
            .lines()
            .filter(|line| {
                // Check for [bead-id] prefix or bead: in body
                let msg = line.split_once(' ').map(|(_, m)| m).unwrap_or(line);
                !msg.starts_with('[') && !msg.to_lowercase().contains("bead:")
            })
            .collect();
        if !missing_refs.is_empty() {
            eprintln!(
                "[workspace] WARNING: {} commit(s) on {branch} missing bead reference (Golden Rule 11):",
                missing_refs.len()
            );
            for line in &missing_refs {
                eprintln!("[workspace]   {line}");
            }
            // Amend the most recent commit to prepend [bead-id] prefix
            let original_msg = log
                .lines()
                .next()
                .unwrap_or("")
                .split_once(' ')
                .map(|(_, msg)| msg)
                .unwrap_or("");
            let amend_msg = format!("[{bead_id}] {original_msg}");
            if let Ok(out) = tokio::process::Command::new("git")
                .args(["commit", "--amend", "-m", &amend_msg])
                .current_dir(repo_path)
                .output()
                .await
                && out.status.success()
            {
                eprintln!("[workspace] auto-amended last commit with [{bead_id}] prefix");
            }
        }
    }

    let needs_pr = matches!(issue_type, "feature" | "epic");

    if needs_pr {
        let push = tokio::process::Command::new("git")
            .args(["push", "origin", branch])
            .current_dir(repo_path)
            .output()
            .await
            .context("pushing branch for PR")?;
        if push.status.success() {
            let msg = format!("pushed {branch} — PR needed");
            eprintln!("[terminal] {bead_id}: {msg}");
            Ok(msg)
        } else {
            let stderr = String::from_utf8_lossy(&push.stderr);
            let msg = format!("push failed: {stderr}");
            eprintln!("[terminal] {bead_id}: {msg}");
            anyhow::bail!(msg)
        }
    } else {
        // Fast-forward merge to main
        let merge = tokio::process::Command::new("git")
            .args(["merge", "--ff-only", branch])
            .current_dir(repo_path)
            .output()
            .await
            .context("ff-merging to main")?;
        if merge.status.success() {
            eprintln!("[terminal] {bead_id}: ff-merged {branch} to main");
            let _ = tokio::process::Command::new("git")
                .args(["push", "origin", "main"])
                .current_dir(repo_path)
                .output()
                .await;
            Ok(format!("ff-merged {branch} to main"))
        } else {
            let stderr = String::from_utf8_lossy(&merge.stderr);
            eprintln!(
                "[terminal] {bead_id}: ff-merge failed ({stderr}), pushing branch for manual merge"
            );
            let _ = tokio::process::Command::new("git")
                .args(["push", "origin", branch])
                .current_dir(repo_path)
                .output()
                .await;
            Ok(format!("ff-merge failed, pushed {branch} for manual merge"))
        }
    }
}
