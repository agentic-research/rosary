use anyhow::{Context, Result};
use std::process::Command;

use crate::bead::Bead;
use crate::config::RepoConfig;

/// Scan all configured repos for beads via `bd list --json`
pub fn scan_repos(repos: &[RepoConfig]) -> Result<Vec<Bead>> {
    let mut all_beads = Vec::new();

    for repo in repos {
        let path = expand_path(&repo.path);
        if !path.join(".beads").exists() {
            continue;
        }

        match read_beads(&path, &repo.name) {
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

/// Read beads from a single repo by invoking `bd list --json`
fn read_beads(repo_path: &std::path::Path, repo_name: &str) -> Result<Vec<Bead>> {
    let output = Command::new("bd")
        .arg("list")
        .arg("--json")
        .current_dir(repo_path)
        .output()
        .with_context(|| format!("running bd list in {}", repo_path.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("bd list failed in {repo_name}: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let values: Vec<serde_json::Value> =
        serde_json::from_str(&stdout).with_context(|| "parsing bd list JSON output")?;

    Ok(values
        .iter()
        .filter_map(|v| Bead::from_bd_json(v, repo_name))
        .collect())
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

fn expand_path(path: &std::path::Path) -> std::path::PathBuf {
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
}
