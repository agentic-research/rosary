use anyhow::{Context, Result};
use sqlx_core::query::query;
use sqlx_core::row::Row;

use super::DoltClient;

impl DoltClient {
    /// Add a dependency: `issue_id` depends on `depends_on_id`.
    pub async fn add_dependency(&self, issue_id: &str, depends_on_id: &str) -> Result<()> {
        let full_issue = self.resolve_id(issue_id).await?;
        let full_dep = self.resolve_id(depends_on_id).await?;
        query("INSERT IGNORE INTO dependencies (issue_id, depends_on_id) VALUES (?, ?)")
            .bind(&full_issue)
            .bind(&full_dep)
            .execute(&self.pool)
            .await
            .with_context(|| format!("adding dependency {issue_id} → {depends_on_id}"))?;
        self.auto_commit(&format!("dep {full_issue} → {full_dep}"))
            .await;
        Ok(())
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
    pub async fn add_comment(&self, issue_id: &str, body: &str, author: &str) -> Result<()> {
        let full_id = self.resolve_id(issue_id).await?;
        let body = crate::secrets::scrub_and_warn(body, &format!("comment on {issue_id}"));
        query("INSERT INTO comments (issue_id, text, author, created_at) VALUES (?, ?, ?, NOW())")
            .bind(&full_id)
            .bind(&body)
            .bind(author)
            .execute(&self.pool)
            .await
            .with_context(|| format!("adding comment to {issue_id}"))?;
        self.auto_commit(&format!("comment on {full_id}")).await;
        Ok(())
    }
}
