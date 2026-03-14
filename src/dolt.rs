//! Native MySQL client for Dolt-backed beads databases.
//!
//! Reads connection info from `.beads/dolt-server.port` and `.beads/metadata.json`,
//! then queries the Dolt server directly over MySQL wire protocol via sqlx.

use anyhow::{Context, Result};
use sqlx_core::query::query;
use sqlx_core::row::Row;
use sqlx_mysql::MySqlPool;
use std::path::Path;

use crate::bead::Bead;

/// Connection details for a Dolt beads server.
#[derive(Debug, Clone)]
pub struct DoltConfig {
    pub host: String,
    pub port: u16,
    pub database: String,
}

impl DoltConfig {
    /// Discover connection details from a repo's `.beads/` directory.
    pub fn from_beads_dir(beads_dir: &Path) -> Result<Self> {
        let port_file = beads_dir.join("dolt-server.port");
        let port_str = std::fs::read_to_string(&port_file)
            .with_context(|| format!("reading {}", port_file.display()))?;
        let port: u16 = port_str
            .trim()
            .parse()
            .with_context(|| format!("parsing port from {}", port_file.display()))?;

        let meta_file = beads_dir.join("metadata.json");
        let database = if meta_file.exists() {
            let meta_str = std::fs::read_to_string(&meta_file)
                .with_context(|| format!("reading {}", meta_file.display()))?;
            let meta: serde_json::Value = serde_json::from_str(&meta_str)?;
            meta["dolt_database"]
                .as_str()
                .or_else(|| meta["database"].as_str())
                .unwrap_or("beads")
                .to_string()
        } else {
            "beads".to_string()
        };

        Ok(DoltConfig {
            host: "127.0.0.1".to_string(),
            port,
            database,
        })
    }

    /// Build a MySQL connection URL.
    pub fn url(&self) -> String {
        format!("mysql://root@{}:{}/{}", self.host, self.port, self.database)
    }
}

/// Client for querying beads from a Dolt server.
pub struct DoltClient {
    pool: MySqlPool,
}

impl DoltClient {
    /// Connect to a Dolt server.
    pub async fn connect(config: &DoltConfig) -> Result<Self> {
        let pool = MySqlPool::connect(&config.url())
            .await
            .with_context(|| format!("connecting to Dolt at {}", config.url()))?;
        Ok(DoltClient { pool })
    }

    /// List all open issues as Beads.
    pub async fn list_beads(&self, repo_name: &str) -> Result<Vec<Bead>> {
        let rows = query(
            r#"SELECT id, title, description, status, priority, issue_type,
                      assignee, created_at, updated_at,
                      (SELECT COUNT(*) FROM dependencies d WHERE d.depends_on_id = i.id) as dep_count,
                      (SELECT COUNT(*) FROM dependencies d WHERE d.issue_id = i.id) as dependency_count,
                      (SELECT COUNT(*) FROM comments c WHERE c.issue_id = i.id) as comment_count
               FROM issues i
               WHERE status != 'closed'
               ORDER BY priority ASC, created_at DESC"#,
        )
        .fetch_all(&self.pool)
        .await
        .context("querying issues")?;

        let beads = rows
            .iter()
            .map(|row| Bead {
                id: row.get("id"),
                title: row.get("title"),
                description: row.try_get("description").unwrap_or_default(),
                status: row.get("status"),
                priority: row.try_get::<i32, _>("priority").unwrap_or(2) as u8,
                issue_type: row
                    .try_get("issue_type")
                    .unwrap_or_else(|_| "task".to_string()),
                owner: row.try_get("assignee").ok(),
                repo: repo_name.to_string(),
                created_at: row.try_get("created_at").unwrap_or_default(),
                updated_at: row.try_get("updated_at").unwrap_or_default(),
                dependency_count: row.try_get::<i64, _>("dependency_count").unwrap_or(0) as u32,
                dependent_count: row.try_get::<i64, _>("dep_count").unwrap_or(0) as u32,
                comment_count: row.try_get::<i64, _>("comment_count").unwrap_or(0) as u32,
                branch: None,
                pr_url: None,
                jj_change_id: None,
            })
            .collect();

        Ok(beads)
    }

