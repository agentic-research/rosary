use anyhow::Result;

use crate::bead::Bead;
use crate::config::RepoConfig;
use crate::dolt::{DoltClient, DoltConfig};

/// Scan all configured repos for beads via native MySQL to Dolt.
pub async fn scan_repos(repos: &[RepoConfig]) -> Result<Vec<Bead>> {
    let mut all_beads = Vec::new();

    for repo in repos {
        let path = expand_path(&repo.path);
        let beads_dir = path.join(".beads");
        if !beads_dir.exists() {
            continue;
        }

        match read_beads(&beads_dir, &repo.name).await {
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

/// Read beads from a single repo via native MySQL connection to Dolt.
async fn read_beads(beads_dir: &std::path::Path, repo_name: &str) -> Result<Vec<Bead>> {
    let config = DoltConfig::from_beads_dir(beads_dir)?;
    let client = DoltClient::connect(&config).await?;
    client.list_beads(repo_name).await
}

pub fn print_status(beads: &[Bead]) {
    let open = beads.iter().filter(|b| b.status == "open").count();
    let in_progress = beads.iter().filter(|b| b.status == "in_progress").count();
    let blocked = beads
        .iter()
        .filter(|b| b.dependency_count > 0 && b.status == "open")
        .count();
    let ready = beads.iter().filter(|b| b.is_ready()).count();

    println!("Across {} repos:", count_repos(beads));
    println!("  Open:        {open}");
    println!("  Ready:       {ready}");
    println!("  In Progress: {in_progress}");
    println!("  Blocked:     {blocked}");
    println!();

    if ready > 0 {
        println!("Ready to work:");
        for b in beads.iter().filter(|b| b.is_ready()).take(10) {
            println!("  {} [P{}] {} — {}", b.repo, b.priority, b.id, b.title);
        }
    }
}

fn count_repos(beads: &[Bead]) -> usize {
    let mut repos: Vec<&str> = beads.iter().map(|b| b.repo.as_str()).collect();
    repos.sort();
    repos.dedup();
    repos.len()
}

pub fn expand_path(path: &std::path::Path) -> std::path::PathBuf {
    let s = path.to_string_lossy();
    if s.starts_with('~')
        && let Some(home) = dirs_next::home_dir()
    {
        return home.join(&s[2..]);
    }
    path.to_path_buf()
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

    #[test]
    fn expand_tilde() {
        let p = expand_path(std::path::Path::new("~/foo/bar"));
        assert!(!p.to_string_lossy().starts_with('~'));
        assert!(p.to_string_lossy().ends_with("foo/bar"));
    }

    #[tokio::test]
    async fn scan_repos_skips_missing_beads_dir() {
        let repos = vec![RepoConfig {
            name: "nonexistent".to_string(),
            path: std::path::PathBuf::from("/tmp/no-such-repo"),
            lang: None,
            self_managed: false,
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
