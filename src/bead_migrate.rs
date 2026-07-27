//! Per-repo bead-store migration (ADR-0021 slice 4).
//!
//! With reads and writes now single-sourced (slices 1-2) and gated (slice 3a),
//! a store-to-store migration is a projection round-trip: read every bead
//! full-fidelity from `source`, write it to `target`, restore the fields
//! `create_bead_full` doesn't take (status, external_ref), copy dependency edges
//! and comments, then VERIFY field-by-field. The caller performs the atomic
//! filesystem swap (rename `.beads/dolt` → `.beads/dolt.bak`, flip metadata)
//! ONLY after [`verify_migration`] returns Ok — so a mismatch aborts leaving the
//! source untouched.
//!
//! Dormant until the CLI wires it (slice 4b, the live-store step); the core +
//! its tests exercise the boundary now, so `allow(dead_code)`.
#![allow(dead_code)]

use std::path::Path;

use anyhow::{Context, Result};

use crate::bead::Bead;
use crate::bead_sqlite::SqliteBeadStore;
use crate::store::{BeadStore, NewBead};

/// Counts from a completed [`migrate_store`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct MigrationReport {
    pub beads: usize,
    pub dependencies: usize,
    /// Subset of `dependencies` whose target bead is NOT in this repo (a
    /// dangling cross-repo edge, e.g. a rosary bead blocking on a mache bead).
    /// Preserved verbatim; surfaced as a diagnostic because it's the edge class
    /// that broke the naive migration.
    pub cross_repo_dependencies: usize,
    pub comments: usize,
    /// Beads carrying a non-empty `acceptance_criteria` in the MIGRATED store.
    /// Surfaced because a read-lossy source reader once silently dropped every
    /// close condition and `verify` couldn't see it (lossy==lossy) — a visible
    /// non-zero count here is the human-checkable proof it survived.
    pub beads_with_acceptance: usize,
}

/// Copy every bead (+ dependency edges + live comments) from `source` into
/// `target`, full-fidelity. Two passes so dependency targets exist before edges
/// reference them. Assumes `target` starts empty. Touches no filesystem and
/// swaps no stores — that is the caller's atomic step, gated on
/// [`verify_migration`].
///
/// Fidelity note: bead *content* (status, priority, type, acceptance,
/// external_ref, scope, files, provenance, owner, timestamps, deps) is
/// preserved exactly. Not carried, by trait limitation, is the comment **audit
/// trail** (exact timestamps, edited/deleted state): `add_comment` can only
/// reproduce body+author. A raw-row copy for comment history is a follow-up.
pub async fn migrate_store(
    source: &dyn BeadStore,
    target: &SqliteBeadStore,
    repo_name: &str,
) -> Result<MigrationReport> {
    let beads = source
        .list_all_beads(repo_name)
        .await
        .context("reading source beads")?;
    // Count close conditions the SOURCE reader actually saw. If the reader is
    // lossy on acceptance_criteria (the rosary-a18a1f class), this is 0 even
    // when the raw store has them — a visible red flag in the dry-run diagnostic.
    let beads_with_acceptance = beads
        .iter()
        .filter(|b| !b.acceptance_criteria.trim().is_empty())
        .count();
    let mut report = MigrationReport {
        beads_with_acceptance,
        ..Default::default()
    };

    // Pass 1: every bead, so dependency targets exist for pass 2.
    for b in &beads {
        target
            .create_bead_full(bead_to_new(b))
            .await
            .with_context(|| format!("writing bead {}", b.id))?;
        // create_bead_full hardcodes status='open' and sets no external_ref —
        // restore the real values. `restore_status` bypasses the transition
        // guard (a migration reconstructs existing state, it doesn't transition).
        if b.status != "open" {
            target
                .restore_status(&b.id, &b.status)
                .await
                .with_context(|| format!("restoring status for {}", b.id))?;
        }
        if let Some(ext) = b.external_ref.as_deref().filter(|e| !e.is_empty()) {
            target
                .set_external_ref(&b.id, ext)
                .await
                .with_context(|| format!("restoring external_ref for {}", b.id))?;
        }
        target
            .restore_timestamps(&b.id, b.created_at, b.updated_at)
            .await
            .with_context(|| format!("restoring timestamps for {}", b.id))?;
        report.beads += 1;
    }

    // Set of this repo's bead ids, to flag dangling cross-repo edges.
    let local_ids: std::collections::HashSet<&str> = beads.iter().map(|b| b.id.as_str()).collect();

    // Pass 2: dependency edges + live comments (all targets now exist).
    for b in &beads {
        for dep in source.get_dependencies(&b.id).await.unwrap_or_default() {
            // restore_dependency (not add_dependency): preserve CROSS-REPO edges
            // verbatim — a rosary bead may block on a mache bead, which the
            // target holds as a dangling depends_on_id (no FK) but which
            // add_dependency's existence check would reject (found by the dry
            // run against the live store, 7/237 edges).
            target
                .restore_dependency(&b.id, &dep)
                .await
                .with_context(|| format!("copying dependency {} -> {dep}", b.id))?;
            report.dependencies += 1;
            if !local_ids.contains(dep.as_str()) {
                report.cross_repo_dependencies += 1;
            }
        }
        for c in source.list_comments(&b.id, false).await.unwrap_or_default() {
            target
                .add_comment(&b.id, &c.text, &c.author)
                .await
                .with_context(|| format!("copying comment on {}", b.id))?;
            report.comments += 1;
        }
    }

    Ok(report)
}

