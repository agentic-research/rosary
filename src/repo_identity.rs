//! Which checkout a path *belongs to* — the identity a linked git worktree
//! shares with its main checkout (rosary-5e6d0f).
//!
//! A linked worktree is a second working tree of ONE repository. Its tracked
//! `.beads/` is a per-branch copy of files, not the repo's bead store: in a
//! Dolt repo the copy is stale tracked files, and in a SQLite repo it has no
//! `beads.db` at all (gitignored), so opening it yields an empty store. Both
//! the store and the repo's name (the bead-id prefix) therefore come from the
//! main checkout. The tracked projection (`.beads/beads.jsonl`) is the one
//! thing that stays per-worktree, and callers derive it from their own root.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The main checkout when `path` is inside a LINKED worktree; `None` for a
/// main checkout, a non-git directory, or when git is unavailable.
///
/// Linked iff `--git-dir` differs from `--git-common-dir`; the main checkout
/// is the common dir's parent. Absolute paths so the answer never depends on
/// the process's current directory.
pub fn main_worktree_root(path: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .args([
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
        ])
        .current_dir(path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let mut lines = text.lines();
    let git_dir = PathBuf::from(lines.next()?.trim());
    let common = PathBuf::from(lines.next()?.trim());
    if git_dir == common {
        return None;
    }
    common.parent().map(Path::to_path_buf)
}

/// The root whose name identifies the repo: the main checkout for a linked
/// worktree, else `path` itself.
pub fn identity_root(path: &Path) -> PathBuf {
    main_worktree_root(path).unwrap_or_else(|| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_in_fixture(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// `proj` with a tracked `.beads/` and a linked worktree `proj-wt-x` —
    /// the shape every dispatched agent's checkout has.
    fn repo_with_worktree(tmp: &Path) -> (PathBuf, PathBuf) {
        let main = tmp.join("proj");
        std::fs::create_dir_all(main.join(".beads")).unwrap();
        std::fs::write(main.join(".beads/beads.jsonl"), "").unwrap();
        git_in_fixture(&main, &["init", "-q"]);
        git_in_fixture(&main, &["add", ".beads/beads.jsonl"]);
        git_in_fixture(
            &main,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "init",
            ],
        );
        let wt = tmp.join("proj-wt-x");
        git_in_fixture(&main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
        assert!(
            wt.join(".beads").exists(),
            "the worktree carries its own tracked .beads/"
        );
        (main.canonicalize().unwrap(), wt.canonicalize().unwrap())
    }

    #[test]
    fn a_linked_worktree_resolves_to_its_main_checkout() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (main, wt) = repo_with_worktree(tmp.path());
        assert_eq!(
            main_worktree_root(&wt).unwrap().canonicalize().unwrap(),
            main
        );
        assert_eq!(
            identity_root(&wt)
                .canonicalize()
                .unwrap()
                .file_name()
                .unwrap(),
            "proj"
        );
    }

    #[test]
    fn a_main_checkout_and_a_plain_dir_are_their_own_identity() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (main, _wt) = repo_with_worktree(tmp.path());
        assert_eq!(main_worktree_root(&main), None);
        let plain = tmp.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(main_worktree_root(&plain), None);
        assert_eq!(identity_root(&plain), plain);
    }

    /// The store a worktree opens is the main checkout's — not the tracked
    /// copy in the worktree, which is stale (Dolt) or empty (SQLite).
    #[test]
    fn a_linked_worktree_opens_the_main_checkouts_store() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (main, wt) = repo_with_worktree(tmp.path());
        assert_eq!(
            crate::resolve_beads_dir(&wt).canonicalize().unwrap(),
            main.join(".beads")
        );
        assert_eq!(
            crate::resolve_beads_dir(&main).canonicalize().unwrap(),
            main.join(".beads")
        );
    }
}
