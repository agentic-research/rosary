//! Long-lived connection pool for bead stores across registered repos.
//!
//! The MCP server creates a `RepoPool` on startup, connecting to all
//! repos with .beads/ directories. Connections are reused across tool
//! calls — no per-request connect overhead.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::config;
use crate::store::BeadStore;

/// Read the current Dolt server port from a `.beads/` directory.
/// Returns `None` if the port file is absent or unparseable.
fn read_dolt_port(beads_dir: &Path) -> Option<u16> {
    if beads_dir.join("dolt").exists()
        && let Ok(config) = crate::dolt::DoltConfig::from_beads_dir(beads_dir)
        && config.port != 0
    {
        return Some(config.port);
    }

    let port_file = beads_dir.join("dolt-server.port");
    let s = std::fs::read_to_string(port_file).ok()?;
    s.trim().parse().ok()
}

/// Return `true` if the Dolt port for this repo has changed since the pool was built.
/// Stale connections must not be returned from `get()` — the MySQL pool is pinned
/// to the old port and all queries will fail after a dolt-server restart.
fn is_dolt_port_stale(
    known_ports: &HashMap<String, u16>,
    beads_dirs: &HashMap<String, PathBuf>,
    repo_name: &str,
) -> bool {
    let Some(known) = known_ports.get(repo_name) else {
        return false;
    };
    let Some(beads_dir) = beads_dirs.get(repo_name) else {
        return false;
    };
    // Only check for Dolt repos (SQLite repos don't have port files).
    if !beads_dir.join("dolt").exists() {
        return false;
    }
    match read_dolt_port(beads_dir) {
        Some(current) => current != *known,
        None => true,
    }
}

/// Long-lived pool of BeadStore connections keyed by repo name.
pub struct RepoPool {
    clients: HashMap<String, Box<dyn BeadStore>>,
    paths: HashMap<String, PathBuf>,
    /// `.beads/` directories used when connecting (for reconnect + port check).
    beads_dirs: HashMap<String, PathBuf>,
    /// Dolt server port observed at connect time. Used to detect restarts.
    known_ports: HashMap<String, u16>,
}

impl RepoPool {
    /// Create an empty pool (for testing and HTTP server startup with no repos).
    #[allow(dead_code)] // used in tests
    pub fn empty() -> Self {
        RepoPool {
            clients: HashMap::new(),
            paths: HashMap::new(),
            beads_dirs: HashMap::new(),
            known_ports: HashMap::new(),
        }
    }

    /// Build a pool holding a single already-connected store, for tests.
    /// `known_ports`/`beads_dirs` stay empty so the staleness guard is a no-op.
    #[cfg(test)]
    pub fn from_client(name: &str, path: PathBuf, store: Box<dyn BeadStore>) -> Self {
        let mut clients: HashMap<String, Box<dyn BeadStore>> = HashMap::new();
        clients.insert(name.to_string(), store);
        let mut paths = HashMap::new();
        paths.insert(name.to_string(), path);
        RepoPool {
            clients,
            paths,
            beads_dirs: HashMap::new(),
            known_ports: HashMap::new(),
        }
    }

    /// Build a pool that RECORDS the config but connects NOTHING (rosary-31193d).
    ///
    /// Eagerly connecting every configured repo here was the dolt-server leak: it
    /// spawned/attached a `dolt sql-server` per Dolt repo on *every* MCP server
    /// start — 34 repos → 34 servers → ~6.5GB — even for a single-repo op
    /// (rosary-a7a668). Now stores are opened lazily, per repo actually used:
    /// [`get`]/[`get_by_path`] connect on demand via the recorded `beads_dirs`,
    /// and the ad-hoc fallback in `get_client` handles the rest. So startup
    /// spawns zero servers; a single-repo op touches exactly one.
    pub async fn from_config(config_path: &str) -> Result<Self> {
        let cfg = config::load_merged(config_path)?;
        let mut paths = HashMap::new();
        let mut beads_dirs = HashMap::new();
        for repo in &cfg.repo {
            let path = crate::scanner::expand_path(&repo.path);
            let beads_dir = path.join(".beads");
            if !beads_dir.exists() {
                continue;
            }
            paths.insert(repo.name.clone(), path);
            beads_dirs.insert(repo.name.clone(), beads_dir);
        }
        Ok(RepoPool {
            clients: HashMap::new(),
            paths,
            beads_dirs,
            known_ports: HashMap::new(),
        })
    }

    /// The recorded repo root path for a registered repo name, without
    /// connecting. Lets a scope-only call (`scope: "repo:X"`, no `repo_path`)
    /// resolve X's path and lazily open just that store — rosary-31193d.
    pub fn path_for(&self, repo_name: &str) -> Option<&Path> {
        self.paths.get(repo_name).map(|p| p.as_path())
    }

