//! rosary-caa952: MCP `rsry_dispatch` must fail CLOSED when it cannot isolate.
//!
//! Before the fix, a `Workspace::create` error was printed and swallowed and
//! `work_dir` fell back to the repo root — an agent dispatched into the main
//! (shared, possibly dirty) checkout, the incident class rosary-ea6c7f closed.
//! `isolate=false` was honored the same way, although the CLI already refuses it.
//!
//! The provider is a name no registry knows, so no test here can ever spawn a
//! real agent: the pre-fix code path reaches provider resolution and errors
//! THERE ("resolving provider"), the fixed path errors at isolation. Asserting
//! on which error surfaces is what distinguishes the two.

use super::*;
use serde_json::json;

const NO_SUCH_PROVIDER: &str = "no-such-provider-caa952";

/// A repo dir with a bead store holding one dispatchable bead, but NO `.git`
/// or `.jj` — so workspace isolation cannot succeed.
async fn unversioned_repo_with_bead() -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let beads_dir = tmp.path().join(".beads");
    std::fs::create_dir_all(&beads_dir).unwrap();
    let store = crate::bead_sqlite::connect_bead_store(&beads_dir)
        .await
        .unwrap();
    store
        .create_bead(
            "CAA952",
            "isolation fixture",
            "Fixture bead for the fail-closed dispatch test. Close condition: \
             `cargo test -p fixture`.",
            1,
            "task",
        )
        .await
        .unwrap();
    let path = tmp.path().to_string_lossy().to_string();
    (tmp, path)
}

async fn bead_status(repo_path: &str) -> String {
    let beads_dir = std::path::Path::new(repo_path).join(".beads");
    let store = crate::bead_sqlite::connect_bead_store(&beads_dir)
        .await
        .unwrap();
    store
        .get_bead("CAA952", &repo_name_from_path(repo_path))
        .await
        .unwrap()
        .expect("fixture bead exists")
        .status
}

fn assert_refused_at_isolation(result: Result<Value>, repo_path: &str) {
    let err = result.expect_err("dispatch must be refused when it cannot isolate");
    let chain = format!("{err:#}");
    assert!(
        chain.contains("workspace isolation failed"),
        "the refusal must come from isolation, not a later step — got: {chain}"
    );
    // Nothing ran in the main checkout: no agent stream log was created there.
    assert!(
        !std::path::Path::new(repo_path)
            .join(crate::dispatch::STREAM_LOG_FILENAME)
            .exists(),
        "no agent may run in the repo root"
    );
}

#[tokio::test]
async fn dispatch_refuses_when_workspace_cannot_be_isolated() {
    let (_tmp, repo_path) = unversioned_repo_with_bead().await;
    let before = bead_status(&repo_path).await;

    let result = tool_dispatch(
        &json!({
            "bead_id": "CAA952",
            "repo_path": repo_path,
            "provider": NO_SUCH_PROVIDER,
        }),
        "rosary.toml",
    )
    .await;

    assert_refused_at_isolation(result, &repo_path);
    assert_eq!(
        bead_status(&repo_path).await,
        before,
        "a refused dispatch must not move the bead"
    );
}

#[tokio::test]
async fn isolate_false_is_ignored_not_honored() {
    let (_tmp, repo_path) = unversioned_repo_with_bead().await;

    let result = tool_dispatch(
        &json!({
            "bead_id": "CAA952",
            "repo_path": repo_path,
            "provider": NO_SUCH_PROVIDER,
            "isolate": false,
        }),
        "rosary.toml",
    )
    .await;

    // isolate=false used to skip isolation and run in root; now it is ignored,
    // so this unversioned repo is refused at isolation like any other.
    assert_refused_at_isolation(result, &repo_path);
}

#[tokio::test]
async fn dispatch_workspace_is_never_the_main_checkout() {
    let repo = crate::testutil::TestRepo::new();
    let root = repo.path().to_path_buf();

    for isolate_requested in [true, false] {
        let id = format!("caa952-happy-{isolate_requested}");
        let ws = dispatch_workspace(&id, "repo", &root, isolate_requested)
            .await
            .expect("a git repo isolates");
        assert_ne!(ws.work_dir, root, "work_dir must not be the main checkout");
        assert!(
            !ws.work_dir.starts_with(&root),
            "work_dir must live outside the checkout, got {}",
            ws.work_dir.display()
        );
        assert_eq!(ws.vcs, crate::workspace::VcsKind::Git);
        ws.cleanup();
    }
}
