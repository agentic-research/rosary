//! rosary-560953 — CLI bead ops outside a repo must never fabricate a store.
//!
//! Root cause under test: `--repo` defaults to ".", repo discovery silently
//! falls back to the raw cwd, `resolve_beads_dir` returns `cwd/.beads` even
//! when nothing is there, and `connect_bead_store` creates an empty beads.db
//! on connect. Net effect: `rsry bead search` from a non-repo cwd printed
//! "No beads matching" (exit 0) against a phantom store it had just created,
//! and `rsry bead create` could black-hole work items into a store no scan
//! reads.
//!
//! These tests need no Dolt — stores are SQLite via `rsry init`. HOME is
//! isolated per test so the global registry never touches the real ~/.rsry.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

fn rsry_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rsry"))
}

/// Run rsry with an isolated HOME and the given cwd.
fn run_rsry(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(rsry_bin())
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        // Belt-and-braces: some libs consult XDG paths on unix.
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .output()
        .expect("spawn rsry")
}

fn stdout_str(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_str(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// `git init` a tempdir so `rsry init`'s hook installation has a real gitdir.
fn git_init(dir: &Path) {
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir)
        .status()
        .expect("spawn git init");
    assert!(status.success(), "git init failed in {}", dir.display());
}

// ---------------------------------------------------------------------------
// 1. Search outside any repo: no phantom store, loud failure when there is
//    nothing to search.
// ---------------------------------------------------------------------------
#[test]
fn search_outside_repo_creates_no_phantom_store() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();

    let out = run_rsry(home.path(), cwd.path(), &["bead", "search", "nope"]);

    assert!(
        !cwd.path().join(".beads").exists(),
        "phantom .beads store was created in a non-repo cwd"
    );
    // Empty registry + not in a repo: this must be a loud error, not a
    // silent "No beads matching" success.
    assert!(
        !out.status.success(),
        "expected non-zero exit outside a repo with no registered repos; stdout={:?}",
        stdout_str(&out)
    );
    let err = stderr_str(&out);
    assert!(
        err.contains("rsry init") || err.contains("--repo"),
        "error should tell the user how to proceed, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// 2. Create outside any repo: refuse, never write into a phantom store.
// ---------------------------------------------------------------------------
#[test]
fn create_outside_repo_refuses() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();

    let out = run_rsry(
        home.path(),
        cwd.path(),
        &["bead", "create", "should never land", "--force"],
    );

    assert!(
        !out.status.success(),
        "bead create outside a repo must refuse; stdout={:?}",
        stdout_str(&out)
    );
    assert!(
        !cwd.path().join(".beads").exists(),
        "bead create outside a repo fabricated a store"
    );
}

// ---------------------------------------------------------------------------
// 3. Explicit --repo pointing at a directory with no store: error with
//    guidance, do not fabricate.
// ---------------------------------------------------------------------------
#[test]
fn explicit_repo_without_store_errors() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let target = TempDir::new().unwrap();

    let target_str = target.path().to_string_lossy().into_owned();
    let out = run_rsry(
        home.path(),
        cwd.path(),
        &["bead", "--repo", &target_str, "list"],
    );

    assert!(
        !out.status.success(),
        "bead list --repo <no-store-dir> must error; stdout={:?}",
        stdout_str(&out)
    );
    assert!(
        !target.path().join(".beads").exists(),
        "--repo to a store-less dir fabricated a store"
    );
    assert!(
        stderr_str(&out).contains("rsry init"),
        "error should point at `rsry init`, got: {}",
        stderr_str(&out)
    );
}

// ---------------------------------------------------------------------------
// 4. Search outside a repo WITH registered repos: fall back to searching the
//    registry (status is already global; search should find what exists).
// ---------------------------------------------------------------------------
#[test]
fn search_falls_back_to_registered_repos() {
    let home = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let elsewhere = TempDir::new().unwrap();
    git_init(repo.path());

    let repo_str = repo.path().to_string_lossy().into_owned();

    // Onboard: creates the SQLite store + registers in the isolated HOME.
    let init = run_rsry(home.path(), repo.path(), &["init", &repo_str]);
    assert!(
        init.status.success(),
        "rsry init failed: {}",
        stderr_str(&init)
    );

    // Create a bead from inside the repo (bare create defaults a close
    // condition to the PR-merge signal).
    let create = run_rsry(
        home.path(),
        repo.path(),
        &[
            "bead",
            "create",
            "xylophone discovery target",
            "--files",
            "src/lib.rs",
        ],
    );
    assert!(
        create.status.success(),
        "bead create failed: {}",
        stderr_str(&create)
    );

    // Search from an unrelated non-repo cwd — must find it via the registry.
    let out = run_rsry(
        home.path(),
        elsewhere.path(),
        &["bead", "search", "xylophone"],
    );
    assert!(
        out.status.success(),
        "cross-repo fallback search failed: {}",
        stderr_str(&out)
    );
    let stdout = stdout_str(&out);
    assert!(
        stdout.contains("xylophone"),
        "fallback search did not surface the bead; stdout: {stdout}"
    );
    assert!(
        !elsewhere.path().join(".beads").exists(),
        "fallback search fabricated a store in the cwd"
    );
}

// ---------------------------------------------------------------------------
// 5. Doctor reports config/store health, not just version drift.
// ---------------------------------------------------------------------------
#[test]
fn doctor_reports_config_health() {
    let home = TempDir::new().unwrap();

    // Register a repo whose path does not exist.
    let rsry_dir = home.path().join(".rsry");
    std::fs::create_dir_all(&rsry_dir).unwrap();
    std::fs::write(
        rsry_dir.join("config.toml"),
        "[[repo]]\nname = \"ghost\"\npath = \"/nonexistent/ghost-repo\"\n",
    )
    .unwrap();

    let cwd = TempDir::new().unwrap();
    // Unlikely port so we never touch a real local MCP service.
    let out = run_rsry(home.path(), cwd.path(), &["doctor", "--port", "59999"]);

    assert!(out.status.success(), "doctor should not hard-fail");
    let stdout = stdout_str(&out);
    assert!(
        stdout.contains("config health"),
        "doctor should print a config-health section; stdout: {stdout}"
    );
    assert!(
        stdout.contains("ghost") && stdout.contains("missing"),
        "doctor should flag the ghost repo's missing path; stdout: {stdout}"
    );
}