fn bead_to_new(b: &Bead) -> NewBead {
    NewBead {
        id: b.id.clone(),
        title: b.title.clone(),
        description: b.description.clone(),
        priority: b.priority,
        issue_type: b.issue_type.clone(),
        owner: b.owner.clone().unwrap_or_default(),
        files: b.files.clone(),
        test_files: b.test_files.clone(),
        depends_on: Vec::new(), // edges added in pass 2, after targets exist
        created_by: b.created_by.clone(),
        scope: b.scope.clone(),
        derived_from: b.derived_from.clone(),
        acceptance_criteria: b.acceptance_criteria.clone(),
    }
}

/// Verify every source bead exists in `target` with matching canonical content.
/// Field-level, NOT count-only: a row-count check passes while every bead
/// silently resets to `open` — the exact data-loss this migration must never
/// ship. Returns the first mismatch as an error (so the caller aborts the swap).
pub async fn verify_migration(
    source: &dyn BeadStore,
    target: &dyn BeadStore,
    repo_name: &str,
) -> Result<()> {
    let src = source.list_all_beads(repo_name).await?;
    for b in &src {
        let t = target
            .get_bead(&b.id, repo_name)
            .await?
            .ok_or_else(|| anyhow::anyhow!("bead {} missing in migrated store", b.id))?;

        macro_rules! same {
            ($field:ident) => {
                if t.$field != b.$field {
                    anyhow::bail!(
                        "bead {}: {} mismatch after migration ({:?} != {:?})",
                        b.id,
                        stringify!($field),
                        t.$field,
                        b.$field
                    );
                }
            };
        }
        same!(title);
        same!(description);
        same!(priority);
        // Status is compared CANONICALLY: the migration normalizes legacy
        // aliases (Dolt's "closed" → "done"), which is intended, not data loss —
        // so compare BeadState, not the raw string.
        if crate::bead::BeadState::from(t.status.as_str())
            != crate::bead::BeadState::from(b.status.as_str())
        {
            anyhow::bail!(
                "bead {}: status mismatch after migration ({:?} != {:?})",
                b.id,
                t.status,
                b.status
            );
        }
        same!(issue_type);
        same!(owner);
        same!(scope);
        same!(external_ref);
        same!(acceptance_criteria);
        same!(files);
        same!(test_files);
        same!(created_by);
        same!(created_at);
        same!(updated_at);

        // Dependency edges must match exactly (set equality).
        let mut s_deps = source.get_dependencies(&b.id).await.unwrap_or_default();
        let mut t_deps = target.get_dependencies(&b.id).await.unwrap_or_default();
        s_deps.sort();
        t_deps.sort();
        if s_deps != t_deps {
            anyhow::bail!(
                "bead {}: dependency edges differ after migration ({t_deps:?} != {s_deps:?})",
                b.id
            );
        }
    }
    Ok(())
}

