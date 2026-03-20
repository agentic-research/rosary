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
    let has_remote = tokio::process::Command::new("git")
        .args(["fetch", "origin", "main"])
        .current_dir(repo_path)
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false);

    // Branch from origin/main if available, otherwise HEAD (local repos without remotes)
    let wt_str = worktree_path.to_string_lossy().to_string();
    let mut args: Vec<&str> = vec!["worktree", "add", &wt_str, "-b", &branch_name];
    if has_remote {
        args.push("origin/main");
    }

    let output = tokio::process::Command::new("git")
        .args(&args)
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
// Terminal step: push branch + create PR
// ---------------------------------------------------------------------------

/// Result of the terminal merge_or_pr step.
pub struct TerminalResult {
    /// Human-readable summary of what happened.
    pub message: String,
    /// PR URL if one was created.
    pub pr_url: Option<String>,
}

/// Terminal step: push branch and create a PR.
///
/// Called after an agent completes work and passes verification. Always pushes
/// the branch and creates a PR — branch protection rules require it.
///
/// `repo_path` should be the MAIN repo (not the worktree).
/// `bead_title` is used for the PR title.
pub async fn merge_or_pr(
    repo_path: &Path,
    branch: &str,
    bead_id: &str,
    issue_type: &str,
) -> Result<TerminalResult> {
    // Golden Rule 11: every commit must reference a bead.
    enforce_bead_refs(repo_path, branch, bead_id).await;

    // Push the branch to origin
    let push = tokio::process::Command::new("git")
        .args(["push", "-u", "origin", branch])
        .current_dir(repo_path)
        .output()
        .await
        .context("pushing branch")?;

    if !push.status.success() {
        let stderr = String::from_utf8_lossy(&push.stderr);
        // No remote? Fall back to ff-merge (local repos, test environments).
        if stderr.contains("does not appear to be a git repository")
            || stderr.contains("No configured push destination")
            || stderr.contains("No such remote")
        {
            return merge_local(repo_path, branch, bead_id).await;
        }
        anyhow::bail!("[terminal] {bead_id}: push failed: {stderr}");
    }
    eprintln!("[terminal] {bead_id}: pushed {branch}");

    // Try to create PR via GitHub App or PAT
    let pr_url = match create_pr_for_bead(repo_path, branch, bead_id, issue_type).await {
        Ok(url) => Some(url),
        Err(e) => {
            eprintln!(
                "[terminal] {bead_id}: PR creation failed ({e}), branch pushed for manual PR"
            );
            None
        }
    };

    let message = if let Some(ref url) = pr_url {
        format!("pushed {branch}, PR: {url}")
    } else {
        format!("pushed {branch} — create PR manually")
    };
    eprintln!("[terminal] {bead_id}: {message}");
    Ok(TerminalResult { message, pr_url })
}

/// Local-only fallback: ff-merge to main when there's no remote.
async fn merge_local(repo_path: &Path, branch: &str, bead_id: &str) -> Result<TerminalResult> {
    eprintln!("[terminal] {bead_id}: no remote, falling back to local ff-merge");
    let merge = tokio::process::Command::new("git")
        .args(["merge", "--ff-only", branch])
        .current_dir(repo_path)
        .output()
        .await
        .context("ff-merging to main")?;

    if merge.status.success() {
        let message = format!("ff-merged {branch} to main (local)");
        eprintln!("[terminal] {bead_id}: {message}");
        Ok(TerminalResult {
            message,
            pr_url: None,
        })
    } else {
        let stderr = String::from_utf8_lossy(&merge.stderr);
        anyhow::bail!("[terminal] {bead_id}: ff-merge failed: {stderr}");
    }
}

