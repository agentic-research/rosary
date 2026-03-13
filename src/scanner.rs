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
            println!(
                "  {} [P{}] {} — {}",
                b.repo, b.priority, b.id, b.title
            );
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
    if s.starts_with('~') {
        if let Some(home) = dirs_next::home_dir() {
            return home.join(&s[2..]);
        }
    }
    path.to_path_buf()
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
        }];
        let beads = scan_repos(&repos).await.unwrap();
        assert!(beads.is_empty());
    }

    /// Integration test — scans a real repo with a running Dolt server.
    /// Set LOOM_TEST_BEADS_DIR to a .beads/ directory (e.g. ~/remotes/art/mache/.beads).
    #[tokio::test]
    async fn scan_live_repo() {
        let beads_dir = match std::env::var("LOOM_TEST_BEADS_DIR") {
            Ok(dir) => dir,
            Err(_) => {
                eprintln!("skipping: LOOM_TEST_BEADS_DIR not set");
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
}