    /// Every configured repo name (whether or not it's connected). Used for
    /// the startup log + cross-repo handlers, since `clients` is now lazy.
    pub fn configured_names(&self) -> Vec<&str> {
        self.beads_dirs.keys().map(|s| s.as_str()).collect()
    }

    /// Open a store for EVERY configured repo (ad hoc, best-effort). For the
    /// cross-repo handlers (webhooks, ticket-load) that must search all repos
    /// for a bead without knowing which one holds it. Connects on demand — a
    /// webhook firing is far rarer than an MCP startup, so this doesn't
    /// reintroduce the startup fan-out.
    pub async fn connect_all(&self) -> Vec<(String, Box<dyn BeadStore>)> {
        let mut out = Vec::new();
        for (name, beads_dir) in &self.beads_dirs {
            match crate::bead_sqlite::connect_bead_store(beads_dir).await {
                Ok(store) => out.push((name.clone(), store)),
                Err(e) => eprintln!("[pool] connect_all: skipping {name} ({e})"),
            }
        }
        out
    }

    /// Get a BeadStore by repo name.
    /// Returns `None` if the repo is unknown or its Dolt port changed since startup
    /// (stale connection — caller will fall through to a fresh connect).
    pub fn get(&self, repo_name: &str) -> Option<&dyn BeadStore> {
        if is_dolt_port_stale(&self.known_ports, &self.beads_dirs, repo_name) {
            eprintln!("[pool] {repo_name}: Dolt port changed, bypassing stale pool entry");
            return None;
        }
        self.clients.get(repo_name).map(|b| b.as_ref())
    }

    /// Get a BeadStore by repo path (resolves name from path).
    /// Canonicalizes symlink aliases on both sides, so the same physical repo
    /// addressed via a different alias (e.g. `~/github/art/X` vs
    /// `~/remotes/art/X`, or macOS `/var` vs `/private/var`) matches its
    /// registered entry instead of silently missing (rosary-617010).
    /// Returns `None` if the repo's Dolt port changed since startup.
    pub fn get_by_path(&self, repo_path: &str) -> Option<(&str, &dyn BeadStore)> {
        let target = Path::new(repo_path);
        let discovered = config::discover_repo_root(target).unwrap_or_else(|| target.to_path_buf());
        let root = crate::scanner::canonicalize_repo_path(&discovered);

        for (name, path) in &self.paths {
            if crate::scanner::canonicalize_repo_path(path) == root {
                if is_dolt_port_stale(&self.known_ports, &self.beads_dirs, name) {
                    eprintln!("[pool] {name}: Dolt port changed, bypassing stale pool entry");
                    return None;
                }
                if let Some(client) = self.clients.get(name) {
                    return Some((name.as_str(), client.as_ref()));
                }
            }
        }
        None
    }

