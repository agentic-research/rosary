//! Personal-tier beads — the third home (ADR-0012, ADR-0022).
//!
//! ADR-0012 (**Accepted**) decides `~/.rsry/personal.db` is the **live working
//! store** for personal beads — "same backend pattern as the orchestrator's
//! `~/.rsry/backend.db`, zero new infra". The **durable, synced artifact** is a
//! separate concern: per-bead `age`-encrypted content-addressed blobs pushed by
//! a `SyncBackend`/`GitRepoBackend` (`rosary-e52b24`, unbuilt).
//!
//! This module is the *home*, not the *transport*. That split is ADR-0012's own
//! (its migration list has `GitRepoBackend` as step 1 of 5, after the store),
//! and it is what lets ADR-0022's goal — canonical storage absorbs none of the
//! other roles — be satisfied before encryption and attestation land.
//!
//! ## What this deliberately does NOT do yet
//!
//! - **No encryption.** Blobs are ADR-0012's sync artifact, not its store. A
//!   personal bead here is as protected as `~/.rsry/backend.db` is today — i.e.
//!   by filesystem permissions, in a directory outside any project repo.
//! - **No signet attestation gate.** ADR-0012 requires a fail-closed signed
//!   consent chain on personal writes; that is `rosary-e55ec9`.
//!
//! Both are tracked. The important property held here is the ADR-0022 one:
//! a personal bead never touches a project repo's store, its tracked JSONL, or
//! its working tree.

use std::path::PathBuf;

use anyhow::{Context, Result};

/// Repo name recorded on personal beads. Not a real repo — personal beads
/// belong to the operator, not to a project, and ADR-0012 puts them "outside
/// any project repo".
///
/// Currently read only by tests and by callers reading the personal store
/// back; kept public because it is the store's addressing convention, not an
/// implementation detail.
#[allow(dead_code)]
pub const SCOPE: &str = "personal";

/// Path to the personal working store. Honours `RSRY_HOME` so tests (and
/// anyone with a non-default layout) can redirect it without touching `$HOME`.
pub fn store_path() -> Result<PathBuf> {
    let root = match std::env::var("RSRY_HOME") {
        Ok(p) if !p.trim().is_empty() => PathBuf::from(p),
        _ => {
            let home = std::env::var("HOME").context(
                "cannot locate the personal bead store: neither RSRY_HOME nor HOME is set",
            )?;
            PathBuf::from(home).join(".rsry")
        }
    };
    Ok(root.join("personal.db"))
}

/// Open (creating if absent) the personal working store.
///
/// Creation is intentional here, unlike the repo path where implicit creation
/// is the `rosary-554a74` defect: there is exactly one personal store at a
/// known location, so "it does not exist yet" is first use, not ambiguity.
pub fn open() -> Result<crate::bead_sqlite::SqliteBeadStore> {
    let path = store_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    crate::bead_sqlite::SqliteBeadStore::connect(&path)
        .with_context(|| format!("opening the personal bead store at {}", path.display()))
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::store::BeadStore;

    /// A guard that points RSRY_HOME at a temp dir. `std::env::set_var` is
    /// process-global, so these tests are serialised by a mutex rather than
    /// racing each other.
    pub struct HomeGuard {
        _tmp: tempfile::TempDir,
        _lock: std::sync::MutexGuard<'static, ()>,
        prev: Option<String>,
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    impl HomeGuard {
        pub fn new() -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let tmp = tempfile::tempdir().unwrap();
            let prev = std::env::var("RSRY_HOME").ok();
            unsafe { std::env::set_var("RSRY_HOME", tmp.path()) };
            Self {
                _tmp: tmp,
                _lock: lock,
                prev,
            }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => unsafe { std::env::set_var("RSRY_HOME", v) },
                None => unsafe { std::env::remove_var("RSRY_HOME") },
            }
        }
    }

    #[test]
    fn store_path_honours_rsry_home() {
        let _g = HomeGuard::new();
        let p = store_path().unwrap();
        assert!(p.ends_with("personal.db"), "got {}", p.display());
        assert!(
            p.starts_with(std::env::var("RSRY_HOME").unwrap()),
            "must live under RSRY_HOME, got {}",
            p.display()
        );
    }

    /// ADR-0012: personal state lives OUTSIDE any project repo. The path must
    /// never resolve into a repo's `.beads/`.
    #[test]
    fn personal_store_is_outside_any_project_repo() {
        let _g = HomeGuard::new();
        let p = store_path().unwrap();
        assert!(
            !p.to_string_lossy().contains(".beads"),
            "personal store must not live in a repo's .beads/, got {}",
            p.display()
        );
    }

    #[tokio::test]
    async fn open_creates_the_store_and_round_trips_a_bead() {
        let _g = HomeGuard::new();
        let store = open().unwrap();
        store
            .create_bead("personal-1", "a private note", "body", 2, "task")
            .await
            .unwrap();
        let got = store.get_bead("personal-1", SCOPE).await.unwrap().unwrap();
        assert_eq!(got.title, "a private note");
        assert!(store_path().unwrap().exists(), "store file must exist");
    }
}
