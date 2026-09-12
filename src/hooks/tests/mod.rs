//! Shared fixtures for the hooks tests, one file per hook so each hook's
//! contract can change without collisions (thread trusted-kernel/projection-timing).

use super::*;
use std::process::Command;

mod gitignore;
mod install;
mod merge;
mod post_merge;
mod pre_commit;
mod pre_push;
/// Run `git` with a genuinely-isolated env so the host's gitconfig
/// can't leak into the test (commit.gpgsign, core.hooksPath, user
/// identity, etc.). We override:
///
/// - `HOME` → empty tempdir so `$HOME/.gitconfig` is a fresh file
/// - `GIT_CONFIG_GLOBAL` → /dev/null on Unix so the global file is
///   forced empty regardless of HOME
/// - `GIT_CONFIG_NOSYSTEM=1` → skip `/etc/gitconfig`
///
/// Each call gets its own scratch HOME so tests don't share state.
pub(in crate::hooks) fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    // Use the existing dir for HOME; git only writes to ~/.gitconfig
    // when called with `config --global`, which we never do. Pointing
    // HOME at a tempdir under our control is sufficient isolation.
    let home = tempfile::tempdir().expect("HOME tempdir");
    Command::new("git")
        .current_dir(dir)
        .env_clear()
        .env("HOME", home.path())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        // Preserve PATH so git itself and its subcommands can be found.
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .args(args)
        .output()
        .expect("spawn git")
}

pub(in crate::hooks) fn init_repo(dir: &Path) {
    assert!(git(dir, &["init", "-q", "-b", "main"]).status.success());
    // user.email / user.name go into THIS repo's local config, not
    // global — so they don't need the env scaffolding above to take
    // effect, but they also don't hurt.
    assert!(
        git(dir, &["config", "user.email", "test@example.invalid"])
            .status
            .success()
    );
    assert!(git(dir, &["config", "user.name", "test"]).status.success());
    assert!(
        git(dir, &["config", "commit.gpgsign", "false"])
            .status
            .success()
    );
}

pub(in crate::hooks) fn seed_commit(dir: &Path) {
    std::fs::write(dir.join("seed"), "x").unwrap();
    assert!(git(dir, &["add", "seed"]).status.success());
    assert!(git(dir, &["commit", "-q", "-m", "seed"]).status.success());
}

// --- .gitattributes routing ---------------------------------------

// --- gitignore-shadow auto-fix (rosary-e97360) ---------------------
//
// Pure classification/removal logic (classify_gitignore_shadow_shape,
// remove_gitignore_line, allowlist_fix_suggestion) lives in
// src/gitignore.rs with its own unit tests — kept as a standalone
// file specifically so it can be mutation-tested in isolation
// (`task mutants:gitignore`) without main.rs's unrelated noise.
// What stays here is the I/O-level integration: does
// `fix_gitignore_shadow` actually read/write/re-verify correctly.

// --- pre-commit-framework integration (rosary-00f2b5) --------------
//
// Pure detection/YAML-editing logic (is_precommit_framework_owned,
// merge_precommit_yaml) lives in src/precommit_yaml.rs with its own
// unit tests, for the same mutation-testing reason as gitignore.rs.
// What stays here is I/O-level: does `install()` actually redirect
// correctly, and does `hooks run` actually execute.

// --- hooks run (rosary-00f2b5) --------------------------------------

#[test]
fn hooks_run_unknown_hook_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_repo(root);
    let err = run(root, "not-a-real-hook").unwrap_err();
    assert!(format!("{err:#}").contains("unknown hook"), "{err:#}");
}

/// `run`, but with an explicit `RSRY_BIN` — the hook's own
/// first-choice resolution, so a stub can stand in for a real store
/// without touching PATH. Returns the raw output because these tests
/// assert on the refusal itself (exit status + the operator-facing
/// message), which `run` collapses into an opaque `Err`.
pub(in crate::hooks) fn run_hook_with_rsry(
    repo_root: &Path,
    name: &str,
    rsry_bin: &Path,
) -> std::process::Output {
    let (_, block) = HOOKS
        .iter()
        .find(|(n, _)| *n == name)
        .expect("hook must be registered");
    Command::new("sh")
        .arg("-c")
        .arg(render_block(block))
        .current_dir(repo_root)
        .env("RSRY_BIN", rsry_bin)
        .output()
        .expect("spawn hook")
}

/// Exit-code propagation, proven without depending on the `rsry`
/// binary: `commit-msg`'s embedded script reads `$1` for the commit
/// message file. `hooks run` passes no positional argument, so `$1`
/// is empty — the script deterministically falls through to its own
/// `exit 1` (no commit-message pattern can match an empty subject).
#[test]
fn hooks_run_propagates_a_nonzero_exit_as_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_repo(root);
    let err = run(root, "commit-msg").unwrap_err();
    assert!(format!("{err:#}").contains("exited"), "{err:#}");
}

// --- embedded-template invariants ---------------------------------

#[test]
fn templates_embedded_and_nonempty() {
    // include_str! is a compile-time op; reading them here proves the
    // build had access to docs/git-hooks/* AND the content survived
    // into the binary. Anything that depends on a real filesystem
    // lookup at runtime (the old find_template_dir path) would fail
    // here when run from a different cwd.
    for (name, content) in HOOKS {
        assert!(!content.trim().is_empty(), "template {name} is empty");
        // The bead-sync hooks drive dolt; the commit-msg hook is the
        // commit-contract gate and has nothing to do with dolt.
        if matches!(*name, "post-merge" | "post-push") {
            assert!(
                content.contains("dolt"),
                "sync template {name} should reference dolt commands"
            );
        }
    }
}

// --- rsry-binary baking (rosary-cb9321) ---------------------------

// --- resolve_hooks_dir --------------------------------------------

// --- merge_hook (pure-function behavior) --------------------------

// --- install (full filesystem + git interaction) ------------------

// --- classify_dolt_remote (pure-function decision logic) ----------

// --- audit: classify_gitignore_check (examples) --------------------

// --- audit: pure predicates (property tests) ------------------------
//
// These prove the LAWS, not just chosen examples — property tests
// over `backend_ambiguous` and `store_export_drifted` per the
// session's mutants-rung discipline (rosary-b2ae79): a law stated
// once and checked over the whole input space catches boundary bugs
// an example can't. `proptest_support` isn't used here (no shrink
// config needed for these small/fast domains); the plain
// `proptest!` macro's defaults are enough.

// --- audit: end-to-end fixtures --------------------------------------

// --- documentation / marker consistency ---------------------------

/// A stub `rsry` whose `bead export -o <path>` writes `body`.
///
/// `body` travels in a sidecar file rather than being interpolated
/// into the script, so no amount of quoting in a bead record can
/// change what the stub does.
pub(in crate::hooks) fn fake_rsry(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("fake-rsry");
    std::fs::write(dir.join("fake-rsry.out"), body).unwrap();
    std::fs::write(
        &path,
        "#!/bin/sh\n\
         while [ $# -gt 0 ]; do\n\
         \x20 if [ \"$1\" = \"-o\" ]; then shift; cat \"$0.out\" > \"$1\"; fi\n\
         \x20 shift\n\
         done\n\
         exit 0\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}
