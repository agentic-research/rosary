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
    let ws = Workspace::create("test-1", "repo", tmp.path(), false, true)
        .await
        .unwrap();
    assert_eq!(ws.vcs, VcsKind::None);
    // work_dir should match the input path (expand_path, no symlink resolution)
    assert_eq!(ws.work_dir, tmp.path().to_path_buf());
    assert!(ws.exec_handle.is_none());
}

#[tokio::test]
async fn workspace_create_no_vcs_with_isolate_errors() {
    let tmp = tempfile::TempDir::new().unwrap();
    // No .jj or .git — isolate=true must error, not silently fall back
    let result = Workspace::create("test-1", "repo", tmp.path(), true, true).await;
    assert!(
        result.is_err(),
        "Workspace::create with isolate=true must fail when no VCS is available, \
         not silently fall back to in-place"
    );
}

#[tokio::test]
async fn workspace_create_no_vcs_without_isolate_falls_through() {
    let tmp = tempfile::TempDir::new().unwrap();
    // No .jj or .git — isolate=false allows in-place execution
    let ws = Workspace::create("test-1", "repo", tmp.path(), false, true)
        .await
        .unwrap();
    assert_eq!(ws.vcs, VcsKind::None);
    assert_eq!(ws.work_dir, tmp.path().to_path_buf());
}

/// rosary-3b8a9b / rosary-d1f5d8: two concurrent `Workspace::create` calls
/// for the SAME bead used to silently share one `work_dir`, so the losing
/// dispatch's handoff writes clobbered the winner's with no error on either
/// side. `reuse: false` on the second call must refuse loudly instead,
/// naming the workspace already holding it — and must leave the first
/// workspace completely untouched.
#[tokio::test]
async fn workspace_create_refuses_concurrent_reuse_when_disallowed() {
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
    std::process::Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(repo)
        .output()
        .unwrap();

    // First dispatch claims the workspace.
    let ws1 = Workspace::create("shared-bead", "repo", repo, true, true)
        .await
        .expect("first create must succeed");
    let marker = ws1.work_dir.join("dispatch-a-handoff.json");
    std::fs::write(&marker, "dispatch A's in-flight state").unwrap();

    // Second, concurrent dispatch for the SAME bead — must refuse, not
    // silently attach and share the worktree.
    let result = Workspace::create("shared-bead", "repo", repo, true, false).await;
    let err = match result {
        Ok(_) => panic!("second create with reuse=false must refuse, not succeed"),
        Err(e) => e,
    };
    let msg = format!("{err:#}");
    assert!(
        msg.contains(&ws1.work_dir.display().to_string()),
        "error must name the existing workspace path: {msg}"
    );

    // The first dispatch's workspace and in-flight state are untouched — no
    // interleaving, nothing lost.
    assert!(ws1.work_dir.exists());
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap(),
        "dispatch A's in-flight state",
        "the losing call must not have clobbered the holder's state"
    );

    // Sanity: reuse=true against the same existing workspace still works —
    // the fix only changes behavior when a caller explicitly asks for it.
    let ws2 = Workspace::create("shared-bead", "repo", repo, true, true)
        .await
        .expect("reuse=true must still attach to the existing workspace");
    assert_eq!(ws1.work_dir, ws2.work_dir);

    sweep::cleanup_git_worktree(repo, "shared-bead");
}

#[tokio::test]
async fn workspace_provision_and_exec() {
    use crate::backend::tests::MockProvider;

    let tmp = tempfile::TempDir::new().unwrap();
    let mock = MockProvider::new();

    let mut ws = Workspace::create("test-1", "repo", tmp.path(), false, true)
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

/// Regression (GAP 2): Workspace::cleanup() must not panic for VcsKind::None.
/// cleanup() is called in dispatch::spawn() when spawn_agent fails after workspace
/// creation. If cleanup() is removed or panics, orphaned worktrees accumulate.
#[tokio::test]
async fn workspace_cleanup_noop_is_safe() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ws = Workspace::create("gap2-noop", "repo", tmp.path(), false, true)
        .await
        .unwrap();
    assert_eq!(ws.vcs, VcsKind::None);
    // Must not panic and must leave the work_dir intact (it's the repo dir itself)
    let work_dir = ws.work_dir.clone();
    ws.cleanup();
    assert!(
        work_dir.exists(),
        "cleanup on VcsKind::None must not remove the repo dir"
    );
}

