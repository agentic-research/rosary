//! Long-lived connection pool for bead stores across registered repos.
//!
//! The MCP server creates a `RepoPool` on startup, connecting to all
//! repos with .beads/ directories. Connections are reused across tool
//! calls — no per-request connect overhead.
//!
//! ## Staleness (rosary-2b8568)
//!
//! The pool used to be a `HashMap` snapshotted once at boot and never
//! revisited: a repo registered (or given its first `.beads/`) after the
//! MCP daemon started was invisible to it until restart, and a repo whose
//! registered path changed kept serving whatever it connected to at boot —
//! silently, with no error. The CLI has no such problem (fresh process,
//! fresh config read, every invocation), so the observable symptom was "the
//! CLI wrote to a different store than the MCP" for exactly as long as the
//! daemon had been running since that repo's config last changed.
//!
//! The fix: every lookup cheaply checks `~/.rsry/config.toml`'s mtime (one
//! `stat`, the same order of cost the existing Dolt-port check already pays
//! per call) and, only when it changed, re-scans the config under a write
//! lock — reaping (dropping) any pooled connection whose registered path
//! moved or which disappeared from config entirely, and picking up any repo
//! that's newly registered or just grew a `.beads/`. The actual reconnect
//! happens lazily, on the next `get`/`get_by_path` for that repo, via
//! [`RepoPool::reap_and_reconnect`] — old connections are simply dropped
//! (Arc refcounting lets any in-flight caller finish against the copy it
//! already holds) and replaced, never force-killed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

use anyhow::Result;
use tokio::sync::RwLock;

use crate::config;
use crate::store::BeadStore;

/// Read the current Dolt server port from a `.beads/` directory.
/// Returns `None` if the port file is absent or unparseable.
fn read_dolt_port(beads_dir: &Path) -> Option<u16> {
    if crate::bead_backend::is_dolt_backed(beads_dir)
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
    if !crate::bead_backend::is_dolt_backed(beads_dir) {
        return false;
    }
    match read_dolt_port(beads_dir) {
        Some(current) => current != *known,
        None => true,
    }
}

fn mtime_secs(path: &str) -> Option<u64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

#[derive(Default)]
struct PoolState {
    clients: HashMap<String, Arc<dyn BeadStore>>,
    paths: HashMap<String, PathBuf>,
    /// `.beads/` directories used when connecting (for reconnect + port check).
    beads_dirs: HashMap<String, PathBuf>,
    /// Dolt server port observed at connect time. Used to detect restarts.
    known_ports: HashMap<String, u16>,
}

/// Long-lived pool of BeadStore connections keyed by repo name.
pub struct RepoPool {
    /// Empty string ("" — from `empty()`/tests) means "never refresh".
    config_path: String,
    /// Last-observed mtime (epoch seconds) of `config_path`. `0` forces a
    /// refresh on first use even if the file's actual mtime is old.
    config_mtime: AtomicU64,
    state: RwLock<PoolState>,
}

impl RepoPool {
    /// Create an empty pool (for testing and HTTP server startup with no repos).
    #[allow(dead_code)] // used in tests
    pub fn empty() -> Self {
        RepoPool {
            config_path: String::new(),
            config_mtime: AtomicU64::new(0),
            state: RwLock::new(PoolState::default()),
        }
    }

    /// Build a pool holding a single already-connected store, for tests.
    /// `config_path` is empty, so the staleness refresh is a permanent no-op.
    #[cfg(test)]
    pub fn from_client(name: &str, path: PathBuf, store: Box<dyn BeadStore>) -> Self {
        let mut clients: HashMap<String, Arc<dyn BeadStore>> = HashMap::new();
        clients.insert(name.to_string(), Arc::from(store));
        let mut paths = HashMap::new();
        paths.insert(name.to_string(), path);
        RepoPool {
            config_path: String::new(),
            config_mtime: AtomicU64::new(0),
            state: RwLock::new(PoolState {
                clients,
                paths,
                beads_dirs: HashMap::new(),
                known_ports: HashMap::new(),
            }),
        }
    }

