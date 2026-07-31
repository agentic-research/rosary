//! Backend-aware bead-store backup & restore (`rsry bead backup` / `restore`).
//!
//! This is distinct from `rsry bead export --jsonl`, which is an *interop /
//! migration* surface carrying bead **content** only (no VCS state). A backup
//! must be **restorable and full-fidelity**, and what that means depends on the
//! backend (ADR-0014):
//!
//! - **SQLite** (`.beads/beads.db`, rosary's single-writer default) — a
//!   consistent single-file snapshot via `VACUUM INTO`. This captures the whole
//!   database (all tables, FTS, committed WAL) in one file, and restore is a
//!   file copy. Fully lossless because there is no Dolt VCS layer to preserve.
//! - **Dolt server mode** (`.beads/dolt/`) — full fidelity (branches, commit
//!   history, working set) is Dolt's own job. We do **not** shell `bd` for this
//!   (ADR-0014: never depend on bd); instead we point the operator at Dolt's
//!   native backup. Wrapping `dolt backup` in rsry is tracked as follow-up.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// Which storage backend a `.beads/` dir uses — same discriminant as
/// [`crate::bead_sqlite::connect_bead_store`] (`dolt/` present ⇒ Dolt).
#[derive(Debug, PartialEq, Eq)]
pub enum Backend {
    Sqlite,
    Dolt,
}

/// Detect the backend for a `.beads/` directory, via the single canonical
/// classifier ([`crate::bead_backend`]) so this can never disagree with
/// `connect_bead_store` or `hooks::audit` again.
pub fn detect_backend(beads_dir: &Path) -> Backend {
    if crate::bead_backend::is_dolt_backed(beads_dir) {
        Backend::Dolt
    } else {
        Backend::Sqlite
    }
}

/// Outcome of a successful [`backup`].
#[derive(Debug)]
pub struct BackupOutcome {
    pub backend: &'static str,
    pub path: PathBuf,
}

/// Back up the bead store in `beads_dir` to `dest`.
///
/// SQLite: a consistent `VACUUM INTO` snapshot. Dolt: returns a guiding error
/// (full backup is Dolt's job). Refuses to overwrite an existing `dest`.
pub fn backup(beads_dir: &Path, dest: &Path) -> Result<BackupOutcome> {
    match detect_backend(beads_dir) {
        Backend::Sqlite => {
            let db = beads_dir.join("beads.db");
            if !db.exists() {
                bail!(
                    "no SQLite bead store at {} — nothing to back up",
                    db.display()
                );
            }
            if dest.exists() {
                bail!(
                    "refusing to overwrite existing backup at {} (choose a new path)",
                    dest.display()
                );
            }
            if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating backup dir {}", parent.display()))?;
            }
            let dest_str = dest
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("backup path is not valid UTF-8"))?;
            // VACUUM INTO writes a clean, consistent copy of the whole DB
            // (incl. committed WAL) — the canonical SQLite backup primitive.
            let conn = rusqlite::Connection::open(&db)
                .with_context(|| format!("opening {}", db.display()))?;
            conn.execute("VACUUM INTO ?1", rusqlite::params![dest_str])
                .with_context(|| format!("VACUUM INTO {dest_str}"))?;
            Ok(BackupOutcome {
                backend: "sqlite",
                path: dest.to_path_buf(),
            })
        }
        Backend::Dolt => bail!(
            "this repo uses Dolt server mode ({}); a full-fidelity backup that preserves \
             branches/history is Dolt's job, not rsry's. Use Dolt's native backup \
             (e.g. a Dolt remote, or `dolt backup`) against that directory. \
             Wrapping this in rsry is tracked as follow-up.",
            crate::bead_backend::dolt_dir(beads_dir).display()
        ),
    }
}

