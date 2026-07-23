//! Workspace lifecycle: create, provision, exec, teardown.

use anyhow::Result;
use std::path::Path;

use crate::backend::{ComputeProvider, ExecHandle, ExecResult, ProvisionOpts};

use super::sweep::{
    cleanup_git_worktree, cleanup_jj_workspace, create_git_worktree, create_jj_workspace,
};
use super::{VcsKind, Workspace};

impl Workspace {
    /// Create a new workspace with code isolation.
    ///
    /// `isolate` is the whole contract, and there is **no degradation path**:
    ///
    /// - `isolate: true` — detect the repo's VCS and create an isolated
    ///   workspace (jj workspace for jj, worktree for git). If that fails, or
    ///   the repo has no VCS to isolate with, this **fails loudly**. It never
    ///   silently runs the agent in the main checkout: a shared/symlink-aliased
    ///   tree (`~/github` → `~/remotes`) is one tree, so an agent branch-switch
    ///   there mutates every other agent's view — the root cause of a real
    ///   data-loss incident.
    /// - `isolate: false` — caller explicitly opted out; run in place.
    ///
    /// Note jj and git are alternatives chosen by *detection*, not a fallback
    /// chain: a jj repo never degrades to a git worktree.
    ///
    /// (The previous doc claimed "tries jj first, falls back to git worktree,
    /// then in-place". All three clauses were false, and it read as though
    /// rosary silently degrades into the exact data-loss case above.)
    pub async fn create(id: &str, repo: &str, repo_path: &Path, isolate: bool) -> Result<Self> {
        // Expand tilde but do NOT canonicalize — that resolves symlinks
        // and breaks paths like ~/github → ~/remotes.
        let repo_path = crate::scanner::expand_path(repo_path);
        let repo_path = repo_path.as_path();

        let vcs = if isolate {
            super::detect_vcs(repo_path)
        } else {
            VcsKind::None
        };

        // Reuse existing workspace if it exists (resume after agent death).
        // Legacy-aware: workspaces created before the root re-key (rosary-a63159)
        // must still be found, or resume cuts a SECOND worktree on a fresh
        // fix/{id} branch and abandons the first agent's unmerged work.
        if let Some(existing_ws) = super::existing_workspace_dir(repo_path, id)
            && vcs != VcsKind::None
        {
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
            // `vcs` is only Jj/Git when `isolate` is true (see the binding
            // above), so isolation was definitionally requested here: a failure
            // is fatal, never a quiet drop into the main checkout. The former
            // `Err(e) if isolate` guard plus its in-place arm was DEAD CODE —
            // the guard always matched, so the fallback below it could not run.
            // It survived only as something scary to read.
            VcsKind::Jj => (
                create_jj_workspace(repo_path, id).await.map_err(|e| {
                    anyhow::anyhow!(
                        "workspace isolation failed for {id}: jj workspace creation failed: {e}"
                    )
                })?,
                VcsKind::Jj,
            ),
            VcsKind::Git => (
                create_git_worktree(repo_path, id).await.map_err(|e| {
                    anyhow::anyhow!(
                        "workspace isolation failed for {id}: git worktree creation failed: {e}"
                    )
                })?,
                VcsKind::Git,
            ),
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