/// Regression: git worktree must branch from HEAD, not an orphan.
/// Bug: worktree only had the `.beads/` init commit, no source code.
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

    let wt_path = sweep::create_git_worktree(repo, "test-regression").await;
    assert!(wt_path.is_ok(), "worktree creation should succeed");
    let wt_path = wt_path.unwrap();

    assert!(
        wt_path.join("src.rs").exists(),
        "worktree must contain source files from HEAD, not just .beads/"
    );

    sweep::cleanup_git_worktree(repo, "test-regression");
}

#[tokio::test]
async fn workspace_exec_without_provision_uses_local() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ws = Workspace::create("test-1", "repo", tmp.path(), false, true)
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

    let mut ws = Workspace::create("test-1", "repo", tmp.path(), false, true)
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

    let ws = Workspace::create("test-1", "repo", tmp.path(), false, true)
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
async fn setup_colocated_repo() -> (tempfile::TempDir, std::path::PathBuf) {
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
    let ws = Workspace::create(bead_id, "test-repo", &repo, true, true)
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
        std::path::PathBuf::from(String::from_utf8_lossy(&toplevel.stdout).trim().to_string());
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
    sweep::cleanup_git_worktree(&repo, bead_id);

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

    let ws = Workspace::create("beads-access-test", "test-repo", &repo, true, true)
        .await
        .expect("workspace create must succeed");

    assert_eq!(ws.vcs, VcsKind::Git);

    // .beads/ should be present in the worktree (branched from HEAD)
    assert!(
        ws.work_dir.join(".beads").join("marker").exists(),
        ".beads/ must be accessible in the git worktree — \
         agents need this to reach Dolt"
    );

    sweep::cleanup_git_worktree(&repo, "beads-access-test");
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
    let ws_a = Workspace::create("agent-alpha", "test-repo", &repo, true, true)
        .await
        .expect("workspace A must succeed");
    let ws_b = Workspace::create("agent-beta", "test-repo", &repo, true, true)
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
    sweep::cleanup_git_worktree(&repo, "agent-alpha");
    sweep::cleanup_git_worktree(&repo, "agent-beta");
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
    let result = sweep::create_git_worktree(&repo, "stale-bead").await;
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

    sweep::cleanup_git_worktree(&repo, "stale-bead");
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

    let result = Workspace::create("test-no-fallback", "repo", &repo, true, true).await;
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
    let ws = Workspace::create(bead_id, "test-repo", &repo, true, true)
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
        None,
    );
    let handoff_path = handoff1.write_to(&ws.work_dir).expect("write handoff 1");
    assert!(handoff_path.exists(), "handoff file must exist");

    // === Phase 2: staging-agent (reuse same workspace) ===
    // The reconciler reopens the bead with the new owner and dispatches again.
    // Workspace::create should reuse the existing worktree.
    let ws2 = Workspace::create(bead_id, "test-repo", &repo, true, true)
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
    let merge_result = sweep::merge_or_pr(&repo, &branch, bead_id, "bug").await;
    assert!(
        merge_result.is_ok(),
        "merge_or_pr must succeed for bug type, got: {:?}",
        merge_result.err()
    );
    let result = merge_result.unwrap();
    // In test repos without a remote, push fails gracefully — just check it ran
    assert!(
        !result.message.is_empty(),
        "merge_or_pr must return a message"
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

    sweep::cleanup_git_worktree(&repo, bead_id);
}

#[test]
fn parse_owner_repo_ssh() {
    let (owner, repo) =
        sweep::parse_owner_repo("git@github.com:agentic-research/rosary.git").unwrap();
    assert_eq!(owner, "agentic-research");
    assert_eq!(repo, "rosary");
}

#[test]
fn parse_owner_repo_https() {
    let (owner, repo) =
        sweep::parse_owner_repo("https://github.com/agentic-research/rosary.git").unwrap();
    assert_eq!(owner, "agentic-research");
    assert_eq!(repo, "rosary");
}

#[test]
fn parse_owner_repo_https_no_git_suffix() {
    let (owner, repo) =
        sweep::parse_owner_repo("https://github.com/agentic-research/rosary").unwrap();
    assert_eq!(owner, "agentic-research");
    assert_eq!(repo, "rosary");
}