    /// Build a pool that RECORDS the config but connects NOTHING (rosary-31193d).
    ///
    /// Eagerly connecting every configured repo here was the dolt-server leak: it
    /// spawned/attached a `dolt sql-server` per Dolt repo on *every* MCP server
    /// start — 34 repos → 34 servers → ~6.5GB — even for a single-repo op
    /// (rosary-a7a668). Now stores are opened lazily, per repo actually used:
    /// [`get`](Self::get)/[`get_by_path`](Self::get_by_path) connect on demand via
    /// the recorded `beads_dirs`, and the ad-hoc fallback in `get_client` handles
    /// the rest. So startup spawns zero servers; a single-repo op touches exactly
    /// one.
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
            config_path: config_path.to_string(),
            config_mtime: AtomicU64::new(mtime_secs(config_path).unwrap_or(0)),
            state: RwLock::new(PoolState {
                clients: HashMap::new(),
                paths,
                beads_dirs,
                known_ports: HashMap::new(),
            }),
        })
    }

    /// Cheap staleness gate: one `stat` on the config file. If its mtime hasn't
    /// moved since the last refresh, this is a no-op — the common case, paid on
    /// every `get`/`get_by_path` the same way the existing Dolt-port check
    /// already is. Only when the mtime DID move does this take a write lock and
    /// re-scan config, reaping any repo whose registered path moved or vanished
    /// (its pooled connection is dropped — reconnected lazily on next access) and
    /// picking up anything newly registered or newly `.beads/`-bearing.
    async fn ensure_fresh(&self) {
        if self.config_path.is_empty() {
            return; // tests / `empty()` — nothing to refresh against.
        }
        let Some(observed) = mtime_secs(&self.config_path) else {
            return;
        };
        if observed <= self.config_mtime.load(Ordering::Acquire) {
            return;
        }
        let Ok(cfg) = config::load_merged(&self.config_path) else {
            return;
        };
        let mut state = self.state.write().await;
        // Double-checked: another caller may have already refreshed while we
        // waited for the write lock.
        if observed <= self.config_mtime.load(Ordering::Acquire) {
            return;
        }

        let mut seen = std::collections::HashSet::new();
        for repo in &cfg.repo {
            let path = crate::scanner::expand_path(&repo.path);
            let beads_dir = path.join(".beads");
            seen.insert(repo.name.clone());
            if !beads_dir.exists() {
                continue;
            }
            let moved = state.paths.get(&repo.name) != Some(&path);
            state.paths.insert(repo.name.clone(), path);
            state.beads_dirs.insert(repo.name.clone(), beads_dir);
            if moved {
                // Reap — drop the connection to wherever this name used to
                // point. The next get() reconnects to the current path.
                state.clients.remove(&repo.name);
                state.known_ports.remove(&repo.name);
            }
        }
        // Reap repos no longer in config, or whose `.beads/` disappeared.
        let gone: Vec<String> = state
            .paths
            .keys()
            .filter(|n| !seen.contains(*n))
            .cloned()
            .collect();
        for name in gone {
            state.paths.remove(&name);
            state.beads_dirs.remove(&name);
            state.clients.remove(&name);
            state.known_ports.remove(&name);
        }
        self.config_mtime.store(observed, Ordering::Release);
    }

    /// Drop any stale entry for `repo_name` and connect fresh from its
    /// recorded `beads_dir`. Returns `None` if the name isn't (or is no
    /// longer) a known repo.
    async fn reap_and_reconnect(&self, repo_name: &str) -> Option<Arc<dyn BeadStore>> {
        let beads_dir = {
            let state = self.state.read().await;
            state.beads_dirs.get(repo_name)?.clone()
        };
        let store: Arc<dyn BeadStore> = Arc::from(
            crate::bead_sqlite::connect_bead_store(&beads_dir)
                .await
                .ok()?,
        );
        let port = read_dolt_port(&beads_dir);
        let mut state = self.state.write().await;
        state.clients.insert(repo_name.to_string(), store.clone());
        match port {
            Some(p) => {
                state.known_ports.insert(repo_name.to_string(), p);
            }
            None => {
                state.known_ports.remove(repo_name);
            }
        }
        Some(store)
    }

    /// The recorded repo root path for a registered repo name, without
    /// connecting. Lets a scope-only call (`scope: "repo:X"`, no `repo_path`)
    /// resolve X's path and lazily open just that store — rosary-31193d.
    pub async fn path_for(&self, repo_name: &str) -> Option<PathBuf> {
        self.ensure_fresh().await;
        self.state.read().await.paths.get(repo_name).cloned()
    }

    /// Every configured repo name (whether or not it's connected). Used for
    /// the startup log + cross-repo handlers, since `clients` is now lazy.
    pub async fn configured_names(&self) -> Vec<String> {
        self.ensure_fresh().await;
        self.state.read().await.beads_dirs.keys().cloned().collect()
    }

    /// Open a store for EVERY configured repo (ad hoc, best-effort). For the
    /// cross-repo handlers (webhooks, ticket-load) that must search all repos
    /// for a bead without knowing which one holds it. Connects on demand — a
    /// webhook firing is far rarer than an MCP startup, so this doesn't
    /// reintroduce the startup fan-out.
    pub async fn connect_all(&self) -> Vec<(String, Box<dyn BeadStore>)> {
        self.ensure_fresh().await;
        let beads_dirs: Vec<(String, PathBuf)> = {
            let state = self.state.read().await;
            state
                .beads_dirs
                .iter()
                .map(|(n, d)| (n.clone(), d.clone()))
                .collect()
        };
        let mut out = Vec::new();
        for (name, beads_dir) in beads_dirs {
            match crate::bead_sqlite::connect_bead_store(&beads_dir).await {
                Ok(store) => out.push((name, store)),
                Err(e) => eprintln!("[pool] connect_all: skipping {name} ({e})"),
            }
        }
        out
    }

    /// Get a BeadStore by repo name. Reconnects transparently if the pooled
    /// entry is stale (Dolt port changed, or the repo's registered path moved
    /// since it was connected) or if the repo was only just registered/given
    /// a `.beads/` after this pool was built. Returns `None` only if the name
    /// is not a known repo at all.
    pub async fn get(&self, repo_name: &str) -> Option<Arc<dyn BeadStore>> {
        self.ensure_fresh().await;
        let stale = {
            let state = self.state.read().await;
            if is_dolt_port_stale(&state.known_ports, &state.beads_dirs, repo_name) {
                true
            } else if let Some(store) = state.clients.get(repo_name) {
                return Some(store.clone());
            } else if state.beads_dirs.contains_key(repo_name) {
                true // known repo, never connected yet
            } else {
                return None; // not a known repo at all
            }
        };
        if stale {
            eprintln!("[pool] {repo_name}: reconnecting (stale or first use)");
        }
        self.reap_and_reconnect(repo_name).await
    }

    /// Get a BeadStore by repo path (resolves name from path).
    /// Canonicalizes symlink aliases on both sides, so the same physical repo
    /// addressed via a different alias (e.g. `~/github/art/X` vs
    /// `~/remotes/art/X`, or macOS `/var` vs `/private/var`) matches its
    /// registered entry instead of silently missing (rosary-617010).
    pub async fn get_by_path(&self, repo_path: &str) -> Option<(String, Arc<dyn BeadStore>)> {
        self.ensure_fresh().await;
        let target = Path::new(repo_path);
        let discovered = config::discover_repo_root(target).unwrap_or_else(|| target.to_path_buf());
        let root = crate::scanner::canonicalize_repo_path(&discovered);

        let name = {
            let state = self.state.read().await;
            state
                .paths
                .iter()
                .find(|(_, path)| crate::scanner::canonicalize_repo_path(path) == root)
                .map(|(name, _)| name.clone())?
        };
        let store = self.get(&name).await?;
        Some((name, store))
    }

    /// Number of repos actually connected so far (lazy — grows on use, not at
    /// startup). Test-only: pins that `from_config` connects nothing.
    #[cfg(test)]
    pub async fn connected_count(&self) -> usize {
        self.state.read().await.clients.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_pool() {
        let pool = RepoPool::empty();
        assert_eq!(pool.connected_count().await, 0);
        assert!(pool.get("nonexistent").await.is_none());
        assert!(pool.get_by_path("/tmp/fake").await.is_none());
        assert!(pool.path_for("nonexistent").await.is_none());
    }

    /// rosary-617010: a repo registered under its real path must be found when
    /// looked up via a SYMLINK ALIAS of the same physical dir (the ~/github →
    /// ~/remotes case). Before the fix, get_by_path compared paths with exact
    /// equality and silently missed the alias → "search returns empty".
    #[tokio::test]
    async fn get_by_path_matches_symlink_alias() {
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
            pool.get_by_path(real.to_str().unwrap()).await.is_some(),
            "real path should match its registered store"
        );
        // …AND via the symlink alias — the bug was this used to return None.
        assert!(
            pool.get_by_path(alias.to_str().unwrap()).await.is_some(),
            "symlink alias must resolve to the same registered store"
        );
        // A genuinely unrelated path still misses.
        assert!(
            pool.get_by_path("/tmp/definitely-not-a-repo-617010")
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn path_for_resolves_without_connecting() {
        // A registered repo's path is available for lazy connect (scope-only
        // calls) without any store being connected.
        let pool = RepoPool {
            config_path: String::new(),
            config_mtime: AtomicU64::new(0),
            state: RwLock::new(PoolState {
                clients: HashMap::new(),
                paths: HashMap::from([("myrepo".to_string(), PathBuf::from("/tmp/myrepo"))]),
                beads_dirs: HashMap::new(),
                known_ports: HashMap::new(),
            }),
        };
        assert_eq!(
            pool.path_for("myrepo").await,
            Some(PathBuf::from("/tmp/myrepo"))
        );
        assert!(
            pool.connected_count().await == 0,
            "path lookup connects nothing"
        );
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
            pool.configured_names()
                .await
                .contains(&"myrepo".to_string()),
            "repo is recorded in config"
        );
        assert_eq!(
            pool.connected_count().await,
            0,
            "from_config must connect nothing (no eager fan-out)"
        );
        assert_eq!(
            pool.path_for("myrepo").await,
            Some(repo.clone()),
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
        assert!(pool.get("fake").await.is_none());
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

    #[tokio::test]
    async fn configured_names_lists_all_registered_regardless_of_connection() {
        let pool = RepoPool {
            config_path: String::new(),
            config_mtime: AtomicU64::new(0),
            state: RwLock::new(PoolState {
                clients: HashMap::new(),
                paths: HashMap::new(),
                beads_dirs: HashMap::from([
                    ("alpha".to_string(), PathBuf::from("/tmp/alpha/.beads")),
                    ("beta".to_string(), PathBuf::from("/tmp/beta/.beads")),
                ]),
                known_ports: HashMap::new(),
            }),
        };
        // configured_names reports all registered repos even though none are
        // connected (lazy) — so the startup log stays truthful.
        let mut names = pool.configured_names().await;
        names.sort_unstable();
        assert_eq!(names, vec!["alpha".to_string(), "beta".to_string()]);
        assert_eq!(pool.connected_count().await, 0);
    }

    /// rosary-2b8568: a repo whose registered path CHANGES after the pool
    /// connected it must be reaped and reconnected to the new path — not
    /// silently served from the old one forever.
    #[tokio::test]
    async fn get_reaps_and_reconnects_when_registered_path_moves() {
        let tmp = tempfile::TempDir::new().unwrap();
        let old_repo = tmp.path().join("old-location");
        let new_repo = tmp.path().join("new-location");
        std::fs::create_dir_all(old_repo.join(".beads")).unwrap();
        std::fs::create_dir_all(new_repo.join(".beads")).unwrap();
        crate::bead_sqlite::SqliteBeadStore::connect(&old_repo.join(".beads/beads.db")).unwrap();
        crate::bead_sqlite::SqliteBeadStore::connect(&new_repo.join(".beads/beads.db")).unwrap();

        let config_path = tmp.path().join("rosary.toml");
        std::fs::write(
            &config_path,
            format!(
                "[[repo]]\nname = \"movedrepo\"\npath = \"{}\"\n",
                old_repo.display()
            ),
        )
        .unwrap();

        let pool = RepoPool::from_config(config_path.to_str().unwrap())
            .await
            .unwrap();
        assert_eq!(pool.path_for("movedrepo").await, Some(old_repo.clone()));
        // Connect it once, at the old location.
        assert!(pool.get("movedrepo").await.is_some());
        assert_eq!(pool.connected_count().await, 1);

        // Re-register the SAME name at a DIFFERENT path — mimics `rsry init`
        // moving/re-pointing a repo, or a stale MCP daemon outliving a config
        // change. Sleep 1s so the mtime we compare against is guaranteed to
        // tick forward on filesystems with second-granularity mtimes.
        std::thread::sleep(std::time::Duration::from_secs(1));
        std::fs::write(
            &config_path,
            format!(
                "[[repo]]\nname = \"movedrepo\"\npath = \"{}\"\n",
                new_repo.display()
            ),
        )
        .unwrap();

        // No restart — same pool, same process. It must pick up the move.
        assert_eq!(pool.path_for("movedrepo").await, Some(new_repo.clone()));
        assert!(
            pool.get("movedrepo").await.is_some(),
            "must reconnect at the new path rather than staying missing"
        );
    }

    /// rosary-2b8568: a repo that had NO `.beads/` (and so was entirely absent
    /// from the pool) at boot must become visible once it's registered and
    /// gains a `.beads/`, without restarting the daemon.
    #[tokio::test]
    async fn get_picks_up_a_repo_registered_after_pool_was_built() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("rosary.toml");
        std::fs::write(&config_path, "").unwrap();

        let pool = RepoPool::from_config(config_path.to_str().unwrap())
            .await
            .unwrap();
        assert!(pool.get("freshrepo").await.is_none());

        std::thread::sleep(std::time::Duration::from_secs(1));
        let repo = tmp.path().join("freshrepo");
        std::fs::create_dir_all(repo.join(".beads")).unwrap();
        crate::bead_sqlite::SqliteBeadStore::connect(&repo.join(".beads/beads.db")).unwrap();
        std::fs::write(
            &config_path,
            format!(
                "[[repo]]\nname = \"freshrepo\"\npath = \"{}\"\n",
                repo.display()
            ),
        )
        .unwrap();

        assert!(
            pool.get("freshrepo").await.is_some(),
            "a repo registered after boot must become reachable without a restart"
        );
    }
}
