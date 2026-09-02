use anyhow::{Context, Result};
use sqlx_core::query::query;
use sqlx_core::row::Row;

use super::DoltClient;

/// Which columns the live `dependencies` table actually has. rosary's own
/// minimal schema is `(issue_id, depends_on_id, dep_type)`; a bd-created (richer)
/// store instead has bd's `type` column plus a NOT-NULL `created_by` with no
/// default — and, after rosary migration 007, *also* a `dep_type` column. So the
/// dep ops must probe the schema and (a) write/read the canonical type column
/// (`type` when present, else `dep_type`) and (b) supply `created_by` when the
/// store requires it. Probed per call (dep ops are infrequent) — rosary-dc0a9f.
struct DepSchema {
    /// The dependency-type column to read/write: `type` (bd) or `dep_type`.
    type_col: &'static str,
    /// `created_by` exists and is NOT NULL with no default → must be supplied.
    needs_created_by: bool,
}

impl DoltClient {
    async fn dep_schema(&self) -> Result<DepSchema> {
        let rows = query(
            "SELECT COLUMN_NAME, IS_NULLABLE, COLUMN_DEFAULT
             FROM INFORMATION_SCHEMA.COLUMNS
             WHERE TABLE_NAME = 'dependencies'",
        )
        .fetch_all(&self.pool)
        .await
        .context("probing dependencies schema")?;
        let (mut has_type, mut needs_created_by) = (false, false);
        for r in &rows {
            let name: String = r.try_get("COLUMN_NAME").unwrap_or_default();
            match name.as_str() {
                // bd's canonical dep-type column.
                "type" => has_type = true,
                "created_by" => {
                    let nullable: String = r.try_get("IS_NULLABLE").unwrap_or_default();
                    let default: Option<String> = r
                        .try_get::<Option<String>, _>("COLUMN_DEFAULT")
                        .ok()
                        .flatten();
                    needs_created_by = nullable.eq_ignore_ascii_case("NO") && default.is_none();
                }
                _ => {}
            }
        }
        // Prefer bd's `type` where it exists so rosary interoperates with the
        // data bd already wrote; fall back to rosary's own `dep_type`.
        let type_col = if has_type { "type" } else { "dep_type" };
        Ok(DepSchema {
            type_col,
            needs_created_by,
        })
    }

    /// Add a dependency: `issue_id` depends on `depends_on_id` (defaults to a
    /// `blocks` edge). Prefer `add_dependency_typed` for containment edges.
    pub async fn add_dependency(&self, issue_id: &str, depends_on_id: &str) -> Result<()> {
        self.add_dependency_typed(issue_id, depends_on_id, "blocks")
            .await
    }

    /// Add a typed dependency edge. `dep_type` is one of
    /// `blocks` | `related` | `parent-child` | `discovered-from`.
    /// Upserts the type when the (issue, depends_on) pair already exists so a
    /// re-link can promote a plain blocks edge to a containment edge.
    pub async fn add_dependency_typed(
        &self,
        issue_id: &str,
        depends_on_id: &str,
        dep_type: &str,
    ) -> Result<()> {
        let full_issue = self.resolve_id(issue_id).await?;
        let full_dep = self.resolve_id(depends_on_id).await?;
        let sch = self.dep_schema().await?;
        let col = sch.type_col; // safe: hardcoded "type" | "dep_type", not user input
        // Rebind the value on conflict (portable) rather than the deprecated
        // VALUES() function. Supply created_by only when the store requires it.
        let (sql, with_creator) = if sch.needs_created_by {
            (
                format!(
                    "INSERT INTO dependencies (issue_id, depends_on_id, `{col}`, created_by) \
                     VALUES (?, ?, ?, ?) ON DUPLICATE KEY UPDATE `{col}` = ?"
                ),
                true,
            )
        } else {
            (
                format!(
                    "INSERT INTO dependencies (issue_id, depends_on_id, `{col}`) \
                     VALUES (?, ?, ?) ON DUPLICATE KEY UPDATE `{col}` = ?"
                ),
                false,
            )
        };
        let mut q = query(&sql).bind(&full_issue).bind(&full_dep).bind(dep_type);
        if with_creator {
            q = q.bind("rosary");
        }
        q = q.bind(dep_type); // ON DUPLICATE KEY UPDATE value
        q.execute(&self.pool).await.with_context(|| {
            format!("adding {dep_type} dependency {issue_id} → {depends_on_id}")
        })?;
        self.auto_commit(&format!("dep {full_issue} → {full_dep} ({dep_type})"))
            .await;
        Ok(())
    }