#[test]
fn parse_owner_repo_invalid() {
    assert!(sweep::parse_owner_repo("https://gitlab.com/foo/bar").is_err());
}

#[test]
fn thread_branch_name_simple() {
    assert_eq!(
        sweep::thread_branch_name("rosary", "unified-query"),
        "rosary/unified-query"
    );
}

#[test]
fn thread_branch_name_slugifies() {
    assert_eq!(
        sweep::thread_branch_name("rosary", "GitHub App Auth"),
        "rosary/github-app-auth"
    );
}

#[test]
fn thread_branch_name_custom_prefix() {
    assert_eq!(
        sweep::thread_branch_name("agent", "core/pipeline"),
        "agent/core-pipeline"
    );
}

/// rosary-efd300: the isolation contract has NO degradation path. With a real
/// VCS and `isolate=true`, the agent's work_dir must never be the main checkout
/// itself — a shared/symlink-aliased tree is one tree, so an agent working
/// in-place mutates every other agent's view (the LLO data-loss root cause).
///
/// Pins the invariant that the removed dead `Err(e) => …falling back to
/// in-place` arms cannot silently regrow.
#[tokio::test]
async fn isolated_workspace_is_never_the_main_checkout() {
    let tmp = tempfile::TempDir::new().unwrap();
    let repo = tmp.path();
    // Minimal real git repo with one commit (worktree add needs a HEAD).
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .current_dir(repo)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap()
    };
    git(&["init", "-q", "."]);
    std::fs::write(repo.join("f.txt"), "x").unwrap();
    git(&["add", "f.txt"]);
    git(&["commit", "-qm", "seed"]);

    let ws = Workspace::create("iso-1", "repo", repo, true, true)
        .await
        .expect("isolation must succeed on a real git repo");

    assert_ne!(
        ws.work_dir.canonicalize().unwrap(),
        repo.canonicalize().unwrap(),
        "isolated workspace must NOT be the main checkout — that is the \
         in-place degradation this contract forbids"
    );
    assert!(
        ws.work_dir.exists(),
        "isolated work_dir must actually exist on disk"
    );

    // Clean up: the workspace root lives outside `tmp`, so dropping the TempDir
    // does not reclaim it (rosary-a63159).
    sweep::cleanup_git_worktree(repo, "iso-1");
}

/// rosary-a63159: `workspace_dir` must be PURE. It used to `create_dir_all` its
/// parent merely to compute a path, so every caller — including a mere
/// existence check — created a directory. With tempdir repo paths that leaked
/// one permanent dir per test run into the developer's real $HOME (>20k of them
/// had accumulated since March, invisible because they are dot-prefixed).
///
/// Deliberately sets **no environment variable**. The first draft of this test
/// did (`unsafe { set_var(WORKTREE_ROOT_ENV, ..) }` with a "single-threaded test
/// scope" claim that is simply false — `cargo test` is multi-threaded, and std
/// says the only sound option is not to call `set_var` in a multi-threaded
/// program at all). Concurrent tests read the same variable, and its `TempDir`
/// was dropped out from under them. Not needed: `cfg!(test)` already redirects
/// the default root to a temp dir, and purity is observable without redirection.
#[test]
fn workspace_dir_is_pure_and_repo_identity_keyed() {
    let repo = tempfile::TempDir::new().unwrap();
    let p = crate::workspace::workspace_dir(repo.path(), "bead-1");

    // PURE: asking where a workspace WOULD go creates nothing.
    assert!(
        !p.exists(),
        "workspace_dir must not create the workspace dir"
    );
    assert!(
        !p.parent().unwrap().exists(),
        "workspace_dir must not create the root either — that was the leak"
    );

    // IDENTITY-KEYED: two repos sharing a basename must NOT share a root.
    // Previously both mapped to <root>/api/<id>, so Workspace::create's
    // reuse-if-exists branch could hand repo B's workspace to an agent in A.
    let holder = tempfile::TempDir::new().unwrap();
    let a = holder.path().join("a").join("api");
    let b = holder.path().join("b").join("api");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let pa = crate::workspace::workspace_dir(&a, "same-bead");
    let pb = crate::workspace::workspace_dir(&b, "same-bead");
    assert_ne!(
        pa, pb,
        "same-basename repos must not collide onto one workspace path"
    );
    assert!(
        pa.to_string_lossy().contains("api-") && pb.to_string_lossy().contains("api-"),
        "basename should remain readable in the key: {pa:?} {pb:?}"
    );
}