    /// Get a single bead by ID.
    pub async fn get_bead(&self, id: &str, repo_name: &str) -> Result<Option<Bead>> {
        let row = query(
            r#"SELECT id, title, description, status, priority, issue_type,
                      assignee, created_at, updated_at,
                      (SELECT COUNT(*) FROM dependencies d WHERE d.depends_on_id = i.id) as dep_count,
                      (SELECT COUNT(*) FROM dependencies d WHERE d.issue_id = i.id) as dependency_count,
                      (SELECT COUNT(*) FROM comments c WHERE c.issue_id = i.id) as comment_count
               FROM issues i
               WHERE id = ?"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("querying issue {id}"))?;

        Ok(row.map(|row| Bead {
            id: row.get("id"),
            title: row.get("title"),
            description: row.try_get("description").unwrap_or_default(),
            status: row.get("status"),
            priority: row.try_get::<i32, _>("priority").unwrap_or(2) as u8,
            issue_type: row
                .try_get("issue_type")
                .unwrap_or_else(|_| "task".to_string()),
            owner: row.try_get("assignee").ok(),
            repo: repo_name.to_string(),
            created_at: row.try_get("created_at").unwrap_or_default(),
            updated_at: row.try_get("updated_at").unwrap_or_default(),
            dependency_count: row.try_get::<i64, _>("dependency_count").unwrap_or(0) as u32,
            dependent_count: row.try_get::<i64, _>("dep_count").unwrap_or(0) as u32,
            comment_count: row.try_get::<i64, _>("comment_count").unwrap_or(0) as u32,
            branch: None,
            pr_url: None,
            jj_change_id: None,
        }))
    }

    /// Update a bead's status.
    pub async fn update_status(&self, id: &str, status: &str) -> Result<()> {
        query("UPDATE issues SET status = ?, updated_at = NOW() WHERE id = ?")
            .bind(status)
            .bind(id)
            .execute(&self.pool)
            .await
            .with_context(|| format!("updating status for {id}"))?;
        Ok(())
    }

    /// Create a new bead (issue) in the database.
    pub async fn create_bead(
        &self,
        id: &str,
        title: &str,
        description: &str,
        priority: u8,
        issue_type: &str,
    ) -> Result<()> {
        query(
            r#"INSERT INTO issues (id, title, description, design, acceptance_criteria, notes, status, priority, issue_type, created_at, updated_at)
               VALUES (?, ?, ?, '', '', '', 'open', ?, ?, NOW(), NOW())"#,
        )
        .bind(id)
        .bind(title)
        .bind(description)
        .bind(priority as i32)
        .bind(issue_type)
        .execute(&self.pool)
        .await
        .with_context(|| format!("creating bead {id}"))?;
        Ok(())
    }

    /// Close a bead by setting its status to 'closed'.
    pub async fn close_bead(&self, id: &str) -> Result<()> {
        query("UPDATE issues SET status = 'closed', updated_at = NOW() WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .with_context(|| format!("closing bead {id}"))?;
        Ok(())
    }

    /// Add a comment to an issue.
    pub async fn add_comment(&self, issue_id: &str, body: &str, author: &str) -> Result<()> {
        query("INSERT INTO comments (issue_id, body, author, created_at) VALUES (?, ?, ?, NOW())")
            .bind(issue_id)
            .bind(body)
            .bind(author)
            .execute(&self.pool)
            .await
            .with_context(|| format!("adding comment to {issue_id}"))?;
        Ok(())
    }

    #[allow(dead_code)] // API surface for rsry bead search
    /// Search beads by title or description substring match.
    pub async fn search_beads(&self, query_str: &str, repo_name: &str) -> Result<Vec<Bead>> {
        let pattern = format!("%{query_str}%");
        let rows = query(
            r#"SELECT id, title, description, status, priority, issue_type,
                      assignee, created_at, updated_at,
                      (SELECT COUNT(*) FROM dependencies d WHERE d.depends_on_id = i.id) as dep_count,
                      (SELECT COUNT(*) FROM dependencies d WHERE d.issue_id = i.id) as dependency_count,
                      (SELECT COUNT(*) FROM comments c WHERE c.issue_id = i.id) as comment_count
               FROM issues i
               WHERE title LIKE ? OR description LIKE ?
               ORDER BY priority ASC, created_at DESC"#,
        )
        .bind(&pattern)
        .bind(&pattern)
        .fetch_all(&self.pool)
        .await
        .with_context(|| format!("searching beads for '{query_str}'"))?;

        let beads = rows
            .iter()
            .map(|row| Bead {
                id: row.get("id"),
                title: row.get("title"),
                description: row.try_get("description").unwrap_or_default(),
                status: row.get("status"),
                priority: row.try_get::<i32, _>("priority").unwrap_or(2) as u8,
                issue_type: row
                    .try_get("issue_type")
                    .unwrap_or_else(|_| "task".to_string()),
                owner: row.try_get("assignee").ok(),
                repo: repo_name.to_string(),
                created_at: row.try_get("created_at").unwrap_or_default(),
                updated_at: row.try_get("updated_at").unwrap_or_default(),
                dependency_count: row.try_get::<i64, _>("dependency_count").unwrap_or(0) as u32,
                dependent_count: row.try_get::<i64, _>("dep_count").unwrap_or(0) as u32,
                comment_count: row.try_get::<i64, _>("comment_count").unwrap_or(0) as u32,
                branch: None,
                pr_url: None,
                jj_change_id: None,
            })
            .collect();

        Ok(beads)
    }

    /// Log an event to the events table for audit trail.
    /// Best-effort: logs warning on failure rather than propagating error.
    pub async fn log_event(&self, issue_id: &str, event_type: &str, detail: &str) {
        let result = query(
            "INSERT INTO events (issue_id, event_type, actor, comment, created_at) VALUES (?, ?, 'rosary', ?, NOW())",
        )
        .bind(issue_id)
        .bind(event_type)
        .bind(detail)
        .execute(&self.pool)
        .await;

        if let Err(e) = result {
            eprintln!("warning: failed to log event for {issue_id}: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn parse_dolt_config_from_beads_dir() {
        let dir = TempDir::new().unwrap();
        let beads = dir.path();

        // Write port file
        let mut port_file = std::fs::File::create(beads.join("dolt-server.port")).unwrap();
        write!(port_file, "60621").unwrap();

        // Write metadata
        std::fs::write(
            beads.join("metadata.json"),
            r#"{"dolt_database": "mache", "project_id": "abc-123"}"#,
        )
        .unwrap();

        let config = DoltConfig::from_beads_dir(beads).unwrap();
        assert_eq!(config.host, "127.0.0.1");
        assert_eq!(config.port, 60621);
        assert_eq!(config.database, "mache");
        assert_eq!(config.url(), "mysql://root@127.0.0.1:60621/mache");
    }

    #[test]
    fn parse_dolt_config_missing_metadata_defaults_to_beads() {
        let dir = TempDir::new().unwrap();
        let beads = dir.path();

        std::fs::write(beads.join("dolt-server.port"), "3306").unwrap();
        // No metadata.json

        let config = DoltConfig::from_beads_dir(beads).unwrap();
        assert_eq!(config.database, "beads");
        assert_eq!(config.port, 3306);
    }

    #[test]
    fn parse_dolt_config_no_port_file_errors() {
        let dir = TempDir::new().unwrap();
        let result = DoltConfig::from_beads_dir(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn parse_dolt_config_bad_port_errors() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("dolt-server.port"), "not-a-number").unwrap();
        let result = DoltConfig::from_beads_dir(dir.path());
        assert!(result.is_err());
    }

    /// Integration test — only runs when a real Dolt server is available.
    /// Set RSRY_TEST_BEADS_DIR to a .beads/ directory with a running server.
    #[tokio::test]
    async fn list_beads_from_live_dolt() {
        let beads_dir = match std::env::var("RSRY_TEST_BEADS_DIR") {
            Ok(dir) => dir,
            Err(_) => {
                eprintln!("skipping: RSRY_TEST_BEADS_DIR not set");
                return;
            }
        };

        let config = DoltConfig::from_beads_dir(Path::new(&beads_dir)).unwrap();
        let client = DoltClient::connect(&config).await.unwrap();
        let beads = client.list_beads("test-repo").await.unwrap();

        // Should get at least one bead from a real database
        assert!(!beads.is_empty(), "expected beads from live Dolt server");
        for bead in &beads {
            assert!(!bead.id.is_empty());
            assert!(!bead.title.is_empty());
            assert_eq!(bead.repo, "test-repo");
        }
    }

    #[tokio::test]
    async fn get_single_bead_from_live_dolt() {
        let beads_dir = match std::env::var("RSRY_TEST_BEADS_DIR") {
            Ok(dir) => dir,
            Err(_) => {
                eprintln!("skipping: RSRY_TEST_BEADS_DIR not set");
                return;
            }
        };

        let config = DoltConfig::from_beads_dir(Path::new(&beads_dir)).unwrap();
        let client = DoltClient::connect(&config).await.unwrap();

        // First list to get a known ID
        let beads = client.list_beads("test").await.unwrap();
        if beads.is_empty() {
            eprintln!("skipping: no beads in database");
            return;
        }

        let id = &beads[0].id;
        let bead = client.get_bead(id, "test").await.unwrap();
        assert!(bead.is_some());
        assert_eq!(bead.unwrap().id, *id);
    }

    /// Integration test — creates, searches, comments, and closes a bead.
    /// Only runs when a real Dolt server is available.
    #[tokio::test]
    async fn crud_lifecycle_live_dolt() {
        let beads_dir = match std::env::var("RSRY_TEST_BEADS_DIR") {
            Ok(dir) => dir,
            Err(_) => {
                eprintln!("skipping: RSRY_TEST_BEADS_DIR not set");
                return;
            }
        };

        let config = DoltConfig::from_beads_dir(Path::new(&beads_dir)).unwrap();
        let client = DoltClient::connect(&config).await.unwrap();

        let test_id = format!(
            "test-crud-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        );

        // Create
        client
            .create_bead(
                &test_id,
                "Test CRUD bead",
                "Integration test description",
                2,
                "task",
            )
            .await
            .unwrap();

        // Verify created
        let bead = client.get_bead(&test_id, "test").await.unwrap();
        assert!(bead.is_some(), "bead should exist after creation");
        let bead = bead.unwrap();
        assert_eq!(bead.title, "Test CRUD bead");
        assert_eq!(bead.status, "open");

        // Search
        let results = client.search_beads("CRUD bead", "test").await.unwrap();
        assert!(
            results.iter().any(|b| b.id == test_id),
            "search should find created bead"
        );

        // Add comment
        client
            .add_comment(&test_id, "Test comment body", "test-runner")
            .await
            .unwrap();

        // Close
        client.close_bead(&test_id).await.unwrap();

        // Verify closed
        let bead = client.get_bead(&test_id, "test").await.unwrap();
        assert!(bead.is_some());
        assert_eq!(bead.unwrap().status, "closed");
    }
}
