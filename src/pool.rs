//! Long-lived connection pool for Dolt servers across registered repos.
//!
//! The MCP server creates a `RepoPool` on startup, connecting to all
//! repos with .beads/ directories. Connections are reused across tool
//! calls — no per-request connect overhead.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::config;
use crate::dolt::{DoltClient, DoltConfig};

/// Long-lived pool of DoltClient connections keyed by repo name.
pub struct RepoPool {
    clients: HashMap<String, DoltClient>,
    paths: HashMap<String, PathBuf>,
}

impl RepoPool {
    /// Create an empty pool (for testing and HTTP server startup with no repos).
    #[allow(dead_code)] // used in tests
    pub fn empty() -> Self {
        RepoPool {
            clients: HashMap::new(),
            paths: HashMap::new(),
        }
    }

    /// Create a pool and connect to all repos in the given config.
    /// Repos that fail to connect are logged and skipped (best-effort).
    pub async fn from_config(config_path: &str) -> Result<Self> {
        let cfg = config::load_merged(config_path)?;
        let mut clients = HashMap::new();
        let mut paths = HashMap::new();

        for repo in &cfg.repo {
            let path = crate::scanner::expand_path(&repo.path);
            let beads_dir = path.join(".beads");
            if !beads_dir.exists() {
                continue;
            }

            paths.insert(repo.name.clone(), path.clone());

            match DoltConfig::from_beads_dir(&beads_dir) {
                Ok(dolt_config) => match DoltClient::connect(&dolt_config).await {
                    Ok(client) => {
                        eprintln!("[pool] connected: {}", repo.name);
                        clients.insert(repo.name.clone(), client);
                    }
                    Err(e) => {
                        eprintln!("[pool] skipping {} (connect failed: {e})", repo.name);
                    }
                },
                Err(e) => {
                    eprintln!("[pool] skipping {} (config error: {e})", repo.name);
                }
            }
        }

        Ok(RepoPool { clients, paths })
    }

    /// Get a DoltClient by repo name.
    pub fn get(&self, repo_name: &str) -> Option<&DoltClient> {
        self.clients.get(repo_name)
    }

    /// Get a DoltClient by repo path (resolves name from path).
    /// Resolves repo path via discover_repo_root (no symlink resolution).
    pub fn get_by_path(&self, repo_path: &str) -> Option<(&str, &DoltClient)> {
        let target = Path::new(repo_path);
        let discovered = config::discover_repo_root(target).unwrap_or_else(|| target.to_path_buf());
        let root = crate::scanner::expand_path(&discovered);

        for (name, path) in &self.paths {
            if *path == root
                && let Some(client) = self.clients.get(name)
            {
                return Some((name.as_str(), client));
            }
        }
        None
    }

    /// Number of connected repos.
    pub fn len(&self) -> usize {
        self.clients.len()
    }

    /// Whether pool has no connections (required by clippy alongside `len()`).
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }

    /// List connected repo names.
    pub fn repo_names(&self) -> Vec<&str> {
        self.clients.keys().map(|s| s.as_str()).collect()
    }

    /// Iterate over all (repo_name, client) pairs. Used by webhook handler.
    #[allow(dead_code)]
    pub fn iter_clients(&self) -> impl Iterator<Item = (&str, &DoltClient)> {
        self.clients.iter().map(|(k, v)| (k.as_str(), v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_pool() {
        let pool = RepoPool {
            clients: HashMap::new(),
            paths: HashMap::new(),
        };
        assert!(pool.is_empty());
        assert_eq!(pool.len(), 0);
        assert!(pool.get("nonexistent").is_none());
        assert!(pool.get_by_path("/tmp/fake").is_none());
        assert!(pool.repo_names().is_empty());
    }

    #[test]
    fn pool_paths_resolve() {
        let mut paths = HashMap::new();
        paths.insert("myrepo".to_string(), PathBuf::from("/tmp/myrepo"));

        let pool = RepoPool {
            clients: HashMap::new(),
            paths,
        };

        // No client for this path, but path resolution works
        assert!(pool.get_by_path("/tmp/myrepo").is_none()); // no client
        assert!(pool.get("myrepo").is_none()); // no client
    }

    #[tokio::test]
    async fn from_config_handles_missing_config() {
        let result = RepoPool::from_config("/nonexistent/rosary.toml").await;
        // Should succeed with empty pool (load_merged falls back to global)
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn from_config_skips_repos_without_beads() {
        // Create a temp config with a repo that has no .beads/
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("test.toml");
        std::fs::write(
            &config_path,
            r#"
[[repo]]
name = "fake"
path = "/tmp/no-such-repo-xyz"
"#,
        )
        .unwrap();

        let pool = RepoPool::from_config(config_path.to_str().unwrap())
            .await
            .unwrap();
        // The fake repo should not be connected (no .beads/ dir).
        // Note: load_merged may connect real repos from ~/.rsry/config.toml.
        assert!(pool.get("fake").is_none());
    }

    #[test]
    fn repo_names_returns_connected() {
        let clients = HashMap::new();
        let mut paths = HashMap::new();

        // We can't easily create a DoltClient without a real server,
        // so test the names/paths logic separately
        paths.insert("alpha".to_string(), PathBuf::from("/tmp/alpha"));
        paths.insert("beta".to_string(), PathBuf::from("/tmp/beta"));

        let pool = RepoPool { clients, paths };
        // No clients connected, so repo_names is empty
        assert!(pool.repo_names().is_empty());
    }
}