/// The root key is normalized: a trailing separator is not a different repo.
/// The key used to hash `to_string_lossy()`, so `/a/b/` and `/a/b` — which are
/// `Path`-EQUAL everywhere else in this codebase — landed in different roots.
#[test]
fn workspace_root_ignores_trailing_separator() {
    let repo = tempfile::TempDir::new().unwrap();
    let with_sep = std::path::PathBuf::from(format!("{}/", repo.path().display()));
    assert_eq!(
        crate::workspace::workspace_root(repo.path()),
        crate::workspace::workspace_root(&with_sep),
        "trailing separator must not fork the workspace root"
    );
}

/// rosary-617010 consistency: `scanner::canonicalize_repo_path` exists so path
/// aliases of ONE physical repo compare equal. The workspace root must agree —
/// otherwise the same repo reached via a symlink gets a second "isolated" root
/// while git's worktree registry (which lives in the physical `.git`) sees one.
#[test]
fn workspace_root_is_alias_stable() {
    let holder = tempfile::TempDir::new().unwrap();
    let real = holder.path().join("real");
    std::fs::create_dir_all(&real).unwrap();
    let alias = holder.path().join("alias");
    std::os::unix::fs::symlink(&real, &alias).unwrap();

    assert_eq!(
        crate::workspace::workspace_root(&real),
        crate::workspace::workspace_root(&alias),
        "a symlink alias of one physical repo must map to ONE workspace root"
    );
}

/// rosary-a63159 (the relocated leak): creating and then cleaning up a workspace
/// must leave **no** directory behind — not in `$HOME`, not in `$TMPDIR`.
///
/// The first fix only stopped `workspace_dir` from mkdir-ing; `ensure_workspace_root`
/// still created a per-repo root on every creation attempt that nothing ever
/// removed, so the same ~9 dirs/run simply moved from `$HOME` to `$TMPDIR`.
#[tokio::test]
async fn workspace_lifecycle_leaves_no_directory_behind() {
    let tmp = tempfile::TempDir::new().unwrap();
    let repo = tmp.path();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .current_dir(repo)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap()
    };
    git(&["init", "-q", "."]);
    std::fs::write(repo.join("f.txt"), "x").unwrap();
    git(&["add", "f.txt"]);
    git(&["commit", "-qm", "seed"]);

    let root = crate::workspace::workspace_root(repo);
    assert!(!root.exists(), "precondition: root must not exist yet");

    let ws = Workspace::create("leak-1", "repo", repo, true, true)
        .await
        .expect("isolation must succeed");
    assert!(ws.work_dir.starts_with(&root));

    sweep::cleanup_git_worktree(repo, "leak-1");
    assert!(
        !root.exists(),
        "the per-repo workspace root must be pruned once empty — leaving it is \
         the leak, merely relocated: {}",
        root.display()
    );
}

/// A creation attempt that FAILS must not leave a root behind either — that was
/// the largest single source of the relocated leak, since the failure paths have
/// no cleanup call at all.
#[tokio::test]
async fn failed_isolation_leaves_no_directory_behind() {
    let tmp = tempfile::TempDir::new().unwrap();
    let repo = tmp.path().canonicalize().unwrap();
    // `.git` exists so detect_vcs says Git, but it is not a real repo, so
    // `git worktree add` fails.
    std::fs::create_dir(repo.join(".git")).unwrap();

    let root = crate::workspace::workspace_root(&repo);
    assert!(
        Workspace::create("leak-2", "repo", &repo, true, true)
            .await
            .is_err()
    );
    assert!(
        !root.exists(),
        "a failed workspace creation must create nothing: {}",
        root.display()
    );
}