    /// List the containment children of a bead — beads linked to it by a
    /// `parent-child` or `discovered-from` edge (the edge points from child to
    /// parent, so children are the dependents filtered to those types). Used by
    /// the close-merged containment gate so a parent doesn't auto-close while
    /// its children are still open.
    pub async fn get_children(&self, issue_id: &str) -> Result<Vec<String>> {
        let full_id = match self.resolve_id(issue_id).await {
            Ok(id) => id,
            Err(_) => return Ok(vec![]),
        };
        let col = self.dep_schema().await?.type_col;
        let rows = query(&format!(
            "SELECT issue_id FROM dependencies
             WHERE depends_on_id = ? AND `{col}` IN ('parent-child', 'discovered-from')"
        ))
        .bind(&full_id)
        .fetch_all(&self.pool)
        .await
        .with_context(|| format!("listing children for {issue_id}"))?;
        Ok(rows
            .iter()
            .map(|r| r.get::<String, _>("issue_id"))
            .collect())
    }

    /// Remove a dependency.
    pub async fn remove_dependency(&self, issue_id: &str, depends_on_id: &str) -> Result<()> {
        let full_issue = self.resolve_id(issue_id).await?;
        let full_dep = self.resolve_id(depends_on_id).await?;
        query("DELETE FROM dependencies WHERE issue_id = ? AND depends_on_id = ?")
            .bind(&full_issue)
            .bind(&full_dep)
            .execute(&self.pool)
            .await
            .with_context(|| format!("removing dependency {issue_id} → {depends_on_id}"))?;
        self.auto_commit(&format!("undep {full_issue} → {full_dep}"))
            .await;
        Ok(())
    }

    /// List dependencies of a bead (what it depends ON).
    #[allow(dead_code)] // API surface — used by future MCP tools and reconciler dep checks
    pub async fn get_dependencies(&self, issue_id: &str) -> Result<Vec<String>> {
        let full_id = match self.resolve_id(issue_id).await {
            Ok(id) => id,
            Err(_) => return Ok(vec![]),
        };
        let rows = query("SELECT depends_on_id FROM dependencies WHERE issue_id = ?")
            .bind(&full_id)
            .fetch_all(&self.pool)
            .await
            .with_context(|| format!("listing dependencies for {issue_id}"))?;
        Ok(rows
            .iter()
            .map(|r| r.get::<String, _>("depends_on_id"))
            .collect())
    }

    /// List dependents of a bead (what depends on IT).
    #[allow(dead_code)] // API surface — used by future MCP tools and reconciler dep checks
    pub async fn get_dependents(&self, issue_id: &str) -> Result<Vec<String>> {
        let full_id = match self.resolve_id(issue_id).await {
            Ok(id) => id,
            Err(_) => return Ok(vec![]),
        };
        let rows = query("SELECT issue_id FROM dependencies WHERE depends_on_id = ?")
            .bind(&full_id)
            .fetch_all(&self.pool)
            .await
            .with_context(|| format!("listing dependents for {issue_id}"))?;
        Ok(rows
            .iter()
            .map(|r| r.get::<String, _>("issue_id"))
            .collect())
    }

