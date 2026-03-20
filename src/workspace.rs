//! Feature-scoped workspace — jj-native isolation + pluggable compute.
//!
//! A `Workspace` manages the full lifecycle of agent work:
//!   1. Isolate: create a jj workspace (or git worktree fallback)
//!   2. Provision: set up compute environment (local or remote)
//!   3. Execute: run agent commands
//!   4. Checkpoint: snapshot state (jj commit + optional provider checkpoint)
//!   5. Teardown: destroy compute + clean up workspace
//!
//! Workspaces can be **bead-scoped** (one workspace per bead, current model)
//! or **feature-scoped** (one workspace for many beads, rollup model).

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::backend::{ComputeProvider, ExecHandle, ExecResult, ProvisionOpts};

/// VCS backend for code isolation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VcsKind {
    /// jj (preferred — zero-cost branching, first-class isolation).
    Jj,
    /// git worktree (fallback when jj unavailable).
    Git,
    /// No VCS isolation — run in-place. Only for single-concurrency.
    None,
}

/// Detect which VCS to use for workspace isolation.
///
/// Colocated repos (both .jj and .git): use git worktree. Agents use
/// `git add/commit` which needs a proper `.git` file that only
/// `git worktree add` provides. jj sees git commits via colocation.
/// The orchestrator handles jj checkpoint/bookmark separately.
pub fn detect_vcs(repo_path: &Path) -> VcsKind {
    let has_jj = repo_path.join(".jj").exists();
    let has_git = repo_path.join(".git").exists();
    match (has_jj, has_git) {
        (_, true) => VcsKind::Git,    // git worktree (jj tracks via colocation)
        (true, false) => VcsKind::Jj, // pure jj
        _ => VcsKind::None,
    }
}

/// A workspace manages isolated agent execution for a bead or feature.
pub struct Workspace {
    /// Bead or feature ID this workspace serves.
    pub id: String,
    /// Repo this workspace operates on.
    pub repo: String,
    /// The repo root path (original, not the workspace copy).
    pub repo_path: PathBuf,
    /// Working directory for this workspace (may differ from repo_path).
    pub work_dir: PathBuf,
    /// VCS used for isolation.
    pub vcs: VcsKind,
    /// Compute provider handle (None until provisioned).
    exec_handle: Option<ExecHandle>,
}

impl Workspace {
    /// Reconstruct a workspace from existing on-disk state.
    ///
    /// Used by MCP tools that need to operate on a workspace created by
    /// a previous call. Does not create anything — just rebuilds the struct.
    pub fn from_existing(id: &str, repo: &str, repo_path: &Path) -> Self {
        let vcs = detect_vcs(repo_path);
        let ws_dir = workspace_dir(repo_path, id);
        let (work_dir, vcs) = if ws_dir.exists() {
            (ws_dir, vcs)
        } else {
            (repo_path.to_path_buf(), VcsKind::None)
        };
        Workspace {
            id: id.to_string(),
            repo: repo.to_string(),
            repo_path: repo_path.to_path_buf(),
            work_dir,
            vcs,
            exec_handle: None,
        }
    }

    /// Create a new workspace with code isolation.
    ///
    /// Tries jj first, falls back to git worktree, then in-place.
    pub async fn create(id: &str, repo: &str, repo_path: &Path, isolate: bool) -> Result<Self> {
        // Canonicalize to avoid relative path issues when MCP server
        // runs from a different cwd than the repo root.
        let repo_path = repo_path
            .canonicalize()
            .unwrap_or_else(|_| repo_path.to_path_buf());
        let repo_path = repo_path.as_path();

        let vcs = if isolate {
            detect_vcs(repo_path)
        } else {
            VcsKind::None
        };

        // Reuse existing workspace if it exists (resume after agent death)
        let existing_ws = workspace_dir(repo_path, id);
        if existing_ws.exists() && vcs != VcsKind::None {
            eprintln!(
                "[workspace] reusing existing workspace: {}",
                existing_ws.display()
            );
            return Ok(Workspace {
                id: id.to_string(),
                repo: repo.to_string(),
                repo_path: repo_path.to_path_buf(),
                work_dir: existing_ws,
                vcs,
                exec_handle: None,
            });
        }

        let (work_dir, actual_vcs) = match vcs {
            VcsKind::Jj => match create_jj_workspace(repo_path, id).await {
                Ok(path) => (path, VcsKind::Jj),
                Err(e) if isolate => {
                    // Isolation was requested — don't silently run in-place.
                    // This prevents agents from writing to the main repo.
                    anyhow::bail!(
                        "workspace isolation failed for {id}: jj workspace creation failed: {e}"
                    );
                }
                Err(e) => {
                    eprintln!("[workspace] jj isolation failed ({e}), falling back to in-place");
                    (repo_path.to_path_buf(), VcsKind::None)
                }
            },
            VcsKind::Git => match create_git_worktree(repo_path, id).await {
                Ok(path) => (path, VcsKind::Git),
                Err(e) if isolate => {
                    // Isolation was requested — don't silently run in-place.
                    // This prevents agents from writing to the main repo.
                    anyhow::bail!(
                        "workspace isolation failed for {id}: git worktree creation failed: {e}"
                    );
                }
                Err(e) => {
                    eprintln!("[workspace] git worktree failed ({e}), falling back to in-place");
                    (repo_path.to_path_buf(), VcsKind::None)
                }
            },
            VcsKind::None if isolate => {
                anyhow::bail!(
                    "workspace isolation failed for {id}: no VCS found in {} \
                     (need .git or .jj for isolation)",
                    repo_path.display()
                );
            }
            VcsKind::None => (repo_path.to_path_buf(), VcsKind::None),
        };

        Ok(Workspace {
            id: id.to_string(),
            repo: repo.to_string(),
            repo_path: repo_path.to_path_buf(),
            work_dir,
            vcs: actual_vcs,
            exec_handle: None,
        })
    }