/// Create a PR for a bead's branch using the configured GitHub auth.
async fn create_pr_for_bead(
    repo_path: &Path,
    branch: &str,
    bead_id: &str,
    issue_type: &str,
) -> Result<String> {
    let github_config = load_github_config_best_effort()?;
    let gh = crate::github::GitHubClient::from_config(&github_config)?;

    let (owner, repo) = parse_github_remote(repo_path)
        .await
        .context("could not determine GitHub owner/repo from git remote")?;

    let base = github_config.base.clone();

    // Build PR title and body
    let title = format!("[{bead_id}] {issue_type}: agent-generated changes");
    let body = format!(
        "## Bead: `{bead_id}`\n\n\
         Type: {issue_type}\n\n\
         ---\n\
         *Generated by [rosary](https://github.com/agentic-research/rosary)*"
    );

    let pr = gh
        .create_pr_from_worktree(repo_path, &owner, &repo, branch, &base, &title, &body)
        .await?;
    Ok(pr.html_url)
}

/// Load GitHub config from ~/.rsry/config.toml, falling back to defaults.
fn load_github_config_best_effort() -> Result<crate::config::GitHubConfig> {
    let home = dirs_next::home_dir().context("no home dir")?;
    let config_path = home.join(".rsry").join("config.toml");
    let content = std::fs::read_to_string(&config_path)
        .with_context(|| format!("reading {}", config_path.display()))?;
    let config: crate::config::Config =
        toml::from_str(&content).with_context(|| format!("parsing {}", config_path.display()))?;
    config
        .github
        .context("[github] section not found in config.toml")
}

/// Parse owner/repo from the `origin` remote URL.
///
/// Handles both SSH (`git@github.com:owner/repo.git`) and HTTPS
/// (`https://github.com/owner/repo.git`) formats.
async fn parse_github_remote(repo_path: &Path) -> Result<(String, String)> {
    let output = tokio::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(repo_path)
        .output()
        .await
        .context("git remote get-url origin")?;

    if !output.status.success() {
        anyhow::bail!("no origin remote configured");
    }

    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    parse_owner_repo(&url).with_context(|| format!("parsing remote URL: {url}"))
}

/// Extract (owner, repo) from a GitHub URL.
pub(super) fn parse_owner_repo(url: &str) -> Result<(String, String)> {
    // SSH: git@github.com:owner/repo.git
    if let Some(path) = url.strip_prefix("git@github.com:") {
        let path = path.trim_end_matches(".git");
        if let Some((owner, repo)) = path.split_once('/') {
            return Ok((owner.to_string(), repo.to_string()));
        }
    }
    // HTTPS: https://github.com/owner/repo.git
    if url.contains("github.com/") {
        let parts: Vec<&str> = url.split("github.com/").collect();
        if parts.len() >= 2 {
            let path = parts[1].trim_end_matches(".git");
            if let Some((owner, repo)) = path.split_once('/') {
                return Ok((owner.to_string(), repo.to_string()));
            }
        }
    }
    anyhow::bail!("unrecognized GitHub remote format")
}

/// Check commit messages for bead references (Golden Rule 11) and auto-amend if missing.
async fn enforce_bead_refs(repo_path: &Path, branch: &str, bead_id: &str) {
    let log_output = tokio::process::Command::new("git")
        .args(["log", "main..HEAD", "--format=%H %s"])
        .current_dir(repo_path)
        .output()
        .await;

    let Ok(log_output) = log_output else { return };
    if !log_output.status.success() {
        return;
    }

    let log = String::from_utf8_lossy(&log_output.stdout);
    let missing_refs: Vec<&str> = log
        .lines()
        .filter(|line| {
            let msg = line.split_once(' ').map(|(_, m)| m).unwrap_or(line);
            !msg.starts_with('[') && !msg.to_lowercase().contains("bead:")
        })
        .collect();

    if missing_refs.is_empty() {
        return;
    }

    eprintln!(
        "[workspace] WARNING: {} commit(s) on {branch} missing bead reference (Golden Rule 11):",
        missing_refs.len()
    );
    for line in &missing_refs {
        eprintln!("[workspace]   {line}");
    }

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