/// Atomically switch a verified `.beads/` from Dolt to SQLite. `built_db` is a
/// complete SQLite store already migrated + verified. Order is chosen so there
/// is **no empty-store window**: the full `beads.db` is put in place *before*
/// `dolt/` is renamed away — while `dolt/` exists, `connect_bead_store` returns
/// Dolt and ignores `beads.db`; the instant `dolt/` becomes `dolt.bak`,
/// `beads.db` is already the complete store. `dolt/` is renamed to `dolt.bak`,
/// **never deleted** (it is the backup). Pure filesystem — the caller stops the
/// dolt-server and flips `metadata.json` around this.
pub fn swap_dolt_to_sqlite(beads_dir: &Path, built_db: &Path) -> Result<()> {
    let dolt = beads_dir.join("dolt");
    let dolt_bak = beads_dir.join("dolt.bak");
    let final_db = beads_dir.join("beads.db");
    if !dolt.is_dir() {
        anyhow::bail!(
            "{} is not a Dolt store — refusing to swap",
            beads_dir.display()
        );
    }
    if dolt_bak.exists() {
        anyhow::bail!(
            "{} already exists — refusing to clobber a prior backup",
            dolt_bak.display()
        );
    }
    if !built_db.is_file() {
        anyhow::bail!("built store {} is missing", built_db.display());
    }
    // Clear stale SQLite artifacts (the 0-byte eadfbe stub, the migrated marker,
    // and any journals) so the moved store is authoritative.
    for f in [
        "beads.db",
        "beads.db.migrated",
        "beads.db-wal",
        "beads.db-shm",
    ] {
        let _ = std::fs::remove_file(beads_dir.join(f));
    }
    // Put the full store in place (still ignored while dolt/ exists).
    std::fs::rename(built_db, &final_db)
        .with_context(|| format!("moving built store into {}", final_db.display()))?;
    // THE switch: after this rename, connect_bead_store resolves to beads.db.
    std::fs::rename(&dolt, &dolt_bak)
        .with_context(|| format!("renaming {} -> {}", dolt.display(), dolt_bak.display()))?;
    Ok(())
}

