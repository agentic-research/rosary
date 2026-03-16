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

/// Detect which VCS is available in a repo.
pub fn detect_vcs(repo_path: &Path) -> VcsKind {
    if repo_path.join(".jj").exists() {
        VcsKind::Jj
    } else if repo_path.join(".git").exists() {
        VcsKind::Git
    } else {
        VcsKind::None
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
                Err(e) => {
                    eprintln!("[workspace] jj isolation failed ({e}), falling back to in-place");
                    (repo_path.to_path_buf(), VcsKind::None)
                }
            },
            VcsKind::Git => match create_git_worktree(repo_path, id).await {
                Ok(path) => (path, VcsKind::Git),
                Err(e) => {
                    eprintln!("[workspace] git worktree failed ({e}), falling back to in-place");
                    (repo_path.to_path_buf(), VcsKind::None)
                }
            },
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

    /// Checkpoint the workspace: jj commit + bookmark, return change ID.
    ///
    /// Call this BEFORE cleanup to preserve the agent's work in the shared
    /// jj repo history. In a colocated repo, the agent's git commits are
    /// already visible to jj, but this adds a bookmark for tracking.
    pub async fn checkpoint(&self, message: &str) -> Result<Option<String>> {
        if self.vcs != VcsKind::Jj {
            return Ok(None);
        }
        self.jj_commit(message).await?;
        let change_id = self.jj_change_id().await?;
        let bookmark = format!("fix/{}", self.id);
        self.jj_bookmark(&bookmark).await?;
        Ok(change_id)
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
/// Uses `{repo_parent}/.rsry-workspaces/{id}` as a deterministic sibling path.
fn workspace_dir(repo_path: &Path, id: &str) -> PathBuf {
    let parent = repo_path.parent().unwrap_or(repo_path);
    let ws_root = parent.join(".rsry-workspaces");
    // Best-effort mkdir — callers handle the actual VCS error if this fails
    let _ = std::fs::create_dir_all(&ws_root);
    ws_root.join(id)
}

/// Create a git worktree for isolated work.
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

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git worktree add failed: {stderr}");
    }

    eprintln!(
        "[workspace] created git worktree: {}",
        worktree_path.display()
    );
    Ok(worktree_path)
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
    for repo_path in repo_paths {
        let ws_root = repo_path
            .parent()
            .unwrap_or(repo_path)
            .join(".rsry-workspaces");
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
    fn detect_vcs_jj_preferred_over_git() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".jj")).unwrap();
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
        // jj should win when both exist
        assert_eq!(detect_vcs(tmp.path()), VcsKind::Jj);
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
    async fn workspace_create_no_vcs_falls_through() {
        let tmp = tempfile::TempDir::new().unwrap();
        // No .jj or .git — even with isolate=true, falls to None
        let ws = Workspace::create("test-1", "repo", tmp.path(), true)
            .await
            .unwrap();
        assert_eq!(ws.vcs, VcsKind::None);
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
        std::fs::write(repo.join("src.rs"), "fn main() {}").unwrap();
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(repo)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(repo)
            .output()
            .unwrap();

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
}