/// Build a **git-backed**, non-colocated jj repo with one committed file,
/// entirely through jj-lib — no `jj` binary.
///
/// Git-backed is the load-bearing word. `leyline_vcs::JjIntegration::init`
/// (what the first version of this test used) calls `Workspace::init_simple`,
/// which builds a **SimpleBackend** repo — a shape that exists only in tests.
/// jj-lib gates its Git backend behind `#[cfg(feature = "git")]` and
/// leyline-vcs used to compile it out, so `JjIntegration::open` failed with
/// "Cannot read the repo" on every repo `jj git init` produces. A SimpleBackend
/// fixture therefore passed **exactly where production failed**
/// (ley-line-open #257).
///
/// Non-colocated matters too: `detect_vcs` routes anything with a `.git` to the
/// git-worktree path, so a colocated fixture would never reach the jj branch and
/// this test would silently assert nothing about jj.
fn git_backed_jj_repo(repo: &std::path::Path) {
    use jj_lib::backend::TreeValue;
    use jj_lib::config::StackedConfig;
    use jj_lib::merged_tree::MergedTree;
    use jj_lib::ref_name::WorkspaceName;
    use jj_lib::repo::Repo as _;
    use jj_lib::repo_path::RepoPathBuf;
    use jj_lib::settings::UserSettings;
    use jj_lib::tree_builder::TreeBuilder;
    use jj_lib::workspace::{Workspace as JjWorkspace, default_working_copy_factories};
    use pollster::FutureExt as _;

    std::fs::create_dir_all(repo).unwrap();
    let settings = UserSettings::from_config(StackedConfig::with_defaults()).unwrap();
    JjWorkspace::init_internal_git(&settings, repo)
        .block_on()
        .expect("init_internal_git — needs jj-lib's `git` feature");

    // Commit one file and check it out in the default workspace, so the repo
    // looks like a checkout someone has been working in and we can assert the
    // agent's workspace receives the content.
    let ws = JjWorkspace::load(
        &settings,
        repo,
        &Default::default(),
        &default_working_copy_factories(),
    )
    .expect("load git-backed workspace");
    let base = ws.repo_loader().load_at_head().block_on().unwrap();
    let store = base.store().clone();

    let file_id = store
        .write_file(
            &RepoPathBuf::from_internal_string("hello.txt").unwrap(),
            &mut b"hello".as_slice(),
        )
        .block_on()
        .unwrap();
    let mut builder = TreeBuilder::new(store.clone(), store.empty_tree_id().clone());
    builder.set(
        RepoPathBuf::from_internal_string("hello.txt").unwrap(),
        TreeValue::File {
            id: file_id,
            executable: false,
            copy_id: jj_lib::backend::CopyId::placeholder(),
        },
    );
    let tree_id = builder.write_tree().block_on().unwrap();
    let tree = MergedTree::resolved(store.clone(), tree_id);

    let mut tx = base.start_transaction();
    let seed = tx
        .repo_mut()
        .new_commit(vec![store.root_commit_id().clone()], tree)
        .set_description("seed")
        .write()
        .block_on()
        .unwrap();
    tx.repo_mut()
        .check_out(WorkspaceName::DEFAULT.to_owned(), &seed)
        .block_on()
        .unwrap();
    tx.repo_mut().rebase_descendants().block_on().unwrap();
    tx.commit("seed").block_on().unwrap();
}

/// Workspace names registered in the repo's shared jj store.
fn jj_workspace_names(repo: &std::path::Path) -> Vec<String> {
    leyline_vcs::JjIntegration::open(repo)
        .expect("open git-backed jj repo")
        .workspace_names()
        .expect("list workspaces")
}