    /// Add a comment to an issue.
    ///
    /// Generates the comment `id` in Rust (UUID v4) and supplies it explicitly
    /// at INSERT time. Migration 005 (rosary-a96b06) makes `comments.id` a
    /// `CHAR(36) NOT NULL PRIMARY KEY` with no SQL-side default — the previous
    /// `DEFAULT (uuid())` was unreliable across Dolt versions and was removed in
    /// rosary-5ad4b0. The Rust-side generation keeps INSERT paths working after
    /// the migration ships.
    pub async fn add_comment(&self, issue_id: &str, body: &str, author: &str) -> Result<()> {
        let full_id = self.resolve_id(issue_id).await?;
        let body = crate::secrets::scrub_and_warn(body, &format!("comment on {issue_id}"));
        let comment_id = uuid::Uuid::new_v4().to_string();
        query("INSERT INTO comments (id, issue_id, text, author, created_at) VALUES (?, ?, ?, ?, NOW())")
            .bind(&comment_id)
            .bind(&full_id)
            .bind(&body)
            .bind(author)
            .execute(&self.pool)
            .await
            .with_context(|| format!("adding comment to {issue_id}"))?;
        self.auto_commit(&format!("comment on {full_id}")).await;
        Ok(())
    }

    /// List comments for an issue. Soft-deleted comments are omitted unless
    /// `include_deleted` is true. Returns oldest-first.
    pub async fn list_comments(
        &self,
        issue_id: &str,
        include_deleted: bool,
    ) -> Result<Vec<crate::bead::Comment>> {
        use sqlx_core::row::Row;
        let full_id = self.resolve_id(issue_id).await?;
        let sql = if include_deleted {
            "SELECT id, issue_id, text, author, created_at, edited_at, edit_reason,
                    original_text, deleted_at, delete_reason
             FROM comments WHERE issue_id = ? ORDER BY created_at ASC, id ASC"
        } else {
            "SELECT id, issue_id, text, author, created_at, edited_at, edit_reason,
                    original_text, deleted_at, delete_reason
             FROM comments WHERE issue_id = ? AND deleted_at IS NULL
             ORDER BY created_at ASC, id ASC"
        };
        let rows = query(sql)
            .bind(&full_id)
            .fetch_all(&self.pool)
            .await
            .with_context(|| format!("listing comments for {issue_id}"))?;
        Ok(rows
            .iter()
            .map(|r| crate::bead::Comment {
                id: r.try_get::<String, _>("id").unwrap_or_default(),
                issue_id: r.try_get("issue_id").unwrap_or_default(),
                text: r.try_get("text").unwrap_or_default(),
                author: r.try_get("author").unwrap_or_default(),
                created_at: r
                    .try_get("created_at")
                    .unwrap_or_else(|_| chrono::Utc::now()),
                edited_at: r.try_get("edited_at").ok(),
                edit_reason: r.try_get::<Option<String>, _>("edit_reason").ok().flatten(),
                original_text: r
                    .try_get::<Option<String>, _>("original_text")
                    .ok()
                    .flatten(),
                deleted_at: r.try_get("deleted_at").ok(),
                delete_reason: r
                    .try_get::<Option<String>, _>("delete_reason")
                    .ok()
                    .flatten(),
            })
            .collect())
    }

    /// Update a comment's body. Captures `original_text` on first edit
    /// (immutable thereafter); records `edited_at` and `edit_reason`.
    /// Returns the updated comment. Errors if `comment_id` does not exist.
    pub async fn update_comment(
        &self,
        comment_id: &str,
        body: &str,
        reason: Option<&str>,
    ) -> Result<crate::bead::Comment> {
        use sqlx_core::row::Row;
        // Fetch current state to determine whether to capture original_text.
        let existing = query(
            "SELECT id, issue_id, text, author, created_at, edited_at, edit_reason,
                    original_text, deleted_at, delete_reason
             FROM comments WHERE id = ?",
        )
        .bind(comment_id)
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("looking up comment {comment_id}"))?
        .ok_or_else(|| anyhow::anyhow!("comment {comment_id} not found"))?;

        let issue_id: String = existing.try_get("issue_id").unwrap_or_default();

        let scrubbed =
            crate::secrets::scrub_and_warn(body, &format!("comment update {comment_id}"));