/// Restore a SQLite bead store in `beads_dir` from a backup at `src`.
///
/// `src` is expected to be a file produced by [`backup`] (a `VACUUM INTO`
/// snapshot with no side WAL). It is copied as a single file; if you hand it a
/// *live* WAL-mode `beads.db` instead, any committed-but-uncheckpointed WAL
/// beside it is not carried over — back up properly rather than copying a live db.
///
/// Refuses to overwrite a live `beads.db` unless `force` is set. Validates that
/// `src` is a readable bead store before clobbering anything, and clears any
/// stale `-wal`/`-shm` sidecars so the restored file is authoritative.
pub fn restore(beads_dir: &Path, src: &Path, force: bool) -> Result<()> {
    if detect_backend(beads_dir) == Backend::Dolt {
        bail!(
            "this repo uses Dolt server mode ({}); restore via Dolt's native tooling, not rsry.",
            crate::bead_backend::dolt_dir(beads_dir).display()
        );
    }
    if !src.exists() {
        bail!("backup not found at {}", src.display());
    }
    // Validate the source is actually a bead store before we touch the target.
    {
        let conn = rusqlite::Connection::open(src)
            .with_context(|| format!("opening backup {}", src.display()))?;
        let has_issues: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='issues'",
                [],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if !has_issues {
            bail!(
                "{} does not look like a bead store (no `issues` table)",
                src.display()
            );
        }
    }
    let db = beads_dir.join("beads.db");
    if db.exists() && !force {
        bail!(
            "refusing to overwrite existing bead store at {} without --force",
            db.display()
        );
    }
    std::fs::create_dir_all(beads_dir)
        .with_context(|| format!("creating {}", beads_dir.display()))?;
    std::fs::copy(src, &db)
        .with_context(|| format!("copying {} -> {}", src.display(), db.display()))?;
    // Drop stale WAL/SHM so the copied file (which has no pending WAL) is the
    // source of truth rather than being shadowed by a leftover journal.
    for ext in ["beads.db-wal", "beads.db-shm"] {
        let _ = std::fs::remove_file(beads_dir.join(ext));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead_sqlite::SqliteBeadStore;
    use crate::store::BeadStore;

    fn seed_store(beads_dir: &Path) {
        std::fs::create_dir_all(beads_dir).unwrap();
        let store = SqliteBeadStore::connect(&beads_dir.join("beads.db")).unwrap();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            store
                .create_bead("rosary-aaaaaa", "keep me", "body", 2, "task")
                .await
                .unwrap();
        });
    }

    #[test]
    fn detect_backend_sqlite_vs_dolt() {
        let tmp = tempfile::tempdir().unwrap();
        let beads = tmp.path().join(".beads");
        std::fs::create_dir_all(&beads).unwrap();
        assert_eq!(detect_backend(&beads), Backend::Sqlite);
        std::fs::create_dir_all(beads.join("dolt")).unwrap();
        assert_eq!(detect_backend(&beads), Backend::Dolt);
    }

    #[test]
    fn backup_then_restore_roundtrips_the_bead() {
        let tmp = tempfile::tempdir().unwrap();
        let beads = tmp.path().join(".beads");
        seed_store(&beads);

        let dest = tmp.path().join("backup/beads.bak");
        let out = backup(&beads, &dest).unwrap();
        assert_eq!(out.backend, "sqlite");
        assert!(dest.exists(), "backup file written");

        let restored_dir = tmp.path().join("restored/.beads");
        restore(&restored_dir, &dest, false).unwrap();
        let store = SqliteBeadStore::connect(&restored_dir.join("beads.db")).unwrap();
        let bead = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async { store.get_bead("rosary-aaaaaa", "rosary").await.unwrap() });
        assert!(bead.is_some(), "restored store has the backed-up bead");
        assert_eq!(bead.unwrap().title, "keep me");
    }

    /// The load-bearing claim (module doc): `VACUUM INTO` captures data that is
    /// committed but still sitting in the WAL (not yet checkpointed into the
    /// main db file). Keep the writer connection OPEN with autocheckpoint off so
    /// the row never folds into beads.db, then back up and confirm it's present.
    #[test]
    fn backup_captures_committed_but_uncheckpointed_wal() {
        let tmp = tempfile::tempdir().unwrap();
        let beads = tmp.path().join(".beads");
        std::fs::create_dir_all(&beads).unwrap();
        let db = beads.join("beads.db");

        let writer = rusqlite::Connection::open(&db).unwrap();
        writer
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
            .unwrap();
        writer
            .execute("CREATE TABLE issues (id TEXT PRIMARY KEY, title TEXT)", [])
            .unwrap();
        writer
            .execute(
                "INSERT INTO issues (id, title) VALUES ('rosary-wal01', 'in the wal')",
                [],
            )
            .unwrap();
        assert!(
            beads.join("beads.db-wal").exists(),
            "committed write should be sitting in the WAL, not the main db"
        );

        // Back up while the writer is still open (no on-close checkpoint).
        let dest = tmp.path().join("snap.db");
        backup(&beads, &dest).unwrap();

        let rdr = rusqlite::Connection::open(&dest).unwrap();
        let title: String = rdr
            .query_row(
                "SELECT title FROM issues WHERE id='rosary-wal01'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            title, "in the wal",
            "VACUUM INTO must capture committed-but-uncheckpointed WAL data"
        );
        drop(writer);
    }

    #[test]
    fn backup_refuses_to_overwrite_existing_dest() {
        let tmp = tempfile::tempdir().unwrap();
        let beads = tmp.path().join(".beads");
        seed_store(&beads);
        let dest = tmp.path().join("b.bak");
        backup(&beads, &dest).unwrap();
        assert!(backup(&beads, &dest).is_err());
    }

    #[test]
    fn restore_refuses_overwrite_without_force_but_allows_with() {
        let tmp = tempfile::tempdir().unwrap();
        let beads = tmp.path().join(".beads");
        seed_store(&beads);
        let dest = tmp.path().join("b.bak");
        backup(&beads, &dest).unwrap();

        assert!(restore(&beads, &dest, false).is_err());
        restore(&beads, &dest, true).unwrap();
    }

    #[test]
    fn restore_rejects_non_bead_store_source() {
        let tmp = tempfile::tempdir().unwrap();
        let beads = tmp.path().join(".beads");
        std::fs::create_dir_all(&beads).unwrap();
        let bogus = tmp.path().join("not-a-db.txt");
        std::fs::write(&bogus, b"definitely not sqlite").unwrap();
        assert!(restore(&beads, &bogus, true).is_err());
    }

    #[test]
    fn backup_dolt_mode_guides_instead_of_failing_silently() {
        let tmp = tempfile::tempdir().unwrap();
        let beads = tmp.path().join(".beads");
        std::fs::create_dir_all(beads.join("dolt")).unwrap();
        let err = backup(&beads, &tmp.path().join("x.bak")).unwrap_err();
        assert!(
            err.to_string().contains("Dolt"),
            "error should point the operator at Dolt's native backup"
        );
    }
}
