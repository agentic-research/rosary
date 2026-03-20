//! Dependency and comment management for beads.

use anyhow::{Context, Result};
use sqlx_core::query::query;
use sqlx_core::row::Row;

use super::DoltClient;

impl DoltClient {
    /// Add a dependency: `issue_id` depends on `depends_on_id`.
    pub async fn add_dependency(&self, issue_id: &str, depends_on_id: &str) -> Result<()> {
        query("INSERT IGNORE INTO dependencies (issue_id, depends_on_id) VALUES (?, ?)")
            .bind(issue_id)
            .bind(depends_on_id)
            .execute(&self.pool)
            .await
            .with_context(|| format!("adding dependency {issue_id} → {depends_on_id}"))?;
        self.auto_commit(&format!("dep {issue_id} → {depends_on_id}"))
            .await;
        Ok(())
    }

    /// Remove a dependency.
    pub async fn remove_dependency(&self, issue_id: &str, depends_on_id: &str) -> Result<()> {
        query("DELETE FROM dependencies WHERE issue_id = ? AND depends_on_id = ?")
            .bind(issue_id)
            .bind(depends_on_id)
            .execute(&self.pool)
            .await
            .with_context(|| format!("removing dependency {issue_id} → {depends_on_id}"))?;
        self.auto_commit(&format!("undep {issue_id} → {depends_on_id}"))
            .await;
        Ok(())
    }

    /// List dependencies of a bead (what it depends ON).
    #[allow(dead_code)] // API surface — used by future MCP tools and reconciler dep checks
    pub async fn get_dependencies(&self, issue_id: &str) -> Result<Vec<String>> {
        let rows = query("SELECT depends_on_id FROM dependencies WHERE issue_id = ?")
            .bind(issue_id)
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
        let rows = query("SELECT issue_id FROM dependencies WHERE depends_on_id = ?")
            .bind(issue_id)
            .fetch_all(&self.pool)
            .await
            .with_context(|| format!("listing dependents for {issue_id}"))?;
        Ok(rows
            .iter()
            .map(|r| r.get::<String, _>("issue_id"))
            .collect())
    }

    /// Add a comment to an issue.
    pub async fn add_comment(&self, issue_id: &str, body: &str, author: &str) -> Result<()> {
        query("INSERT INTO comments (issue_id, text, author, created_at) VALUES (?, ?, ?, NOW())")
            .bind(issue_id)
            .bind(body)
            .bind(author)
            .execute(&self.pool)
            .await
            .with_context(|| format!("adding comment to {issue_id}"))?;
        self.auto_commit(&format!("comment on {issue_id}")).await;
        Ok(())
    }
}