        // First-edit captures original_text, atomically IN the update itself:
        // SET clauses evaluate left-to-right (MySQL semantics), so
        // `COALESCE(original_text, text)` reads the pre-edit text before the
        // `text = ?` assignment lands. The previous read-then-conditional-write
        // let two concurrent first edits both observe original_text as NULL and
        // the second clobber the true original with already-edited text
        // (rosary-44eec8 gap 6).
        query(
            "UPDATE comments
             SET original_text = COALESCE(original_text, text),
                 text = ?, edited_at = NOW(), edit_reason = ?
             WHERE id = ?",
        )
        .bind(&scrubbed)
        .bind(reason)
        .bind(comment_id)
        .execute(&self.pool)
        .await
        .with_context(|| format!("updating comment {comment_id}"))?;

        self.auto_commit(&format!("comment update {comment_id} on {issue_id}"))
            .await;

        // Return the post-update row.
        let updated = query(
            "SELECT id, issue_id, text, author, created_at, edited_at, edit_reason,
                    original_text, deleted_at, delete_reason
             FROM comments WHERE id = ?",
        )
        .bind(comment_id)
        .fetch_one(&self.pool)
        .await
        .with_context(|| format!("re-fetching comment {comment_id}"))?;

        Ok(crate::bead::Comment {
            id: updated
                .try_get::<String, _>("id")
                .unwrap_or_else(|_| comment_id.to_string()),
            issue_id: updated.try_get("issue_id").unwrap_or_default(),
            text: updated.try_get("text").unwrap_or_default(),
            author: updated.try_get("author").unwrap_or_default(),
            created_at: updated
                .try_get("created_at")
                .unwrap_or_else(|_| chrono::Utc::now()),
            edited_at: updated.try_get("edited_at").ok(),
            edit_reason: updated
                .try_get::<Option<String>, _>("edit_reason")
                .ok()
                .flatten(),
            original_text: updated
                .try_get::<Option<String>, _>("original_text")
                .ok()
                .flatten(),
            deleted_at: updated.try_get("deleted_at").ok(),
            delete_reason: updated
                .try_get::<Option<String>, _>("delete_reason")
                .ok()
                .flatten(),
        })
    }

    /// Soft-delete a comment. Idempotent (re-deleting refreshes timestamp).
    /// Errors if the comment does not exist.
    pub async fn delete_comment(&self, comment_id: &str, reason: Option<&str>) -> Result<()> {
        // Confirm existence first so we can report a clean error.
        let exists: i64 = query("SELECT COUNT(*) AS cnt FROM comments WHERE id = ?")
            .bind(comment_id)
            .fetch_one(&self.pool)
            .await
            .with_context(|| format!("looking up comment {comment_id}"))?
            .try_get("cnt")
            .unwrap_or(0);
        if exists == 0 {
            anyhow::bail!("comment {comment_id} not found");
        }

        // The deletion reason lands in `delete_reason` — its own column —
        // so it's preserved even when the comment was previously edited
        // (which would have populated `edit_reason`). Re-deletion overwrites
        // an earlier deletion's reason; the timestamp is also refreshed.
        query(
            "UPDATE comments
             SET deleted_at = NOW(), delete_reason = ?
             WHERE id = ?",
        )
        .bind(reason)
        .bind(comment_id)
        .execute(&self.pool)
        .await
        .with_context(|| format!("soft-deleting comment {comment_id}"))?;
        self.auto_commit(&format!("comment soft-delete {comment_id}"))
            .await;
        Ok(())
    }

    /// Hard-delete a comment — physically removes the row. CLI-only;
    /// destroys audit trail. Errors if the comment does not exist.
    pub async fn hard_delete_comment(&self, comment_id: &str) -> Result<()> {
        let result = query("DELETE FROM comments WHERE id = ?")
            .bind(comment_id)
            .execute(&self.pool)
            .await
            .with_context(|| format!("hard-deleting comment {comment_id}"))?;
        if result.rows_affected() == 0 {
            anyhow::bail!("comment {comment_id} not found");
        }
        self.auto_commit(&format!("comment hard-delete {comment_id}"))
            .await;
        Ok(())
    }
}