/// Rewrite `.beads/metadata.json` to declare the SQLite backend after a swap.
/// Best-effort but reported: preserves other keys, sets `backend`/`database` to
/// `sqlite` and drops the `dolt_*` keys.
pub fn flip_metadata_to_sqlite(beads_dir: &Path) -> Result<()> {
    let path = beads_dir.join("metadata.json");
    let mut meta: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(obj) = meta.as_object_mut() {
        obj.insert("backend".into(), serde_json::json!("sqlite"));
        obj.insert("database".into(), serde_json::json!("sqlite"));
        obj.remove("dolt_mode");
        obj.remove("dolt_database");
    }
    std::fs::write(&path, format!("{}\n", serde_json::to_string_pretty(&meta)?))
        .with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead_sqlite::SqliteBeadStore;
    use std::path::Path;

    fn store() -> SqliteBeadStore {
        SqliteBeadStore::connect(Path::new(":memory:")).unwrap()
    }

    // --- atomic swap (pure filesystem) ---

    #[test]
    fn swap_puts_sqlite_in_place_and_preserves_dolt_as_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let beads = tmp.path().join(".beads");
        std::fs::create_dir_all(beads.join("dolt")).unwrap();
        std::fs::write(beads.join("dolt/data"), b"dolt-store").unwrap();
        // A pre-existing 0-byte stub (the eadfbe artifact) that must be cleared.
        std::fs::write(beads.join("beads.db"), b"").unwrap();
        let built = beads.join("beads.db.new");
        std::fs::write(&built, b"MIGRATED-SQLITE").unwrap();

        swap_dolt_to_sqlite(&beads, &built).unwrap();

        // dolt/ preserved as dolt.bak (never deleted), and gone from its slot.
        assert!(!beads.join("dolt").exists(), "dolt/ moved away");
        assert!(
            beads.join("dolt.bak/data").exists(),
            "dolt preserved as backup"
        );
        // The full store is now beads.db (the built one, not the empty stub).
        assert_eq!(
            std::fs::read(beads.join("beads.db")).unwrap(),
            b"MIGRATED-SQLITE"
        );
        assert!(!built.exists(), "built .new consumed by the rename");
    }

    #[test]
    fn swap_refuses_to_clobber_existing_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let beads = tmp.path().join(".beads");
        std::fs::create_dir_all(beads.join("dolt")).unwrap();
        std::fs::create_dir_all(beads.join("dolt.bak")).unwrap(); // prior backup
        let built = beads.join("beads.db.new");
        std::fs::write(&built, b"x").unwrap();
        let err = swap_dolt_to_sqlite(&beads, &built).unwrap_err();
        assert!(err.to_string().contains("dolt.bak"), "must refuse: {err}");
        assert!(
            beads.join("dolt").is_dir(),
            "source left untouched on refusal"
        );
    }

    #[test]
    fn flip_metadata_declares_sqlite() {
        let tmp = tempfile::tempdir().unwrap();
        let beads = tmp.path().join(".beads");
        std::fs::create_dir_all(&beads).unwrap();
        std::fs::write(
            beads.join("metadata.json"),
            r#"{"backend":"dolt","dolt_mode":"server","dolt_database":"rosary","keep":"me"}"#,
        )
        .unwrap();
        flip_metadata_to_sqlite(&beads).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(beads.join("metadata.json")).unwrap())
                .unwrap();
        assert_eq!(v["backend"], "sqlite");
        assert!(v.get("dolt_mode").is_none(), "dolt_mode dropped");
        assert_eq!(v["keep"], "me", "unrelated keys preserved");
    }

    fn seed_full(id: &str, acceptance: &str) -> NewBead {
        NewBead {
            id: id.into(),
            title: "main".into(),
            description: "body".into(),
            priority: 1,
            issue_type: "bug".into(),
            owner: "dev-agent".into(),
            files: vec!["src/a.rs".into()],
            test_files: vec!["tests/a.rs".into()],
            depends_on: vec![],
            created_by: Some("alice".into()),
            scope: "auth".into(),
            derived_from: vec![],
            acceptance_criteria: acceptance.into(),
        }
    }

    #[tokio::test]
    async fn migrate_preserves_content_status_deps_and_comments() {
        let src = store();
        src.create_bead("dep-1", "blocker", "", 2, "task")
            .await
            .unwrap();
        src.create_bead_full(seed_full("b-1", "cargo test green"))
            .await
            .unwrap();
        src.restore_status("b-1", "blocked").await.unwrap();
        src.set_external_ref("b-1", "kiln:x").await.unwrap();
        src.add_dependency("b-1", "dep-1").await.unwrap();
        src.add_comment("b-1", "a migration note", "bob")
            .await
            .unwrap();

        let tgt = store();
        let report = migrate_store(&src, &tgt, "repo").await.unwrap();
        assert_eq!(
            report,
            MigrationReport {
                beads: 2,
                dependencies: 1,
                cross_repo_dependencies: 0,
                comments: 1,
                // b-1 carries a close condition; dep-1 does not.
                beads_with_acceptance: 1,
            }
        );

        // The load-bearing gate: field-level verify passes.
        verify_migration(&src, &tgt, "repo").await.unwrap();

        // Spot-check the fields a naive migration would drop.
        let b = tgt.get_bead("b-1", "repo").await.unwrap().unwrap();
        assert_eq!(b.status, "blocked", "status restored, not reset to open");
        assert_eq!(b.acceptance_criteria, "cargo test green");
        assert_eq!(b.external_ref.as_deref(), Some("kiln:x"));
        assert_eq!(b.scope, "auth");
        assert_eq!(b.owner.as_deref(), Some("dev-agent"));
        assert_eq!(
            tgt.get_dependencies("b-1").await.unwrap(),
            vec!["dep-1".to_string()]
        );
        assert_eq!(tgt.list_comments("b-1", false).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn migrate_preserves_cross_repo_dependency() {
        // The dry-run against the live store found rosary beads blocking on
        // mache beads — a dangling (cross-repo) depends_on_id that
        // add_dependency's existence check rejects. The migration must preserve
        // it verbatim (restore_dependency).
        let src = store();
        src.create_bead("x", "t", "", 2, "task").await.unwrap();
        src.restore_dependency("x", "mache-cbf644").await.unwrap();

        let tgt = store();
        let report = migrate_store(&src, &tgt, "repo").await.unwrap();
        assert_eq!(report.dependencies, 1, "cross-repo edge is copied");
        verify_migration(&src, &tgt, "repo").await.unwrap();
        assert_eq!(
            tgt.get_dependencies("x").await.unwrap(),
            vec!["mache-cbf644".to_string()],
            "cross-repo edge preserved verbatim"
        );
    }

    #[tokio::test]
    async fn migrate_preserves_created_and_updated_timestamps() {
        let src = store();
        src.create_bead("timestamped", "title", "", 2, "task")
            .await
            .unwrap();
        src.restore_timestamps(
            "timestamped",
            "2024-01-02T03:04:05Z".parse().unwrap(),
            "2025-06-07T08:09:10Z".parse().unwrap(),
        )
        .await
        .unwrap();

        let tgt = store();
        migrate_store(&src, &tgt, "repo").await.unwrap();

        let migrated = tgt.get_bead("timestamped", "repo").await.unwrap().unwrap();
        assert_eq!(
            migrated.created_at.to_rfc3339(),
            "2024-01-02T03:04:05+00:00"
        );
        assert_eq!(
            migrated.updated_at.to_rfc3339(),
            "2025-06-07T08:09:10+00:00"
        );
    }

    #[tokio::test]
    async fn verify_rejects_timestamp_drift() {
        let src = store();
        src.create_bead("timestamped", "title", "", 2, "task")
            .await
            .unwrap();
        src.restore_timestamps(
            "timestamped",
            "2024-01-02T03:04:05Z".parse().unwrap(),
            "2025-06-07T08:09:10Z".parse().unwrap(),
        )
        .await
        .unwrap();

        let tgt = store();
        tgt.create_bead("timestamped", "title", "", 2, "task")
            .await
            .unwrap();

        let err = verify_migration(&src, &tgt, "repo").await.unwrap_err();
        assert!(
            err.to_string().contains("timestamp"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn verify_catches_a_silently_reset_field() {
        // The bug this migration must never ship: a target where status/acceptance
        // silently reset to defaults must FAIL verify, not pass a count check.
        let src = store();
        src.create_bead_full(seed_full("x", "done when X"))
            .await
            .unwrap();
        src.restore_status("x", "blocked").await.unwrap();

        let tgt = store();
        // Simulate a lossy migration: create the bead but skip the status/field
        // restore (status stays 'open', acceptance stays '').
        tgt.create_bead("x", "main", "body", 1, "bug")
            .await
            .unwrap();

        let err = verify_migration(&src, &tgt, "repo").await.unwrap_err();
        assert!(
            err.to_string().contains("mismatch"),
            "verify must catch the silent reset, got: {err}"
        );
    }
}
