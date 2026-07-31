//! Canonical bead-store BACKEND KIND detection over a `.beads/` directory.
//!
//! `.beads/` can hold up to three artifacts rsry knows about: a Dolt server
//! store (`dolt/`), a SQLite store (`beads.db`), and a bd-era embedded-Dolt
//! store (`embeddeddolt/`) that rsry cannot read at all. Which combination
//! is present determines whether a repo is readable, and — critically —
//! whether it's AMBIGUOUS (more than one live candidate, so nothing may
//! guess which is authoritative).
//!
//! Before this module, that classification was independently reimplemented
//! across ~10 files with subtly different (and disagreeing) predicates. Two
//! concrete bugs traced directly to the drift:
//!
//! - `connect_bead_store`'s runtime guard correctly bailed on `dolt/` +
//!   `beads.db` coexisting, but `hooks::audit`'s own "backend ambiguity"
//!   check (`backend_ambiguous`, rosary-b5c8a1) never tested that shape at
//!   all — it only fired on `embeddeddolt/` coexisting with something else.
//!   A real repo (cloister: a live `dolt/` server plus a stray empty
//!   `beads.db`) hit the runtime guard, silently vanished from
//!   `rsry status`/`list_beads` fleet aggregation, and the mechanical audit
//!   built specifically to catch storage-shape problems said nothing.
//! - `backend_ambiguous` ALSO flagged `embeddeddolt/` + `dolt/` and
//!   `embeddeddolt/` + `beads.db` as ambiguous — but `connect_bead_store`
//!   treats both as perfectly fine: `dolt/` and `beads.db` each
//!   unconditionally win over an unused `embeddeddolt/` (see
//!   `unreadable_backend_warning`'s doc comment), so those were false
//!   positives the whole time, disagreeing with the store's own real
//!   behavior.
//!
//! [`detect_backend`] is now the one place this decision is made; every
//! caller above derives its narrower question (which store to connect,
//! whether to warn, whether to skip a Dolt-only code path) from its answer
//! instead of re-deriving the raw path checks.

use std::path::{Path, PathBuf};

/// What `.beads/` actually holds, classified from just its directory
/// listing — no I/O beyond `exists`/`is_dir` on the three known artifact
/// names, no connection attempted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeadBackend {
    /// `dolt/` exists (regardless of `embeddeddolt/`) and `beads.db` does
    /// not — the readable, authoritative server-mode store. `embeddeddolt/`
    /// is dead weight when this variant applies; dolt always wins.
    Dolt,
    /// `beads.db` exists and `dolt/` does not — the readable, authoritative
    /// SQLite store. Same precedence: an unused `embeddeddolt/` here is
    /// dead weight too, not a conflict.
    Sqlite,
    /// Both `dolt/` and `beads.db` exist. Nothing may guess which is
    /// authoritative (rosary-9103f7) — every caller must refuse and name
    /// both paths, exactly like `connect_bead_store` does.
    Ambiguous,
    /// ONLY `embeddeddolt/` (the bd-era store) is present — data exists but
    /// rsry cannot read it at all, and there is no fallback store
    /// (rosary-21e2d4). A registered repo in this state silently reports 0
    /// beads unless something warns.
    UnreadableEmbeddedOnly,
    /// None of the three artifacts exist — an uninitialized `.beads/`.
    Uninitialized,
}

impl BeadBackend {
    /// True for the one state every ambiguity-sensitive caller must refuse
    /// to guess through.
    pub fn is_ambiguous(self) -> bool {
        matches!(self, Self::Ambiguous)
    }
}

/// `.beads/dolt`'s path, given a `.beads/` directory. Centralizes the
/// artifact-name string so it can't drift between call sites.
pub fn dolt_dir(beads_dir: &Path) -> PathBuf {
    beads_dir.join("dolt")
}

/// `.beads/beads.db`'s path, given a `.beads/` directory.
pub fn sqlite_path(beads_dir: &Path) -> PathBuf {
    beads_dir.join("beads.db")
}

