use anyhow::Result;

use crate::bead::Bead;
use crate::config::RepoConfig;
/// Scan all configured repos for **open/active** beads — the triage view.
/// Terminal beads (done/closed/rejected) are excluded by the store's active
/// filter, so this is the "what is there to work on" set.
pub async fn scan_repos(repos: &[RepoConfig]) -> Result<Vec<Bead>> {
    scan_repos_inner(repos, false).await
}

/// Scan all configured repos for **every** bead, terminal ones included.
/// `rsry status` uses this so its counts reflect the full population — the
/// open-only [`scan_repos`] structurally can't see done/closed beads, which is
/// why `done` was always reported as 0.
pub async fn scan_repos_all(repos: &[RepoConfig]) -> Result<Vec<Bead>> {
    scan_repos_inner(repos, true).await
}

async fn scan_repos_inner(repos: &[RepoConfig], include_terminal: bool) -> Result<Vec<Bead>> {
    let mut all_beads = Vec::new();

    for repo in repos {
        let path = expand_path(&repo.path);
        let beads_dir = path.join(".beads");
        if !beads_dir.exists() {
            continue;
        }

        match read_beads(&beads_dir, &repo.name, include_terminal).await {
            Ok(beads) => all_beads.extend(beads),
            Err(e) => eprintln!("warning: failed to read beads from {}: {e}", repo.name),
        }
    }

    // Sort: ready items first, then by priority (lower = higher priority)
    all_beads.sort_by(|a, b| {
        b.is_ready()
            .cmp(&a.is_ready())
            .then(a.priority.cmp(&b.priority))
    });

    Ok(all_beads)
}

/// Read beads from a single repo via BeadStore (SQLite or Dolt fallback).
/// `include_terminal` selects the full [`list_all_beads`](crate::store::BeadStore::list_all_beads)
/// view over the active-only [`list_beads`](crate::store::BeadStore::list_beads).
async fn read_beads(
    beads_dir: &std::path::Path,
    repo_name: &str,
    include_terminal: bool,
) -> Result<Vec<Bead>> {
    let store = crate::bead_sqlite::connect_bead_store(beads_dir).await?;
    if include_terminal {
        store.list_all_beads(repo_name).await
    } else {
        store.list_beads(repo_name).await
    }
}

/// Expand `~` in paths. Uses shellexpand (already a dependency).
pub fn expand_path(path: &std::path::Path) -> std::path::PathBuf {
    let s = path.to_string_lossy();
    std::path::PathBuf::from(shellexpand::tilde(&s).to_string())
}

/// Expand `~` AND resolve symlinks, so two path aliases of the same physical
/// repo compare equal — e.g. `~/github/art/X` → `~/remotes/art/X` (a real
/// symlink here), or macOS's `/var` → `/private/var`. Falls back to the
/// tilde-expanded path when canonicalize fails (e.g. the path doesn't exist
/// yet). Use this for repo-path *matching* (rosary-617010) — NOT where a
/// symlinked path must be preserved verbatim (see `workspace/lifecycle.rs`).
pub fn canonicalize_repo_path(path: &std::path::Path) -> std::path::PathBuf {
    let expanded = expand_path(path);
    std::fs::canonicalize(&expanded).unwrap_or(expanded)
}

/// Resolve a user-supplied repo path to an absolute repo root.
///
/// Handles: "." → cwd, "~" → home, walks up to find .git/.beads/Cargo.toml.
/// Use this for any path that comes from user input (CLI args, MCP params).
pub fn resolve_repo_path(user_path: &std::path::Path) -> std::path::PathBuf {
    // Step 1: "." or empty → cwd
    let expanded = if user_path == std::path::Path::new(".") || user_path.as_os_str().is_empty() {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    } else {
        expand_path(user_path)
    };
    // Step 2: walk up to repo root
    crate::config::discover_repo_root(&expanded).unwrap_or(expanded)
}