    /// Provision compute for this workspace.
    pub async fn provision(&mut self, provider: &dyn ComputeProvider) -> Result<()> {
        let opts = ProvisionOpts::new(&self.id, &self.repo);
        let handle = provider.provision(&opts).await?;
        self.exec_handle = Some(handle);
        Ok(())
    }

    /// Execute a command in this workspace's compute environment.
    ///
    /// If a compute provider is provisioned, runs there.
    /// Otherwise runs locally in the workspace directory.
    pub async fn exec(&self, provider: &dyn ComputeProvider, cmd: &[&str]) -> Result<ExecResult> {
        if let Some(ref handle) = self.exec_handle {
            provider.exec(handle, cmd).await
        } else {
            // No provisioned handle — run locally
            crate::backend::LocalProvider
                .exec(
                    &ExecHandle {
                        id: format!("local-{}", self.id),
                        backend: "local".into(),
                    },
                    cmd,
                )
                .await
        }
    }

    /// Create a jj commit (code checkpoint) in the workspace.
    pub async fn jj_commit(&self, message: &str) -> Result<()> {
        if self.vcs != VcsKind::Jj {
            return Ok(());
        }
        let output = tokio::process::Command::new("jj")
            .args(["commit", "-m", message])
            .current_dir(&self.work_dir)
            .output()
            .await
            .context("jj commit")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("jj commit failed: {stderr}");
        }
        Ok(())
    }

    /// Create a jj bookmark for this workspace's work.
    pub async fn jj_bookmark(&self, name: &str) -> Result<()> {
        if self.vcs != VcsKind::Jj {
            return Ok(());
        }
        let output = tokio::process::Command::new("jj")
            .args(["bookmark", "create", name])
            .current_dir(&self.work_dir)
            .output()
            .await
            .context("jj bookmark create")?;

        if !output.status.success() {
            // Bookmark may already exist — try move instead
            let output = tokio::process::Command::new("jj")
                .args(["bookmark", "move", name])
                .current_dir(&self.work_dir)
                .output()
                .await
                .context("jj bookmark move")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("jj bookmark failed: {stderr}");
            }
        }
        Ok(())
    }

    /// Checkpoint the workspace: commit all changes, return commit/change ID.
    ///
    /// Call this BEFORE cleanup to preserve the agent's work.
    /// - Git worktree: `git add -A && git commit`
    /// - jj workspace: `jj commit` + bookmark
    ///   The orchestrator calls this — agents don't commit themselves.
    pub async fn checkpoint(&self, message: &str) -> Result<Option<String>> {
        match self.vcs {
            VcsKind::Git => self.git_checkpoint(message).await,
            VcsKind::Jj => {
                self.jj_commit(message).await?;
                let change_id = self.jj_change_id().await?;
                let bookmark = format!("fix/{}", self.id);
                self.jj_bookmark(&bookmark).await?;
                Ok(change_id)
            }
            VcsKind::None => Ok(None),
        }
    }

    /// Git checkpoint: stage all changes and commit. Returns short SHA.
    async fn git_checkpoint(&self, message: &str) -> Result<Option<String>> {
        // Check if there are any changes to commit
        let status = tokio::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&self.work_dir)
            .output()
            .await
            .context("git status")?;

        if status.stdout.is_empty() {
            return Ok(None); // Nothing to commit
        }

        // Stage all changes
        let add = tokio::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(&self.work_dir)
            .output()
            .await
            .context("git add")?;

        if !add.status.success() {
            let stderr = String::from_utf8_lossy(&add.stderr);
            anyhow::bail!("git add failed: {stderr}");
        }

        // Commit
        let commit = tokio::process::Command::new("git")
            .args(["commit", "-m", message])
            .current_dir(&self.work_dir)
            .output()
            .await
            .context("git commit")?;

        if !commit.status.success() {
            let stderr = String::from_utf8_lossy(&commit.stderr);
            anyhow::bail!("git commit failed: {stderr}");
        }

        // Get short SHA
        let rev = tokio::process::Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .current_dir(&self.work_dir)
            .output()
            .await
            .context("git rev-parse")?;

        let sha = String::from_utf8_lossy(&rev.stdout).trim().to_string();
        Ok(if sha.is_empty() { None } else { Some(sha) })
    }

    /// Get the jj change ID of the most recent commit (@-).
    ///
    /// After `jj commit`, the new commit is @- (jj advances the working copy).
    async fn jj_change_id(&self) -> Result<Option<String>> {
        if self.vcs != VcsKind::Jj {
            return Ok(None);
        }
        let output = tokio::process::Command::new("jj")
            .args(["log", "-r", "@-", "--no-graph", "-T", "change_id"])
            .current_dir(&self.work_dir)
            .output()
            .await
            .context("jj log change_id")?;

        if !output.status.success() {
            return Ok(None);
        }
        let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if id.is_empty() {
            Ok(None)
        } else {
            Ok(Some(id))
        }
    }

    /// Tear down the workspace: destroy compute + clean up VCS isolation.
    pub async fn teardown(self, provider: &dyn ComputeProvider) -> Result<()> {
        // Destroy compute
        if let Some(ref handle) = self.exec_handle
            && let Err(e) = provider.destroy(handle).await
        {
            eprintln!("[workspace] compute destroy failed: {e}");
        }

        // Clean up VCS isolation
        match self.vcs {
            VcsKind::Jj => {
                cleanup_jj_workspace(&self.repo_path, &self.id);
            }
            VcsKind::Git => {
                cleanup_git_worktree(&self.repo_path, &self.id);
            }
            VcsKind::None => {}
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// jj isolation
// ---------------------------------------------------------------------------

/// Create a jj workspace for isolated agent work.
async fn create_jj_workspace(repo_path: &Path, id: &str) -> Result<PathBuf> {
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
pub(crate) fn cleanup_jj_workspace(repo_path: &Path, id: &str) {
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

/// Resolve the workspace directory for a bead, creating the parent if needed.
/// Uses `~/.rsry/worktrees/{repo}/{id}` — user-scoped, survives repo cleans,
/// doesn't collide with CC's .claude/worktrees/ or other tools.
fn workspace_dir(repo_path: &Path, id: &str) -> PathBuf {
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

/// Create a git worktree for isolated work.
///
/// Handles the common case where a previous dispatch left behind a
/// `fix/{id}` branch — deletes the stale branch and retries.
async fn create_git_worktree(repo_path: &Path, id: &str) -> Result<PathBuf> {
    let branch_name = format!("fix/{id}");
    let worktree_path = workspace_dir(repo_path, id);

    let output = tokio::process::Command::new("git")
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
pub(crate) fn cleanup_git_worktree(repo_path: &Path, id: &str) {
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_vcs_jj() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".jj")).unwrap();
        assert_eq!(detect_vcs(tmp.path()), VcsKind::Jj);
    }

    #[test]
    fn detect_vcs_git() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
        assert_eq!(detect_vcs(tmp.path()), VcsKind::Git);
    }

    #[test]
    fn detect_vcs_colocated_uses_git() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".jj")).unwrap();
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
        // Colocated: git worktree for agents, jj tracks via colocation
        assert_eq!(detect_vcs(tmp.path()), VcsKind::Git);
    }

    #[test]
    fn detect_vcs_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert_eq!(detect_vcs(tmp.path()), VcsKind::None);
    }

    #[tokio::test]
    async fn workspace_create_no_isolation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let canonical = tmp.path().canonicalize().unwrap();
        let ws = Workspace::create("test-1", "repo", tmp.path(), false)
            .await
            .unwrap();
        assert_eq!(ws.vcs, VcsKind::None);
        assert_eq!(ws.work_dir, canonical);
        assert!(ws.exec_handle.is_none());
    }

    #[tokio::test]
    async fn workspace_create_no_vcs_with_isolate_errors() {
        let tmp = tempfile::TempDir::new().unwrap();
        // No .jj or .git — isolate=true must error, not silently fall back
        let result = Workspace::create("test-1", "repo", tmp.path(), true).await;
        assert!(
            result.is_err(),
            "Workspace::create with isolate=true must fail when no VCS is available, \
             not silently fall back to in-place"
        );
    }

    #[tokio::test]
    async fn workspace_create_no_vcs_without_isolate_falls_through() {
        let tmp = tempfile::TempDir::new().unwrap();
        let canonical = tmp.path().canonicalize().unwrap();
        // No .jj or .git — isolate=false allows in-place execution
        let ws = Workspace::create("test-1", "repo", tmp.path(), false)
            .await
            .unwrap();
        assert_eq!(ws.vcs, VcsKind::None);
        assert_eq!(ws.work_dir, canonical);
    }

    #[tokio::test]
    async fn workspace_provision_and_exec() {
        use crate::backend::tests::MockProvider;

        let tmp = tempfile::TempDir::new().unwrap();
        let mock = MockProvider::new();

        let mut ws = Workspace::create("test-1", "repo", tmp.path(), false)
            .await
            .unwrap();
        ws.provision(&mock).await.unwrap();
        assert!(ws.exec_handle.is_some());

        let result = ws.exec(&mock, &["echo", "hi"]).await.unwrap();
        assert!(result.success());

        let provisions = mock.provisions.lock().unwrap();
        assert_eq!(provisions.len(), 1);
        assert_eq!(provisions[0].bead_id, "test-1");
    }

    /// Regression: git worktree must branch from HEAD, not an orphan.
    /// Bug: worktree only had .beads/ bd init commit, no source code.
    #[tokio::test]
    async fn git_worktree_has_source_code_not_just_beads() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path();

        std::process::Command::new("git")
            .args(["init"])
            .current_dir(repo)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(repo)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(repo)
            .output()
            .unwrap();
        std::fs::write(repo.join("src.rs"), "fn main() {}").unwrap();
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(repo)
            .output()
            .unwrap();
        let commit = std::process::Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(commit.status.success(), "git commit must succeed");

        // Simulate .beads/ (Dolt creates its own git repo inside)
        std::fs::create_dir_all(repo.join(".beads").join("dolt")).unwrap();

        let wt_path = create_git_worktree(repo, "test-regression").await;
        assert!(wt_path.is_ok(), "worktree creation should succeed");
        let wt_path = wt_path.unwrap();

        assert!(
            wt_path.join("src.rs").exists(),
            "worktree must contain source files from HEAD, not just .beads/"
        );

        cleanup_git_worktree(repo, "test-regression");
    }

    #[tokio::test]
    async fn workspace_exec_without_provision_uses_local() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ws = Workspace::create("test-1", "repo", tmp.path(), false)
            .await
            .unwrap();

        // No provision — should fall back to LocalProvider
        let mock = crate::backend::tests::MockProvider::new();
        let result = ws.exec(&mock, &["echo", "fallback"]).await.unwrap();
        // This actually runs locally via LocalProvider, not through mock
        assert!(result.success());
        assert!(result.stdout.contains("fallback"));
    }

    #[tokio::test]
    async fn workspace_teardown_destroys_compute() {
        use crate::backend::tests::MockProvider;

        let tmp = tempfile::TempDir::new().unwrap();
        let mock = MockProvider::new();

        let mut ws = Workspace::create("test-1", "repo", tmp.path(), false)
            .await
            .unwrap();
        ws.provision(&mock).await.unwrap();
        ws.teardown(&mock).await.unwrap();

        let destroys = mock.destroys.lock().unwrap();
        assert_eq!(destroys.len(), 1);
        assert_eq!(destroys[0], "mock-test-1");
    }

    #[tokio::test]
    async fn workspace_teardown_without_provision_ok() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mock = crate::backend::tests::MockProvider::new();

        let ws = Workspace::create("test-1", "repo", tmp.path(), false)
            .await
            .unwrap();
        // Should not error even without provisioning
        ws.teardown(&mock).await.unwrap();

        let destroys = mock.destroys.lock().unwrap();
        assert_eq!(destroys.len(), 0);
    }

    #[test]
    fn vcs_kind_eq() {
        assert_eq!(VcsKind::Jj, VcsKind::Jj);
        assert_ne!(VcsKind::Jj, VcsKind::Git);
        assert_ne!(VcsKind::Git, VcsKind::None);
    }

    // -----------------------------------------------------------------------
    // Helper: create a git+jj colocated repo in a tempdir.
    //
    // Returns (TempDir, canonical repo path). TempDir must be held alive
    // for the lifetime of the test (drop deletes it).
    // -----------------------------------------------------------------------
    async fn setup_colocated_repo() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path().canonicalize().unwrap();

        // git init
        let out = std::process::Command::new("git")
            .args(["init"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "git init failed");

        // Configure git user (needed for commits in CI / clean environments)
        std::process::Command::new("git")
            .args(["config", "user.email", "test@rosary.dev"])
            .current_dir(&repo)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "rosary-test"])
            .current_dir(&repo)
            .output()
            .unwrap();

        // Seed a source file so HEAD exists
        std::fs::write(repo.join("lib.rs"), "pub fn hello() {}").unwrap();
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&repo)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&repo)
            .output()
            .unwrap();

        // jj init --colocate (creates .jj/ alongside existing .git/)
        let jj = std::process::Command::new("jj")
            .args(["git", "init", "--colocate"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(
            jj.status.success(),
            "jj git init --colocate failed: {}",
            String::from_utf8_lossy(&jj.stderr)
        );

        // Sanity: both dirs exist
        assert!(repo.join(".git").exists(), ".git must exist");
        assert!(repo.join(".jj").exists(), ".jj must exist");

        (tmp, repo)
    }

    // -----------------------------------------------------------------------
    // Regression test for rosary-a0eb7c / commit 120fd5a:
    //
    // In old code, detect_vcs() returned Jj for colocated repos, which
    // created jj workspaces where git paths resolved wrong (agent git
    // add/commit saw parent-relative paths). The fix returns Git for
    // colocated repos so git worktree is used instead.
    //
    // This test exercises the FULL dispatch lifecycle:
    //   1. detect_vcs → Git (not Jj) for colocated repo
    //   2. Workspace::create → git worktree with proper .git file
    //   3. git rev-parse inside worktree → worktree path (not parent)
    //   4. git add + commit inside worktree → clean paths (no prefix)
    //   5. Workspace::checkpoint → returns a SHA
    //   6. cleanup → worktree removed, work visible in main repo log
    // -----------------------------------------------------------------------
    #[tokio::test]
    async fn e2e_colocated_workspace_isolation() {
        // Skip if jj is not installed (CI without jj)
        if std::process::Command::new("jj")
            .arg("--help")
            .output()
            .is_err()
        {
            eprintln!("SKIP: jj not installed");
            return;
        }

        let (_tmp, repo) = setup_colocated_repo().await;
        let bead_id = "e2e-colocated-test";

        // ----- Step 1: detect_vcs returns Git for colocated repos ----------
        assert_eq!(
            detect_vcs(&repo),
            VcsKind::Git,
            "colocated repo (both .jj/ and .git/) must use Git worktree, not Jj"
        );

        // ----- Step 2: Workspace::create produces a git worktree ----------
        let ws = Workspace::create(bead_id, "test-repo", &repo, true)
            .await
            .expect("workspace create must succeed");

        assert_eq!(ws.vcs, VcsKind::Git, "workspace vcs should be Git");
        assert_ne!(
            ws.work_dir, ws.repo_path,
            "worktree dir must differ from repo root"
        );
        assert!(
            ws.work_dir.exists(),
            "worktree directory must exist on disk"
        );

        // The worktree should contain a .git *file* (not directory) pointing
        // back to the parent repo's worktree metadata.
        let dot_git = ws.work_dir.join(".git");
        assert!(dot_git.exists(), "worktree must have a .git file");
        assert!(
            dot_git.is_file(),
            ".git in worktree must be a file (gitdir pointer), not a directory"
        );

        // Source files from HEAD must be present
        assert!(
            ws.work_dir.join("lib.rs").exists(),
            "worktree must contain source files from HEAD"
        );

        // ----- Step 3: git rev-parse --show-toplevel → worktree path ------
        let toplevel = tokio::process::Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(&ws.work_dir)
            .output()
            .await
            .expect("git rev-parse must succeed");
        assert!(toplevel.status.success());

        let toplevel_path =
            PathBuf::from(String::from_utf8_lossy(&toplevel.stdout).trim().to_string());
        // Canonicalize both to handle macOS /private/var vs /var symlinks
        assert_eq!(
            toplevel_path.canonicalize().unwrap(),
            ws.work_dir.canonicalize().unwrap(),
            "git rev-parse --show-toplevel must return the WORKTREE path, not the parent repo"
        );

        // ----- Step 4: git add + commit inside worktree → clean paths -----
        let test_file = ws.work_dir.join("agent-output.txt");
        std::fs::write(&test_file, "agent wrote this").unwrap();

        let add = tokio::process::Command::new("git")
            .args(["add", "agent-output.txt"])
            .current_dir(&ws.work_dir)
            .output()
            .await
            .expect("git add must succeed");
        assert!(
            add.status.success(),
            "git add failed: {}",
            String::from_utf8_lossy(&add.stderr)
        );

        let commit = tokio::process::Command::new("git")
            .args(["commit", "-m", "agent: test commit"])
            .current_dir(&ws.work_dir)
            .output()
            .await
            .expect("git commit must succeed");
        assert!(
            commit.status.success(),
            "git commit failed: {}",
            String::from_utf8_lossy(&commit.stderr)
        );

        // Verify committed paths don't have a .rsry-workspaces/ prefix.
        // `git diff-tree` lists paths in the last commit — they should be
        // root-relative within the worktree, not parent-relative.
        let diff_tree = tokio::process::Command::new("git")
            .args(["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"])
            .current_dir(&ws.work_dir)
            .output()
            .await
            .expect("git diff-tree must succeed");
        let committed_paths = String::from_utf8_lossy(&diff_tree.stdout);
        assert!(
            committed_paths.contains("agent-output.txt"),
            "committed file must appear in diff-tree"
        );
        assert!(
            !committed_paths.contains(".rsry-workspaces"),
            "committed paths must NOT contain .rsry-workspaces/ prefix — \
             this means git is resolving paths relative to parent, not worktree. \
             Got: {committed_paths}"
        );

        // ----- Step 5: Workspace::checkpoint → returns a SHA ---------------
        // Write another file so checkpoint has something to commit
        std::fs::write(ws.work_dir.join("checkpoint-file.txt"), "checkpoint data").unwrap();

        let sha = ws
            .checkpoint("e2e: checkpoint test")
            .await
            .expect("checkpoint must succeed");
        assert!(
            sha.is_some(),
            "checkpoint must return a SHA when there are dirty files"
        );
        let sha = sha.unwrap();
        assert!(!sha.is_empty(), "checkpoint SHA must be non-empty");

        // Verify the checkpoint commit also has clean paths
        let diff_tree2 = tokio::process::Command::new("git")
            .args(["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"])
            .current_dir(&ws.work_dir)
            .output()
            .await
            .unwrap();
        let checkpoint_paths = String::from_utf8_lossy(&diff_tree2.stdout);
        assert!(
            checkpoint_paths.contains("checkpoint-file.txt"),
            "checkpoint commit must include the new file"
        );
        assert!(
            !checkpoint_paths.contains(".rsry-workspaces"),
            "checkpoint paths must not have workspace prefix"
        );

        // ----- Step 6: cleanup → worktree gone, work in main repo log -----
        let worktree_dir = ws.work_dir.clone();

        // Record the branch name to look up in main repo after cleanup
        let branch_out = tokio::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(&worktree_dir)
            .output()
            .await
            .unwrap();
        let _branch_name = String::from_utf8_lossy(&branch_out.stdout)
            .trim()
            .to_string();

        // Get the full SHA of the branch tip before cleanup
        let full_sha_out = tokio::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&worktree_dir)
            .output()
            .await
            .unwrap();
        let full_sha = String::from_utf8_lossy(&full_sha_out.stdout)
            .trim()
            .to_string();

        // cleanup_git_worktree removes the worktree AND deletes the branch,
        // so we must verify the commit is reachable by SHA before cleanup.
        // But since the branch is force-deleted, the commit becomes
        // unreachable (gc would collect it). Instead, verify the SHA exists
        // in the main repo's object store before cleanup.
        let verify_before = std::process::Command::new("git")
            .args(["cat-file", "-t", &full_sha])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(
            verify_before.status.success(),
            "commit SHA must exist in main repo object store before cleanup"
        );
        let obj_type = String::from_utf8_lossy(&verify_before.stdout)
            .trim()
            .to_string();
        assert_eq!(obj_type, "commit", "SHA must point to a commit object");

        // Now clean up
        cleanup_git_worktree(&repo, bead_id);

        // Worktree directory should be removed
        assert!(
            !worktree_dir.exists(),
            "worktree directory must be removed after cleanup"
        );

        // The commit object still exists in git's object store (it's not
        // garbage collected immediately). Verify it's still there.
        let verify_after = std::process::Command::new("git")
            .args(["cat-file", "-t", &full_sha])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(
            verify_after.status.success(),
            "commit object must still exist in main repo after worktree removal \
             (git objects persist until gc)"
        );
    }

    /// Verify that .beads/ directory from the parent repo is accessible
    /// inside a git worktree (it comes from HEAD, so if .beads/ is
    /// committed or if it's an untracked dir, agents can still reach Dolt).
    ///
    /// This test creates a .beads/ marker in the repo and verifies the
    /// worktree can see it. Actual Dolt connectivity requires a running
    /// Dolt server, so this test only checks file-level accessibility.
    #[tokio::test]
    #[ignore] // requires jj installed; run with `cargo test -- --ignored`
    async fn e2e_colocated_worktree_beads_accessible() {
        if std::process::Command::new("jj")
            .arg("--help")
            .output()
            .is_err()
        {
            eprintln!("SKIP: jj not installed");
            return;
        }

        let (_tmp, repo) = setup_colocated_repo().await;

        // Create and commit a .beads/ marker file (simulates Dolt init)
        std::fs::create_dir_all(repo.join(".beads")).unwrap();
        std::fs::write(repo.join(".beads").join("marker"), "dolt-placeholder").unwrap();
        std::process::Command::new("git")
            .args(["add", ".beads/"])
            .current_dir(&repo)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "add .beads marker"])
            .current_dir(&repo)
            .output()
            .unwrap();

        let ws = Workspace::create("beads-access-test", "test-repo", &repo, true)
            .await
            .expect("workspace create must succeed");

        assert_eq!(ws.vcs, VcsKind::Git);

        // .beads/ should be present in the worktree (branched from HEAD)
        assert!(
            ws.work_dir.join(".beads").join("marker").exists(),
            ".beads/ must be accessible in the git worktree — \
             agents need this to reach Dolt"
        );

        cleanup_git_worktree(&repo, "beads-access-test");
    }

    /// Concurrent multi-agent isolation: two worktrees from the same repo
    /// must not cross-contaminate. Each agent writes a different file;
    /// neither file appears in the other worktree or in main.
    #[tokio::test]
    async fn concurrent_worktree_isolation() {
        if std::process::Command::new("jj")
            .arg("--help")
            .output()
            .is_err()
        {
            eprintln!("SKIP: jj not installed");
            return;
        }

        let (_tmp, repo) = setup_colocated_repo().await;

        // Create two worktrees concurrently
        let ws_a = Workspace::create("agent-alpha", "test-repo", &repo, true)
            .await
            .expect("workspace A must succeed");
        let ws_b = Workspace::create("agent-beta", "test-repo", &repo, true)
            .await
            .expect("workspace B must succeed");

        assert_ne!(ws_a.work_dir, ws_b.work_dir, "worktrees must be distinct");
        assert_ne!(ws_a.work_dir, repo, "worktree A must differ from main");
        assert_ne!(ws_b.work_dir, repo, "worktree B must differ from main");

        // Each "agent" writes a unique file
        std::fs::write(ws_a.work_dir.join("alpha.txt"), "alpha output").unwrap();
        std::fs::write(ws_b.work_dir.join("beta.txt"), "beta output").unwrap();

        // Commit in each worktree
        for (label, ws) in [("alpha", &ws_a), ("beta", &ws_b)] {
            let add = std::process::Command::new("git")
                .args(["add", "-A"])
                .current_dir(&ws.work_dir)
                .output()
                .unwrap();
            assert!(add.status.success(), "{label} git add failed");

            let commit = std::process::Command::new("git")
                .args(["commit", "-m", &format!("{label} work")])
                .current_dir(&ws.work_dir)
                .output()
                .unwrap();
            assert!(commit.status.success(), "{label} git commit failed");
        }

        // Verify isolation: alpha.txt must NOT exist in beta or main
        assert!(
            ws_a.work_dir.join("alpha.txt").exists(),
            "alpha.txt must exist in worktree A"
        );
        assert!(
            !ws_b.work_dir.join("alpha.txt").exists(),
            "alpha.txt must NOT leak into worktree B"
        );
        assert!(
            !repo.join("alpha.txt").exists(),
            "alpha.txt must NOT leak into main repo"
        );

        // Verify isolation: beta.txt must NOT exist in alpha or main
        assert!(
            ws_b.work_dir.join("beta.txt").exists(),
            "beta.txt must exist in worktree B"
        );
        assert!(
            !ws_a.work_dir.join("beta.txt").exists(),
            "beta.txt must NOT leak into worktree A"
        );
        assert!(
            !repo.join("beta.txt").exists(),
            "beta.txt must NOT leak into main repo"
        );

        // Verify each worktree's git log only has its own commit
        let log_a = std::process::Command::new("git")
            .args(["log", "--oneline", "-1", "--format=%s"])
            .current_dir(&ws_a.work_dir)
            .output()
            .unwrap();
        let msg_a = String::from_utf8_lossy(&log_a.stdout).trim().to_string();
        assert_eq!(
            msg_a, "alpha work",
            "worktree A HEAD must be alpha's commit"
        );

        let log_b = std::process::Command::new("git")
            .args(["log", "--oneline", "-1", "--format=%s"])
            .current_dir(&ws_b.work_dir)
            .output()
            .unwrap();
        let msg_b = String::from_utf8_lossy(&log_b.stdout).trim().to_string();
        assert_eq!(msg_b, "beta work", "worktree B HEAD must be beta's commit");

        // Verify main's git status is clean — no unstaged diffs from worktree ops.
        // Regression: worktree isolation leak caused agent changes to appear as
        // unstaged diffs in main, blocking ff-merge of other agents' work.
        let main_status = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&repo)
            .output()
            .unwrap();
        let main_status_str = String::from_utf8_lossy(&main_status.stdout)
            .trim()
            .to_string();
        assert!(
            main_status_str.is_empty(),
            "main repo git status must be clean after worktree operations, got: {main_status_str}"
        );

        // Cleanup
        cleanup_git_worktree(&repo, "agent-alpha");
        cleanup_git_worktree(&repo, "agent-beta");
    }

    /// Regression: create_git_worktree must handle an existing branch
    /// from a previous failed dispatch by cleaning up and retrying.
    #[tokio::test]
    async fn git_worktree_retries_on_existing_branch() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path().canonicalize().unwrap();

        // Set up git repo
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(&repo)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "test@rosary.dev"])
            .current_dir(&repo)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "rosary-test"])
            .current_dir(&repo)
            .output()
            .unwrap();
        std::fs::write(repo.join("main.rs"), "fn main() {}").unwrap();
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&repo)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&repo)
            .output()
            .unwrap();

        // Create a stale branch (simulates a previous failed dispatch)
        std::process::Command::new("git")
            .args(["branch", "fix/stale-bead"])
            .current_dir(&repo)
            .output()
            .unwrap();

        // Now try to create a worktree for the same bead ID — should succeed
        // by cleaning up the stale branch
        let result = create_git_worktree(&repo, "stale-bead").await;
        assert!(
            result.is_ok(),
            "create_git_worktree must retry after cleaning stale branch, got: {:?}",
            result.err()
        );

        let wt_path = result.unwrap();
        assert!(wt_path.exists(), "worktree directory must exist");
        assert!(
            wt_path.join("main.rs").exists(),
            "worktree must contain source files"
        );

        cleanup_git_worktree(&repo, "stale-bead");
    }

    /// When isolate=true and VCS setup fails, Workspace::create must
    /// return an error instead of silently falling back to in-place.
    #[tokio::test]
    async fn workspace_create_isolate_true_no_silent_fallback() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path().canonicalize().unwrap();

        // Create .git dir so detect_vcs returns Git, but don't init git
        // so git worktree add will fail
        std::fs::create_dir(repo.join(".git")).unwrap();

        let result = Workspace::create("test-no-fallback", "repo", &repo, true).await;
        assert!(
            result.is_err(),
            "Workspace::create with isolate=true must fail when VCS setup fails, \
             not silently fall back to in-place"
        );
    }

    /// E2E pipeline integration: exercises the full dispatch lifecycle
    /// across two pipeline phases (dev-agent → staging-agent) in a single
    /// worktree. Tests workspace reuse, handoff writing, checkpoint, and
    /// merge_or_pr.
    ///
    /// No Dolt, no real Claude — just the workspace + handoff + merge mechanics.
    #[tokio::test]
    async fn e2e_pipeline_two_phase_lifecycle() {
        if std::process::Command::new("jj")
            .arg("--help")
            .output()
            .is_err()
        {
            eprintln!("SKIP: jj not installed");
            return;
        }

        let (_tmp, repo) = setup_colocated_repo().await;
        let bead_id = "pipeline-e2e-test";

        // === Phase 1: dev-agent ===
        let ws = Workspace::create(bead_id, "test-repo", &repo, true)
            .await
            .expect("phase 1 workspace create");
        assert_eq!(ws.vcs, VcsKind::Git);

        // Stub agent work: write a file and commit
        std::fs::write(ws.work_dir.join("fix.rs"), "fn fix() { /* dev-agent */ }").unwrap();
        let commit = std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(&ws.work_dir)
            .output()
            .unwrap();
        assert!(commit.status.success());
        let commit = std::process::Command::new("git")
            .args(["commit", "-m", &format!("bead:{bead_id} dev-agent fix")])
            .current_dir(&ws.work_dir)
            .output()
            .unwrap();
        assert!(commit.status.success());

        // Checkpoint (orchestrator does this after agent exits)
        let sha1 = ws
            .checkpoint("fix(pipeline-e2e-test): dev-agent work")
            .await
            .expect("phase 1 checkpoint");
        // Checkpoint may return None if nothing new to commit (agent already committed)
        // That's fine — the agent's commit is what matters

        // Write handoff for phase 1
        let work1 = crate::manifest::Work {
            commits: vec![crate::manifest::CommitInfo {
                sha: sha1.clone().unwrap_or_else(|| "agent-sha".to_string()),
                message: format!("bead:{bead_id} dev-agent fix"),
                author: "dev-agent".to_string(),
            }],
            files_changed: vec!["fix.rs".to_string()],
            lines_added: 1,
            lines_removed: 0,
            diff_stat: None,
        };
        let handoff1 = crate::handoff::Handoff::new(
            0,
            "dev-agent",
            Some("staging-agent"),
            bead_id,
            "test",
            &work1,
        );
        let handoff_path = handoff1.write_to(&ws.work_dir).expect("write handoff 1");
        assert!(handoff_path.exists(), "handoff file must exist");

        // === Phase 2: staging-agent (reuse same workspace) ===
        // The reconciler reopens the bead with the new owner and dispatches again.
        // Workspace::create should reuse the existing worktree.
        let ws2 = Workspace::create(bead_id, "test-repo", &repo, true)
            .await
            .expect("phase 2 workspace create (reuse)");

        assert_eq!(
            ws.work_dir, ws2.work_dir,
            "workspace must be REUSED across pipeline phases"
        );

        // The previous agent's files must be present
        assert!(
            ws2.work_dir.join("fix.rs").exists(),
            "dev-agent's fix.rs must persist into phase 2"
        );

        // Handoff chain must be readable by the next agent
        let chain = crate::handoff::Handoff::read_chain(&ws2.work_dir);
        assert_eq!(chain.len(), 1, "handoff chain must have phase 0");
        assert_eq!(chain[0].from_agent, "dev-agent");
        assert_eq!(chain[0].to_agent.as_deref(), Some("staging-agent"));

        // Staging-agent work: add a test file
        std::fs::write(
            ws2.work_dir.join("fix_test.rs"),
            "#[test] fn test_fix() { fix(); }",
        )
        .unwrap();
        let commit = std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(&ws2.work_dir)
            .output()
            .unwrap();
        assert!(commit.status.success());
        let commit = std::process::Command::new("git")
            .args([
                "commit",
                "-m",
                &format!("bead:{bead_id} staging-agent review"),
            ])
            .current_dir(&ws2.work_dir)
            .output()
            .unwrap();
        assert!(commit.status.success());

        // Checkpoint phase 2
        let _sha2 = ws2
            .checkpoint("fix(pipeline-e2e-test): staging-agent review")
            .await
            .expect("phase 2 checkpoint");

        // === Terminal step: merge to main ===
        let branch = format!("fix/{bead_id}");
        let merge_result = merge_or_pr(&repo, &branch, bead_id, "bug").await;
        assert!(
            merge_result.is_ok(),
            "merge_or_pr must succeed for bug type, got: {:?}",
            merge_result.err()
        );
        let msg = merge_result.unwrap();
        assert!(
            msg.contains("ff-merged"),
            "bug beads should ff-merge, got: {msg}"
        );

        // Verify: both files are now in main
        assert!(
            repo.join("fix.rs").exists(),
            "dev-agent's fix.rs must be in main after merge"
        );
        assert!(
            repo.join("fix_test.rs").exists(),
            "staging-agent's fix_test.rs must be in main after merge"
        );

        // Verify: main's git log has both commits
        let log = std::process::Command::new("git")
            .args(["log", "--oneline", "--format=%s"])
            .current_dir(&repo)
            .output()
            .unwrap();
        let log_output = String::from_utf8_lossy(&log.stdout);
        assert!(
            log_output.contains("staging-agent review"),
            "main log must include staging-agent commit"
        );
        assert!(
            log_output.contains("dev-agent fix"),
            "main log must include dev-agent commit"
        );

        cleanup_git_worktree(&repo, bead_id);
    }
}