/// `.beads/embeddeddolt`'s path, given a `.beads/` directory.
pub fn embedded_dolt_dir(beads_dir: &Path) -> PathBuf {
    beads_dir.join("embeddeddolt")
}

/// Is this `.beads/` a live, readable Dolt server store? Convenience for the
/// call sites that only ever need this one boolean (Dolt-only code paths,
/// port-staleness checks) rather than the full classification — still
/// routed through [`dolt_dir`] so the artifact name has one source.
pub fn is_dolt_backed(beads_dir: &Path) -> bool {
    dolt_dir(beads_dir).is_dir()
}

/// Classify what `.beads/` holds. Pure filesystem existence checks.
pub fn detect_backend(beads_dir: &Path) -> BeadBackend {
    let has_dolt = dolt_dir(beads_dir).is_dir();
    let has_sqlite = sqlite_path(beads_dir).exists();
    if has_dolt && has_sqlite {
        return BeadBackend::Ambiguous;
    }
    if has_dolt {
        return BeadBackend::Dolt;
    }
    if has_sqlite {
        return BeadBackend::Sqlite;
    }
    if embedded_dolt_dir(beads_dir).is_dir() {
        return BeadBackend::UnreadableEmbeddedOnly;
    }
    BeadBackend::Uninitialized
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(dolt: bool, sqlite: bool, embedded: bool) -> tempfile::TempDir {
        let tmp = tempfile::TempDir::new().unwrap();
        if dolt {
            std::fs::create_dir_all(dolt_dir(tmp.path())).unwrap();
        }
        if sqlite {
            std::fs::write(sqlite_path(tmp.path()), b"").unwrap();
        }
        if embedded {
            std::fs::create_dir_all(embedded_dolt_dir(tmp.path())).unwrap();
        }
        tmp
    }

    #[test]
    fn dolt_only() {
        let tmp = make(true, false, false);
        assert_eq!(detect_backend(tmp.path()), BeadBackend::Dolt);
    }

    #[test]
    fn sqlite_only() {
        let tmp = make(false, true, false);
        assert_eq!(detect_backend(tmp.path()), BeadBackend::Sqlite);
    }

    #[test]
    fn embedded_only_is_unreadable() {
        let tmp = make(false, false, true);
        assert_eq!(
            detect_backend(tmp.path()),
            BeadBackend::UnreadableEmbeddedOnly
        );
    }

    #[test]
    fn nothing_is_uninitialized() {
        let tmp = make(false, false, false);
        assert_eq!(detect_backend(tmp.path()), BeadBackend::Uninitialized);
    }

    #[test]
    fn dolt_and_sqlite_is_ambiguous() {
        let tmp = make(true, true, false);
        assert_eq!(detect_backend(tmp.path()), BeadBackend::Ambiguous);
        assert!(detect_backend(tmp.path()).is_ambiguous());
    }

    /// The false-positive `hooks::audit` had before this module: dolt wins
    /// over an unused embeddeddolt, this is NOT ambiguous.
    #[test]
    fn dolt_and_embedded_is_dolt_not_ambiguous() {
        let tmp = make(true, false, true);
        assert_eq!(detect_backend(tmp.path()), BeadBackend::Dolt);
    }

    /// The other false-positive: sqlite wins over an unused embeddeddolt.
    #[test]
    fn sqlite_and_embedded_is_sqlite_not_ambiguous() {
        let tmp = make(false, true, true);
        assert_eq!(detect_backend(tmp.path()), BeadBackend::Sqlite);
    }

    /// The exact cloister shape that motivated this module: all three
    /// present. Dolt+sqlite ambiguity is the one that matters — dolt would
    /// otherwise silently win over the phantom beads.db and mask it.
    #[test]
    fn all_three_present_is_ambiguous() {
        let tmp = make(true, true, true);
        assert_eq!(detect_backend(tmp.path()), BeadBackend::Ambiguous);
    }

    #[test]
    fn is_dolt_backed_matches_dolt_dir_presence() {
        assert!(is_dolt_backed(make(true, false, false).path()));
        assert!(!is_dolt_backed(make(false, true, false).path()));
    }
}