/// The Git backend must be in the **normal** dependency graph, not just the
/// test one.
///
/// This is the smoking gun from the review that falsified this change: `grep
/// 'name = "gix"' Cargo.lock` had **no match**, because leyline-vcs took jj-lib
/// with `default-features = false` and jj-lib gates its Git backend behind
/// `#[cfg(feature = "git")]`. `JjIntegration::open` then failed with "Cannot
/// read the repo" on every repo `jj git init` produces.
///
/// The behavioural tests below cannot police this on their own. rosary declares
/// jj-lib as a **dev-dependency** with `features = ["git"]` to build a
/// git-backed fixture, and cargo unifies features across the test build — so
/// under `cargo test` leyline-vcs would get the Git backend even if its own
/// manifest stopped asking for it. `cargo build` does not resolve
/// dev-dependencies, so the shipped `rsry` would be broken while the suite
/// stayed green: exactly the review's finding, one level up.
///
/// `Cargo.lock` records the resolved **normal** graph, so checking it here
/// closes that hole. A repin of `leyline-vcs` that loses the backend fails this
/// test.
#[test]
fn workspace_root_pins_the_jj_git_backend() {
    let lock = include_str!("../../Cargo.lock");
    assert!(
        lock.lines().any(|l| l == r#"name = "gix""#),
        "gix is absent from Cargo.lock — jj-lib's Git backend has been compiled \
         out again, which makes every non-colocated jj repo unopenable \
         (ley-line-open #257). Check leyline-vcs's jj-lib feature list."
    );
}

/// SPIKE PROOF (rosary-efd300): jj workspace creation through jj-lib, on the
/// repo shape production actually has.
///
/// The other 30 workspace tests do NOT answer this — `detect_vcs_jj` only
/// mkdirs a fake `.jj`, and colocated repos route to git worktree. So they
/// prove the git paths still work, not that the jj-lib swap functions.
///
/// This asserts three things, each of which was false at some point in this
/// change's life:
///   1. a **git-backed** jj repo can be opened at all (it could not — the Git
///      backend was compiled out of leyline-vcs; ley-line-open #257);
///   2. the workspace is **registered** in the shared jj store under its
///      bead-derived name — an ordinary directory cannot satisfy this;
///   3. the workspace **contains the repo's files**. `jj-lib`'s
///      `init_workspace_with_existing_repo` checks out the EMPTY ROOT COMMIT,
///      so before #257 this produced a directory holding nothing but `.jj`.
///      An agent needs source, not a registered empty dir, and nothing in the
///      original test would have noticed.
///
/// It writes under `RSRY_WORKTREE_ROOT` (rosary-a63159, #405), so it neither
/// touches `$HOME` nor leaves state that makes a SECOND `cargo test` run fail —
/// which the first version of this test did, deterministically.
#[tokio::test]
async fn jj_lib_creates_a_real_isolated_workspace_without_the_jj_binary() {
    let tmp = tempfile::TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    git_backed_jj_repo(&repo);
    assert_eq!(jj_workspace_names(&repo), vec!["default".to_string()]);

    let ws = Workspace::create("spike-1", "repo", &repo, true, true)
        .await
        .expect("jj-lib workspace creation must succeed on a GIT-BACKED jj repo");

    assert_eq!(ws.vcs, VcsKind::Jj, "must have taken the jj path");
    assert!(ws.work_dir.exists(), "work_dir must exist on disk");
    assert_ne!(
        ws.work_dir.canonicalize().unwrap(),
        repo.canonicalize().unwrap(),
        "must be isolated, not the main checkout"
    );

    // (2) REGISTERED in the shared jj store.
    let after = jj_workspace_names(&repo);
    assert!(
        after.iter().any(|n| n == "fix-spike-1"),
        "workspace must be registered under its bead-derived name, got {after:?}"
    );

    // (3) POPULATED. The whole point: an agent needs source.
    assert_eq!(
        std::fs::read_to_string(ws.work_dir.join("hello.txt")).unwrap(),
        "hello",
        "the agent's workspace must contain the repo's files, not just `.jj`"
    );
}

/// Teardown must never leave the bead **stuck**.
///
/// `add_workspace` rejects a duplicate name, so the dangerous state is "name
/// still registered in the jj store, directory deleted": the next dispatch for
/// this bead then fails on the duplicate name, forever. `cleanup_jj_workspace`
/// therefore only deletes the directory once the forget succeeded — so either
/// outcome (cleaned, or left in place) is one the next dispatch can recover
/// from. This asserts the property that actually matters and holds in both
/// worlds: **the same bead can be dispatched again**.
#[tokio::test]
async fn jj_workspace_teardown_never_wedges_the_next_dispatch() {
    let tmp = tempfile::TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    git_backed_jj_repo(&repo);

    let ws = Workspace::create("spike-2", "repo", &repo, true, true)
        .await
        .expect("first create");
    let work_dir = ws.work_dir.clone();
    ws.teardown(&crate::backend::LocalProvider).await.unwrap();

    // Consistency: the name is registered if and only if the directory remains.
    let still_registered = jj_workspace_names(&repo).iter().any(|n| n == "fix-spike-2");
    assert_eq!(
        still_registered,
        work_dir.exists(),
        "teardown must not leave the jj name registered with the directory gone — \
         that is the state that makes the next dispatch fail permanently"
    );

    let again = Workspace::create("spike-2", "repo", &repo, true, true)
        .await
        .expect("re-dispatch of the same bead must succeed after teardown");
    assert!(again.work_dir.exists());
}