    /// Number of repos actually connected so far (lazy — grows on use, not at
    /// startup). Test-only: pins that `from_config` connects nothing.
    #[cfg(test)]
    pub fn connected_count(&self) -> usize {
        self.clients.len()
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
            beads_dirs: HashMap::new(),
            known_ports: HashMap::new(),
        };
        assert_eq!(pool.connected_count(), 0);
        assert!(pool.get("nonexistent").is_none());
        assert!(pool.get_by_path("/tmp/fake").is_none());
        assert!(pool.path_for("nonexistent").is_none());
    }

    /// rosary-617010: a repo registered under its real path must be found when
    /// looked up via a SYMLINK ALIAS of the same physical dir (the ~/github →
    /// ~/remotes case). Before the fix, get_by_path compared paths with exact
    /// equality and silently missed the alias → "search returns empty".
    #[test]
    fn get_by_path_matches_symlink_alias() {
        let tmp = tempfile::TempDir::new().unwrap();
        let real = tmp.path().join("real-repo");
        std::fs::create_dir_all(real.join(".beads")).unwrap();
        let store =
            crate::bead_sqlite::SqliteBeadStore::connect(&real.join(".beads/beads.db")).unwrap();

        // An alias dir symlinking to the real repo (mirrors ~/github/art/X → ~/remotes/art/X).
        let alias = tmp.path().join("alias-repo");
        std::os::unix::fs::symlink(&real, &alias).unwrap();

        let pool = RepoPool::from_client("myrepo", real.clone(), Box::new(store));

        // Found via the real path (sanity)…
        assert!(
            pool.get_by_path(real.to_str().unwrap()).is_some(),
            "real path should match its registered store"
        );
        // …AND via the symlink alias — the bug was this used to return None.
        assert!(
            pool.get_by_path(alias.to_str().unwrap()).is_some(),
            "symlink alias must resolve to the same registered store"
        );
        // A genuinely unrelated path still misses.
        assert!(
            pool.get_by_path("/tmp/definitely-not-a-repo-617010")
                .is_none()
        );
    }

    #[test]
    fn path_for_resolves_without_connecting() {
        // A registered repo's path is available for lazy connect (scope-only
        // calls) without any store being connected.
        let mut paths = HashMap::new();
        paths.insert("myrepo".to_string(), PathBuf::from("/tmp/myrepo"));
        let pool = RepoPool {
            clients: HashMap::new(),
            paths,
            beads_dirs: HashMap::new(),
            known_ports: HashMap::new(),
        };
        assert_eq!(pool.path_for("myrepo"), Some(Path::new("/tmp/myrepo")));
        assert!(pool.connected_count() == 0, "path lookup connects nothing");
    }

    /// The leak fix (rosary-31193d / a7a668): `from_config` over a config with a
    /// real (SQLite) repo must connect NOTHING — no store, no dolt server — even
    /// though the repo exists. Servers are spawned lazily, per repo actually
    /// used, not eagerly for all on every MCP startup.
    #[tokio::test]
    async fn from_config_connects_nothing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path().join("myrepo");
        std::fs::create_dir_all(repo.join(".beads")).unwrap();
        // A SQLite bead store so the repo is "real" (has .beads/).
        crate::bead_sqlite::SqliteBeadStore::connect(&repo.join(".beads/beads.db")).unwrap();

        let config_path = tmp.path().join("rosary.toml");
        std::fs::write(
            &config_path,
            format!(
                "[[repo]]\nname = \"myrepo\"\npath = \"{}\"\n",
                repo.display()
            ),
        )
        .unwrap();

        let pool = RepoPool::from_config(config_path.to_str().unwrap())
            .await
            .unwrap();

        // Recorded but NOT connected — the whole point.
        assert!(
            pool.configured_names().contains(&"myrepo"),
            "repo is recorded in config"
        );
        assert_eq!(
            pool.connected_count(),
            0,
            "from_config must connect nothing (no eager fan-out)"
        );
        assert_eq!(
            pool.path_for("myrepo"),
            Some(repo.as_path()),
            "path is resolvable for lazy connect"
        );
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
    fn stale_dolt_port_returns_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let beads_dir = tmp.path().to_path_buf();

        // Create a fake dolt/ dir so is_dolt_port_stale treats this as a Dolt repo.
        std::fs::create_dir(beads_dir.join("dolt")).unwrap();

        // Write port 12345 as the "current" port.
        std::fs::write(beads_dir.join("dolt-server.port"), "12345").unwrap();

        let repo_name = "stalerepo";
        let mut known_ports = HashMap::new();
        // Pool was built with port 9999 — server restarted on 12345.
        known_ports.insert(repo_name.to_string(), 9999u16);

        let mut beads_dirs = HashMap::new();
        beads_dirs.insert(repo_name.to_string(), beads_dir.clone());

        assert!(
            is_dolt_port_stale(&known_ports, &beads_dirs, repo_name),
            "port changed from 9999 to 12345 → should be stale"
        );

        assert!(
            !beads_dir.join("dolt-server.port").exists(),
            "closed port should be cleaned while checking staleness"
        );
    }

    #[test]
    fn dolt_port_prefers_live_sql_server_info_over_legacy_port_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let beads_dir = tmp.path().to_path_buf();
        let dolt_meta = beads_dir.join("dolt").join("beads").join(".dolt");
        std::fs::create_dir_all(&dolt_meta).unwrap();
        let Ok(listener) = std::net::TcpListener::bind("127.0.0.1:0") else {
            eprintln!("[skip] cannot bind loopback listener in this sandbox");
            return;
        };
        let port = listener.local_addr().unwrap().port();

        std::fs::write(beads_dir.join("dolt-server.port"), "12345").unwrap();
        std::fs::write(
            dolt_meta.join("sql-server.info"),
            format!("{}:{port}:test-server", std::process::id()),
        )
        .unwrap();

        assert_eq!(read_dolt_port(&beads_dir), Some(port));

        let repo_name = "rosary";
        let mut known_ports = HashMap::new();
        known_ports.insert(repo_name.to_string(), port);

        let mut beads_dirs = HashMap::new();
        beads_dirs.insert(repo_name.to_string(), beads_dir);

        assert!(
            !is_dolt_port_stale(&known_ports, &beads_dirs, repo_name),
            "pool staleness must use Dolt sql-server.info, not stale legacy port file"
        );
    }

    #[test]
    fn configured_names_lists_all_registered_regardless_of_connection() {
        let mut beads_dirs = HashMap::new();
        beads_dirs.insert("alpha".to_string(), PathBuf::from("/tmp/alpha/.beads"));
        beads_dirs.insert("beta".to_string(), PathBuf::from("/tmp/beta/.beads"));
        let pool = RepoPool {
            clients: HashMap::new(),
            paths: HashMap::new(),
            beads_dirs,
            known_ports: HashMap::new(),
        };
        // configured_names reports all registered repos even though none are
        // connected (lazy) — so the startup log stays truthful.
        let mut names = pool.configured_names();
        names.sort_unstable();
        assert_eq!(names, vec!["alpha", "beta"]);
        assert_eq!(pool.connected_count(), 0);
    }
}
