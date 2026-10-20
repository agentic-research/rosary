//! Verbatim-write primitives for RESTORE and MIGRATION contexts — kept out
//! of the store's normal write path so the bypasses stay visible.

use anyhow::{Context, Result};
use rusqlite::params;

use super::SqliteBeadStore;

impl SqliteBeadStore {
    /// Insert a dependency edge **verbatim**, without checking that
    /// `depends_on_id` resolves to a local bead. For RESTORE contexts only —
    /// migration must preserve **cross-repo** edges (a rosary bead blocking on
    /// a mache bead), which the target legitimately holds as a dangling
    /// `depends_on_id` (the table has no FK), but which `add_dependency`'s
    /// existence check rejects. Inherent, so the bypass stays out of normal
    /// code paths.
    #[cfg(test)] // production restores are typed (`restore_dependency_typed`)
    pub(crate) async fn restore_dependency(
        &self,
        issue_id: &str,
        depends_on_id: &str,
    ) -> Result<()> {
        self.restore_dependency_typed(
            issue_id,
            depends_on_id,
            crate::contract_deps::DEFAULT_DEP_TYPE,
        )
        .await
    }

    /// [`Self::restore_dependency`] with the edge's type (rosary-664e25).
    pub(crate) async fn restore_dependency_typed(
        &self,
        issue_id: &str,
        depends_on_id: &str,
        dep_type: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO dependencies (issue_id, depends_on_id, dep_type) VALUES (?1, ?2, ?3)
             ON CONFLICT(issue_id, depends_on_id) DO UPDATE SET dep_type = excluded.dep_type",
            params![issue_id, depends_on_id, dep_type],
        )
        .with_context(|| format!("restoring dependency {issue_id} -> {depends_on_id}"))?;
        Ok(())
    }

    /// Make `issue_id`'s outgoing edge set EXACTLY `deps` — the restore of an
    /// incoming record that won last-writer-wins (rosary-664e25). An
    /// insert-only restore can't propagate a removed edge or a changed type.
    /// Verbatim like [`Self::restore_dependency`] (cross-repo targets allowed).
    pub(crate) async fn replace_dependencies(
        &self,
        issue_id: &str,
        deps: &[(String, String)],
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM dependencies WHERE issue_id = ?1",
            params![issue_id],
        )?;
        for (dep, dep_type) in deps {
            tx.execute(
                "INSERT INTO dependencies (issue_id, depends_on_id, dep_type) VALUES (?1, ?2, ?3)",
                params![issue_id, dep, dep_type],
            )?;
        }
        tx.commit()
            .with_context(|| format!("replacing dependencies of {issue_id}"))?;
        Ok(())
    }
}