/// Jaccard similarity between two strings, tokenized by whitespace.
///
/// Returns a value between 0.0 (no overlap) and 1.0 (identical token sets).
/// Used for deduplication: if two bead titles have high similarity, one is
/// likely a duplicate of the other.
pub fn jaccard_similarity(a: &str, b: &str) -> f64 {
    use std::collections::HashSet;
    let set_a: HashSet<&str> = a.split_whitespace().collect();
    let set_b: HashSet<&str> = b.split_whitespace().collect();

    if set_a.is_empty() && set_b.is_empty() {
        return 1.0;
    }

    let intersection = set_a.intersection(&set_b).count() as f64;
    let union = set_a.union(&set_b).count() as f64;

    if union == 0.0 {
        return 0.0;
    }

    intersection / union
}

#[allow(dead_code)] // Used by reconciler dedup + future /btw skill
/// Find beads with titles similar to the given title.
///
/// Returns a vec of (bead_id, similarity_score) for beads above the threshold.
/// Used by the reconciler for dedup and by the `/btw` skill for pre-creation checks.
pub fn find_similar_beads(title: &str, existing: &[Bead], threshold: f64) -> Vec<(String, f64)> {
    existing
        .iter()
        .filter_map(|b| {
            let score = jaccard_similarity(title, &b.title);
            if score >= threshold {
                Some((b.id.clone(), score))
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The golden oracle every tilde-expansion test below compares against —
    /// independent of `expand_path`'s own implementation, so a bug in
    /// `expand_path` can't hide behind a self-referential assertion.
    fn home() -> std::path::PathBuf {
        dirs_next::home_dir().expect("test environment must have a resolvable HOME")
    }

    #[test]
    fn expand_tilde_is_byte_exact_to_home() {
        let p = expand_path(std::path::Path::new("~/foo/bar"));
        assert_eq!(p, home().join("foo/bar"));
    }

    #[test]
    fn expand_bare_tilde_is_byte_exact_to_home() {
        let p = expand_path(std::path::Path::new("~"));
        assert_eq!(p, home());
    }

    #[test]
    fn expand_absolute_path_unchanged() {
        let p = expand_path(std::path::Path::new("/tmp/myrepo"));
        assert_eq!(p, std::path::PathBuf::from("/tmp/myrepo"));
    }

    #[test]
    fn expand_relative_non_tilde_path_unchanged() {
        // Only a LEADING `~` is special — a path that merely contains one
        // elsewhere, or has none, passes through untouched.
        let p = expand_path(std::path::Path::new("foo/~bar/baz"));
        assert_eq!(p, std::path::PathBuf::from("foo/~bar/baz"));
    }

    #[test]
    fn resolve_repo_dot_is_byte_exact_to_cwd() {
        let resolved = resolve_repo_path(std::path::Path::new("."));
        let cwd = std::env::current_dir().unwrap();
        // std::env::current_dir() is itself the discovery starting point —
        // resolve_repo_path then walks UP from it looking for a repo marker,
        // so the exact expectation is discover_repo_root's own answer for
        // this process's cwd (this repo, running under `cargo test`, always
        // has a marker at or above cwd — Cargo.toml at minimum).
        let expected = crate::config::discover_repo_root(&cwd).unwrap_or(cwd);
        assert_eq!(resolved, expected);
    }

    #[test]
    fn resolve_repo_empty_matches_dot() {
        // "" and "." must resolve identically — both mean "start from cwd."
        assert_eq!(
            resolve_repo_path(std::path::Path::new("")),
            resolve_repo_path(std::path::Path::new("."))
        );
    }

    #[test]
    fn resolve_repo_path_composes_expand_then_discover_byte_exact() {
        // resolve_repo_path's documented contract is exactly two steps:
        // expand ~, then walk up for a marker. Assert the composition
        // directly against each step's own function, independently of
        // whether this environment's $HOME ancestry happens to contain a
        // marker (it may — e.g. a sandboxed $HOME nested under a repo
        // checkout — so this must not assume "no marker found").
        let tmp = tempfile::TempDir::new_in(home()).unwrap();
        let leaf = tmp.path().join("plain-subdir");
        std::fs::create_dir_all(&leaf).unwrap();
        let tilde_arg = format!("~/{}", leaf.strip_prefix(home()).unwrap().to_string_lossy());

        let resolved = resolve_repo_path(std::path::Path::new(&tilde_arg));
        let expected = crate::config::discover_repo_root(&leaf).unwrap_or(leaf);
        assert_eq!(resolved, expected);
    }

    // --- canonicalize_repo_path (rosary-617010's function, zero coverage
    // before this — the exact function two real bugs this session traced
    // back to: the ~/github vs ~/remotes symlink alias, and RepoPool
    // reconnect matching) --------------------------------------------------

    #[test]
    fn canonicalize_repo_path_resolves_through_a_real_symlink() {
        let tmp = tempfile::TempDir::new().unwrap();
        let real = tmp.path().join("real-repo");
        std::fs::create_dir_all(&real).unwrap();
        let alias = tmp.path().join("alias-repo");
        std::os::unix::fs::symlink(&real, &alias).unwrap();

        let via_alias = canonicalize_repo_path(&alias);
        let via_real = canonicalize_repo_path(&real);
        // Byte-exact: both aliases of the same physical directory must
        // canonicalize to the identical PathBuf, not just "point at
        // something with the same content" — this equality IS the property
        // RepoPool::get_by_path and the ~/github->~/remotes alias case
        // depend on.
        assert_eq!(via_alias, via_real);
        // And it must actually be the real (non-symlink) path, not the
        // alias echoed back unresolved.
        assert_eq!(via_alias, real.canonicalize().unwrap());
        assert_ne!(via_alias, alias);
    }

    #[test]
    fn canonicalize_repo_path_falls_back_to_expanded_when_nonexistent() {
        // std::fs::canonicalize fails on a path that doesn't exist —
        // canonicalize_repo_path's documented fallback is the tilde-
        // expanded (but otherwise untouched) path, byte-exact.
        let nonexistent =
            std::path::Path::new("/tmp/definitely-does-not-exist-rosary-scanner-test");
        assert!(!nonexistent.exists());
        assert_eq!(
            canonicalize_repo_path(nonexistent),
            expand_path(nonexistent)
        );
    }

    #[test]
    fn canonicalize_repo_path_expands_tilde_before_canonicalizing() {
        // A tilde-prefixed path to a REAL directory must both expand AND
        // canonicalize — exercising both steps in one call, byte-exact
        // against a HOME-relative expectation computed independently.
        let tmp = tempfile::TempDir::new_in(home()).unwrap();
        let leaf = tmp.path().join("canon-check");
        std::fs::create_dir_all(&leaf).unwrap();
        let tilde_arg = format!("~/{}", leaf.strip_prefix(home()).unwrap().to_string_lossy());

        let resolved = canonicalize_repo_path(std::path::Path::new(&tilde_arg));
        assert_eq!(resolved, leaf.canonicalize().unwrap());
    }

    #[test]
    fn resolve_repo_discovers_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().join("myrepo");
        let subdir = root.join("src").join("deep");
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();

        let resolved = resolve_repo_path(&subdir);
        assert_eq!(resolved, root);
    }

    #[test]
    fn resolve_repo_file_name_not_unnamed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().join("venturi");
        std::fs::create_dir_all(root.join(".git")).unwrap();

        let resolved = resolve_repo_path(&root);
        let name = resolved.file_name().unwrap().to_string_lossy();
        assert_eq!(name, "venturi", "should extract repo name, not 'unnamed'");
    }

    #[tokio::test]
    async fn scan_repos_skips_missing_beads_dir() {
        let repos = vec![RepoConfig {
            name: "nonexistent".to_string(),
            path: std::path::PathBuf::from("/tmp/no-such-repo"),
            lang: None,
            self_managed: false,
            approval: crate::config::DispatchApproval::Approved,
        }];
        let beads = scan_repos(&repos).await.unwrap();
        assert!(beads.is_empty());
    }

    /// Integration test — scans a real repo with a running Dolt server.
    /// Set RSRY_TEST_BEADS_DIR to a .beads/ directory (e.g. ~/remotes/art/mache/.beads).
    #[tokio::test]
    async fn scan_live_repo() {
        let beads_dir = match std::env::var("RSRY_TEST_BEADS_DIR") {
            Ok(dir) => dir,
            Err(_) => {
                eprintln!("skipping: RSRY_TEST_BEADS_DIR not set");
                return;
            }
        };

        // The parent of .beads/ is the repo root
        let repo_path = std::path::Path::new(&beads_dir)
            .parent()
            .expect("beads dir should have a parent");

        let repos = vec![RepoConfig {
            name: "test-repo".to_string(),
            path: repo_path.to_path_buf(),
            lang: None,
            self_managed: false,
            approval: crate::config::DispatchApproval::Approved,
        }];

        let beads = scan_repos(&repos).await.unwrap();
        assert!(!beads.is_empty(), "expected beads from live Dolt server");

        // Verify sorting: ready items first
        let first_ready = beads.iter().position(|b| !b.is_ready());
        let last_ready = beads.iter().rposition(|b| b.is_ready());
        if let (Some(first_non), Some(last_r)) = (first_ready, last_ready) {
            assert!(
                last_r < first_non,
                "ready beads should sort before non-ready"
            );
        }

        // All beads should have the repo name we passed
        for b in &beads {
            assert_eq!(b.repo, "test-repo");
            assert!(!b.id.is_empty());
            assert!(!b.title.is_empty());
        }
    }

    #[test]
    fn jaccard_identical_strings() {
        assert!((jaccard_similarity("fix the bug", "fix the bug") - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn jaccard_disjoint_strings() {
        assert!((jaccard_similarity("fix the bug", "add new feature")).abs() < f64::EPSILON);
    }

    #[test]
    fn jaccard_partial_overlap() {
        let sim = jaccard_similarity("fix the widget bug", "fix the gadget bug");
        // intersection: {fix, the, bug} = 3, union: {fix, the, widget, bug, gadget} = 5
        assert!((sim - 0.6).abs() < f64::EPSILON);
    }

    #[test]
    fn jaccard_empty_strings() {
        assert!((jaccard_similarity("", "") - 1.0).abs() < f64::EPSILON);
    }

    fn make_bead(id: &str, title: &str) -> Bead {
        Bead {
            id: id.to_string(),
            title: title.to_string(),
            description: String::new(),
            status: "open".to_string(),
            priority: 2,
            issue_type: "task".to_string(),
            owner: None,
            repo: "test".to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            dependency_count: 0,
            dependent_count: 0,
            comment_count: 0,
            branch: None,
            pr_url: None,
            jj_change_id: None,
            external_ref: None,
            files: Vec::new(),
            test_files: Vec::new(),
            created_by: None,
            scope: String::new(),
            derived_from: vec![],
            acceptance_criteria: String::new(),
        }
    }

    #[test]
    fn find_similar_exact_match() {
        let existing = vec![make_bead("a", "fix the widget bug")];
        let results = find_similar_beads("fix the widget bug", &existing, 0.6);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "a");
        assert!((results[0].1 - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn find_similar_no_match() {
        let existing = vec![make_bead("a", "add new feature")];
        let results = find_similar_beads("fix the widget bug", &existing, 0.6);
        assert!(results.is_empty());
    }

    #[test]
    fn find_similar_partial_match() {
        let existing = vec![
            make_bead("a", "fix the widget bug"),
            make_bead("b", "fix the gadget bug"),
            make_bead("c", "completely unrelated task"),
        ];
        let results = find_similar_beads("fix the widget bug", &existing, 0.6);
        assert_eq!(results.len(), 2); // a (exact) + b (0.6)
    }

    #[test]
    fn find_similar_empty_existing() {
        let results = find_similar_beads("anything", &[], 0.6);
        assert!(results.is_empty());
    }
}
