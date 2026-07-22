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

use anyhow::{Context, Result};

use crate::bead::Bead;
use crate::bead_sqlite::SqliteBeadStore;
use crate::store::{BeadStore, NewBead};

/// Counts from a completed [`migrate_store`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct MigrationReport {
    pub beads: usize,
    pub dependencies: usize,
    pub comments: usize,
}

/// Copy every bead (+ dependency edges + live comments) from `source` into
/// `target`, full-fidelity. Two passes so dependency targets exist before edges
/// reference them. Assumes `target` starts empty. Touches no filesystem and
/// swaps no stores — that is the caller's atomic step, gated on
/// [`verify_migration`].
///
/// Fidelity note: bead *content* (status, priority, type, acceptance,
/// external_ref, scope, files, provenance, owner, deps) is preserved exactly.
/// Not carried, by trait limitation: the comment **audit trail** (exact
/// timestamps, edited/deleted state) — `add_comment` can only reproduce
/// body+author — and bead timestamps, which shift to migration time
/// (`create_bead_full` stamps `now()`). A raw-row copy for those is a follow-up.
pub async fn migrate_store(
    source: &dyn BeadStore,
    target: &SqliteBeadStore,
    repo_name: &str,
) -> Result<MigrationReport> {
    let beads = source
        .list_all_beads(repo_name)
        .await
        .context("reading source beads")?;
    let mut report = MigrationReport::default();

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
        report.beads += 1;
    }

    // Pass 2: dependency edges + live comments (all targets now exist).
    for b in &beads {
        for dep in source.get_dependencies(&b.id).await.unwrap_or_default() {
            target
                .add_dependency(&b.id, &dep)
                .await
                .with_context(|| format!("copying dependency {} -> {dep}", b.id))?;
            report.dependencies += 1;
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
        same!(status);
        same!(priority);
        same!(issue_type);
        same!(owner);
        same!(scope);
        same!(external_ref);
        same!(acceptance_criteria);
        same!(files);
        same!(test_files);
        same!(created_by);

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead_sqlite::SqliteBeadStore;
    use std::path::Path;

    fn store() -> SqliteBeadStore {
        SqliteBeadStore::connect(Path::new(":memory:")).unwrap()
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
                comments: 1
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
