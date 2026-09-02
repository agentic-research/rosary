//! CLI integration tests for `rsry dispatch` — the targeted-reconciler
//! delegation (rosary-451a9a).
//!
//! Background: the legacy `dispatch::run` made its own post-exit completion
//! judgment and discarded rejected status writes (`let _ = update_status(..)`),
//! leaving beads silently stuck at `dispatched` while printing a false success
//! message on the default SQLite backend. The fix routes `rsry dispatch`
//! through the reconciler's targeted pipeline (`run --once --bead` semantics),
//! so the CLI gets the same tiered verification, feedback-contract gate, and
//! retry/deadletter bookkeeping as every other dispatch surface.
//!
//! These tests are hermetic — no agent process is ever spawned. They exercise
//! the preflight failures (which must fire before the reconciler starts) and
//! the dry-run path (which proves delegation reached the reconciler's queue).

#[path = "common/mod.rs"]
mod dispatch_common;

use dispatch_common::{created_id, git_ok as git, rsry_run as run};
use std::path::Path;
use std::process::Output;

/// Fresh git repo with a SQLite bead store, isolated HOME.
fn init_repo() -> (tempfile::TempDir, tempfile::TempDir) {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q", "-b", "main"]);
    git(repo.path(), &["config", "user.email", "test@example.com"]);
    git(repo.path(), &["config", "user.name", "Test User"]);

    let repo_path = repo.path().to_string_lossy().into_owned();
    let init = run(repo.path(), home.path(), &["init", &repo_path]);
    assert!(
        init.status.success(),
        "rsry init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    (repo, home)
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The bead's current status as stored, via `bead list --status all`.
fn bead_status(repo: &Path, home: &Path, id: &str) -> String {
    let out = run(repo, home, &["bead", "list", "--status", "all", "--json"]);
    assert!(
        out.status.success(),
        "bead list failed: {}",
        stderr_of(&out)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    v["beads"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|b| b["id"] == id)
        .unwrap_or_else(|| panic!("bead {id} not in list output"))["status"]
        .as_str()
        .unwrap()
        .to_string()
}

// ---------------------------------------------------------------------------
// Preflight failures fire loudly, before any reconciler/agent work
// ---------------------------------------------------------------------------

#[test]
fn dispatch_nonexistent_bead_fails_loudly() {
    let (repo, home) = init_repo();
    let out = run(repo.path(), home.path(), &["dispatch", "zzzz-ffffff"]);
    assert!(
        !out.status.success(),
        "dispatch of nonexistent bead must fail; stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
    let err = stderr_of(&out);
    assert!(
        err.contains("not found"),
        "stderr should name the missing bead: {err}"
    );
}

#[test]
fn dispatch_refuses_bead_without_close_condition_and_leaves_it_open() {
    let (repo, home) = init_repo();
    // --force skips the create-time close-condition defaulting, producing a
    // bead with no verifiable "done" — dispatch must refuse it (the same gate
    // MCP tool_dispatch enforces), BEFORE any status write.
    let create = run(
        repo.path(),
        home.path(),
        &[
            "bead",
            "create",
            "gateless bead",
            "--files",
            "src/lib.rs",
            "--force",
        ],
    );
    assert!(
        create.status.success(),
        "forced create failed: {}",
        stderr_of(&create)
    );
    let id = created_id(&create);

    let out = run(repo.path(), home.path(), &["dispatch", &id]);
    assert!(
        !out.status.success(),
        "dispatch must refuse a close-condition-less bead"
    );
    let err = stderr_of(&out);
    assert!(
        err.contains("close condition"),
        "stderr should explain the missing close condition: {err}"
    );
    // Preflight failure must leave the bead untouched — not stuck at dispatched.
    assert_eq!(
        bead_status(repo.path(), home.path(), &id),
        "open",
        "a refused dispatch must not mutate bead status"
    );
}

#[test]
fn dispatch_isolate_false_is_deprecated_and_ignored() {
    let (repo, home) = init_repo();
    let out = run(
        repo.path(),
        home.path(),
        &["dispatch", "zzzz-ffffff", "--isolate=false"],
    );
    let err = stderr_of(&out);
    // Warning fires even though the dispatch itself fails preflight.
    assert!(
        err.contains("isolate") && err.contains("deprecated"),
        "expected a deprecation warning for --isolate=false: {err}"
    );
}

// ---------------------------------------------------------------------------
// Delegation: dispatch routes through the reconciler's targeted pipeline
// ---------------------------------------------------------------------------

#[test]
fn dispatch_dry_run_routes_through_targeted_reconciler() {
    let (repo, home) = init_repo();
    let create = run(
        repo.path(),
        home.path(),
        &[
            "bead",
            "create",
            "dispatchable bead",
            "--files",
            "src/lib.rs",
            "--test-files",
            "tests/smoke.rs",
        ],
    );
    assert!(
        create.status.success(),
        "create failed: {}",
        stderr_of(&create)
    );
    let id = created_id(&create);

    let out = run(repo.path(), home.path(), &["dispatch", &id, "--dry-run"]);
    let err = stderr_of(&out);
    assert!(
        out.status.success(),
        "dry-run dispatch should succeed: {err}"
    );
    // Proof of delegation: the reconciler's own targeted-mode banner and
    // dry-run dispatch line, not the legacy dispatch::run output.
    assert!(
        err.contains(&format!("targeting bead {id}")),
        "expected the reconciler's targeted-mode banner: {err}"
    );
    assert!(
        err.contains("[dry-run] would dispatch"),
        "expected the reconciler's dry-run dispatch line: {err}"
    );
    // Dry run mutates nothing.
    assert_eq!(
        bead_status(repo.path(), home.path(), &id),
        "open",
        "dry-run must not mutate bead status"
    );
}
