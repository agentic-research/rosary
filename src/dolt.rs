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
    /// Path to the .beads/ directory (for auto-start + state files).
    pub beads_dir: std::path::PathBuf,
}

impl DoltConfig {
    /// Path to the Dolt database directory.
    pub fn dolt_dir(&self) -> std::path::PathBuf {
        self.beads_dir.join("dolt").join(&self.database)
    }

    /// Discover connection details from a repo's `.beads/` directory.
    pub fn from_beads_dir(beads_dir: &Path) -> Result<Self> {
        let port_file = beads_dir.join("dolt-server.port");
        let port: u16 = if port_file.exists() {
            let port_str = std::fs::read_to_string(&port_file)
                .with_context(|| format!("reading {}", port_file.display()))?;
            port_str
                .trim()
                .parse()
                .with_context(|| format!("parsing port from {}", port_file.display()))?
        } else {
            0 // No server running — connect() will auto-start
        };

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
            beads_dir: beads_dir.to_path_buf(),
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
    /// Connect to a Dolt server, auto-starting if not running.
    ///
    /// Follows the same pattern as beads' `EnsureRunning()`:
    /// 1. Try connecting (3s timeout)
    /// 2. If fails, start `dolt sql-server` from the db directory
    /// 3. Wait for it to accept connections
    /// 4. Retry the MySQL connection
    pub async fn connect(config: &DoltConfig) -> Result<Self> {
        // Fast path — server already running
        if let Ok(Ok(pool)) = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            MySqlPool::connect(&config.url()),
        )
        .await
        {
            return Ok(DoltClient { pool });
        }

        // Server not running — auto-start from the dolt data directory
        let dolt_dir = config.dolt_dir();
        if !dolt_dir.exists() {
            anyhow::bail!("Dolt database directory not found: {}", dolt_dir.display());
        }

        eprintln!(
            "[dolt] auto-starting server for {} on port {}...",
            config.database, config.port
        );

        // Allocate ephemeral port if configured port is 0
        let port = if config.port == 0 {
            let listener =
                std::net::TcpListener::bind("127.0.0.1:0").context("allocating ephemeral port")?;
            let port = listener.local_addr()?.port();
            drop(listener);
            port
        } else {
            config.port
        };

        // Start dolt sql-server as detached process
        let mut cmd = tokio::process::Command::new("dolt");
        cmd.args(["sql-server", "-H", "127.0.0.1", "-P", &port.to_string()]);
        cmd.current_dir(&dolt_dir);
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());

        let child = cmd.spawn().with_context(|| {
            format!(
                "starting dolt sql-server in {} (is dolt installed?)",
                dolt_dir.display()
            )
        })?;

        // Write PID + port files so bd/rsry can find this server later
        let beads_dir = &config.beads_dir;
        let _ = std::fs::write(
            beads_dir.join("dolt-server.pid"),
            child.id().unwrap_or(0).to_string(),
        );
        let _ = std::fs::write(beads_dir.join("dolt-server.port"), port.to_string());

        // Wait for server to accept connections (up to 10s)
        let addr = format!("127.0.0.1:{port}");
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if tokio::time::Instant::now() > deadline {
                anyhow::bail!(
                    "dolt sql-server started but not accepting connections on port {port}"
                );
            }
            if tokio::net::TcpStream::connect(&addr).await.is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }

        // Connect via MySQL
        let url = format!("mysql://root@127.0.0.1:{port}/{}", config.database);
        let pool =
            tokio::time::timeout(std::time::Duration::from_secs(5), MySqlPool::connect(&url))
                .await
                .with_context(|| format!("timeout connecting after auto-start on port {port}"))?
                .with_context(|| format!("connecting to Dolt at {url}"))?;

        eprintln!("[dolt] server started on port {port}");
        Ok(DoltClient { pool })
    }

    /// Commit the current working set so changes are visible to new connections.
    /// Dolt sql-server uses per-session isolation — writes are invisible to other
    /// connections until committed. Best-effort: logs warning on failure.
    async fn auto_commit(&self, message: &str) {
        // -Am stages all tables + commits in one call (dolt cheat sheet)
        let result = query("CALL DOLT_COMMIT('-Am', ?, '--allow-empty')")
            .bind(message)
            .execute(&self.pool)
            .await;
        if let Err(e) = result {
            eprintln!("[dolt] auto_commit failed: {e}");
        }
    }

    /// Parse files and test_files from the notes JSON column.
    fn parse_files_from_notes(row: &sqlx_mysql::MySqlRow) -> (Vec<String>, Vec<String>) {
        let notes: Option<String> = row.try_get("notes").ok();
        let parsed: Option<serde_json::Value> = notes.and_then(|s| serde_json::from_str(&s).ok());
        let files = parsed
            .as_ref()
            .and_then(|v| v.get("files"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let test_files = parsed
            .as_ref()
            .and_then(|v| v.get("test_files"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        (files, test_files)
    }

    /// List all open issues as Beads.
    pub async fn list_beads(&self, repo_name: &str) -> Result<Vec<Bead>> {
        let rows = query(
            r#"SELECT i.id, i.title, i.description, i.status, i.priority, i.issue_type,
                      i.assignee, i.external_ref, i.notes, i.created_at, i.updated_at,
                      COALESCE(dep.cnt, 0) as dep_count,
                      COALESCE(deps.cnt, 0) as dependency_count,
                      COALESCE(cmt.cnt, 0) as comment_count
               FROM issues i
               LEFT JOIN (SELECT depends_on_id, COUNT(*) as cnt FROM dependencies GROUP BY depends_on_id) dep
                    ON dep.depends_on_id = i.id
               LEFT JOIN (SELECT issue_id, COUNT(*) as cnt FROM dependencies GROUP BY issue_id) deps
                    ON deps.issue_id = i.id
               LEFT JOIN (SELECT issue_id, COUNT(*) as cnt FROM comments GROUP BY issue_id) cmt
                    ON cmt.issue_id = i.id
               WHERE i.status != 'closed'
               ORDER BY i.priority ASC, i.created_at DESC"#,
        )
        .fetch_all(&self.pool)
        .await
        .context("querying issues")?;

        let beads = rows
            .iter()
            .map(|row| {
                let (files, test_files) = Self::parse_files_from_notes(row);
                Bead {
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
                    external_ref: row.try_get("external_ref").ok(),
                    files,
                    test_files,
                }
            })
            .collect();

        Ok(beads)
    }

    /// Get a single bead by ID.
    pub async fn get_bead(&self, id: &str, repo_name: &str) -> Result<Option<Bead>> {
        let row = query(
            r#"SELECT id, title, description, status, priority, issue_type,
                      assignee, external_ref, notes, created_at, updated_at,
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

        Ok(row.map(|row| {
            let (files, test_files) = Self::parse_files_from_notes(&row);
            Bead {
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
                external_ref: row.try_get("external_ref").ok(),
                branch: None,
                pr_url: None,
                jj_change_id: None,
                files,
                test_files,
            }
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
        self.auto_commit(&format!("{id}: status → {status}")).await;
        Ok(())
    }

    /// Update a bead's assignee (owner).
    pub async fn set_assignee(&self, id: &str, assignee: &str) -> Result<()> {
        query("UPDATE issues SET assignee = ?, updated_at = NOW() WHERE id = ?")
            .bind(assignee)
            .bind(id)
            .execute(&self.pool)
            .await
            .with_context(|| format!("setting assignee for {id}"))?;
        Ok(())
    }

    /// Get the current status of a bead by ID.
    pub async fn get_status(&self, id: &str) -> Result<Option<String>> {
        let row = query("SELECT status FROM issues WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .with_context(|| format!("querying status for {id}"))?;
        Ok(row.map(|r| r.get("status")))
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
        self.auto_commit(&format!("create {id}")).await;
        Ok(())
    }

    /// Set files and test_files on a bead. Stored as JSON in the notes column.
    pub async fn set_files(&self, id: &str, files: &[String], test_files: &[String]) -> Result<()> {
        let file_json = serde_json::json!({
            "files": files,
            "test_files": test_files,
        });
        query("UPDATE issues SET notes = ?, updated_at = NOW() WHERE id = ?")
            .bind(file_json.to_string())
            .bind(id)
            .execute(&self.pool)
            .await
            .with_context(|| format!("setting files for {id}"))?;
        Ok(())
    }

    /// PATCH-style update: only writes fields that are `Some` in the update.
    /// Returns the list of field names that were actually updated.
    pub async fn update_bead_fields(
        &self,
        id: &str,
        update: &crate::bead::BeadUpdate,
    ) -> Result<Vec<String>> {
        let mut set_clauses = Vec::new();
        let mut bind_values: Vec<String> = Vec::new();
        let mut updated_fields = Vec::new();

        if let Some(ref title) = update.title {
            set_clauses.push("title = ?");
            bind_values.push(title.clone());
            updated_fields.push("title".to_string());
        }
        if let Some(ref description) = update.description {
            set_clauses.push("description = ?");
            bind_values.push(description.clone());
            updated_fields.push("description".to_string());
        }
        if let Some(priority) = update.priority {
            set_clauses.push("priority = ?");
            bind_values.push(priority.to_string());
            updated_fields.push("priority".to_string());
        }
        if let Some(ref issue_type) = update.issue_type {
            set_clauses.push("issue_type = ?");
            bind_values.push(issue_type.clone());
            updated_fields.push("issue_type".to_string());
        }
        if let Some(ref owner) = update.owner {
            set_clauses.push("assignee = ?");
            bind_values.push(owner.clone());
            updated_fields.push("owner".to_string());
        }
        if update.files.is_some() || update.test_files.is_some() {
            // Files/test_files are stored as JSON in the notes column.
            // Read existing notes first to preserve fields not being updated.
            let existing_notes: serde_json::Value = {
                let row = query("SELECT notes FROM issues WHERE id = ?")
                    .bind(id)
                    .fetch_optional(&self.pool)
                    .await?;
                row.and_then(|r| {
                    r.try_get::<String, _>("notes")
                        .ok()
                        .and_then(|s| serde_json::from_str(&s).ok())
                })
                .unwrap_or_else(|| serde_json::json!({}))
            };

            let files = update
                .files
                .as_ref()
                .map(|f| serde_json::json!(f))
                .unwrap_or_else(|| {
                    existing_notes
                        .get("files")
                        .cloned()
                        .unwrap_or(serde_json::json!([]))
                });
            let test_files = update
                .test_files
                .as_ref()
                .map(|f| serde_json::json!(f))
                .unwrap_or_else(|| {
                    existing_notes
                        .get("test_files")
                        .cloned()
                        .unwrap_or(serde_json::json!([]))
                });

            let notes_json = serde_json::json!({
                "files": files,
                "test_files": test_files,
            });
            set_clauses.push("notes = ?");
            bind_values.push(notes_json.to_string());
            if update.files.is_some() {
                updated_fields.push("files".to_string());
            }
            if update.test_files.is_some() {
                updated_fields.push("test_files".to_string());
            }
        }

        if set_clauses.is_empty() {
            return Ok(updated_fields);
        }

        set_clauses.push("updated_at = NOW()");
        let sql = format!("UPDATE issues SET {} WHERE id = ?", set_clauses.join(", "));

        // Build the query with dynamic binds.
        // sqlx doesn't support dynamic bind count easily, so we use raw SQL via execute_raw
        // after safely escaping. However, since all values are strings and we control the SQL,
        // we build a parameterized query manually.
        let mut q = query(&sql);
        for val in &bind_values {
            q = q.bind(val);
        }
        q = q.bind(id);
        q.execute(&self.pool)
            .await
            .with_context(|| format!("updating fields for {id}"))?;

        self.auto_commit(&format!("update {id}: {}", updated_fields.join(", ")))
            .await;
        Ok(updated_fields)
    }

    /// Close a bead by setting its status to 'closed'.
    pub async fn close_bead(&self, id: &str) -> Result<()> {
        query("UPDATE issues SET status = 'closed', updated_at = NOW() WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .with_context(|| format!("closing bead {id}"))?;
        self.auto_commit(&format!("close {id}")).await;
        Ok(())
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

    #[allow(dead_code)] // API surface for rsry bead search
    /// Search beads by title or description substring match.
    pub async fn search_beads(&self, query_str: &str, repo_name: &str) -> Result<Vec<Bead>> {
        let pattern = format!("%{query_str}%");
        let rows = query(
            r#"SELECT i.id, i.title, i.description, i.status, i.priority, i.issue_type,
                      i.assignee, i.external_ref, i.notes, i.created_at, i.updated_at,
                      COALESCE(dep.cnt, 0) as dep_count,
                      COALESCE(deps.cnt, 0) as dependency_count,
                      COALESCE(cmt.cnt, 0) as comment_count
               FROM issues i
               LEFT JOIN (SELECT depends_on_id, COUNT(*) as cnt FROM dependencies GROUP BY depends_on_id) dep
                    ON dep.depends_on_id = i.id
               LEFT JOIN (SELECT issue_id, COUNT(*) as cnt FROM dependencies GROUP BY issue_id) deps
                    ON deps.issue_id = i.id
               LEFT JOIN (SELECT issue_id, COUNT(*) as cnt FROM comments GROUP BY issue_id) cmt
                    ON cmt.issue_id = i.id
               WHERE i.title LIKE ? OR i.description LIKE ?
               ORDER BY i.priority ASC, i.created_at DESC
               LIMIT 50"#,
        )
        .bind(&pattern)
        .bind(&pattern)
        .fetch_all(&self.pool)
        .await
        .with_context(|| format!("searching beads for '{query_str}'"))?;

        let beads = rows
            .iter()
            .map(|row| {
                let (files, test_files) = Self::parse_files_from_notes(row);
                Bead {
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
                    external_ref: row.try_get("external_ref").ok(),
                    files,
                    test_files,
                }
            })
            .collect();

        Ok(beads)
    }

    /// Get the external_ref for a bead (e.g., "AGE-5").
    /// Used by persist_status to mirror state transitions to Linear.
    pub async fn get_external_ref(&self, id: &str) -> Result<Option<String>> {
        let row = query(
            "SELECT external_ref FROM issues WHERE id = ? AND external_ref IS NOT NULL AND external_ref != ''",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("getting external_ref for {id}"))?;
        Ok(row.and_then(|r| r.try_get::<String, _>("external_ref").ok()))
    }

    #[allow(dead_code)] // API surface — used by sync module
    /// Set the external_ref for a bead (e.g., Linear issue identifier like "AGE-5").
    pub async fn set_external_ref(&self, id: &str, external_ref: &str) -> Result<()> {
        query("UPDATE issues SET external_ref = ?, updated_at = NOW() WHERE id = ?")
            .bind(external_ref)
            .bind(id)
            .execute(&self.pool)
            .await
            .with_context(|| format!("setting external_ref for {id}"))?;
        Ok(())
    }

    /// Find a bead by its external_ref (e.g., "AGE-5").
    /// Returns the bead ID if found. Used by the webhook handler to map
    /// Linear issue identifiers back to local beads.
    #[allow(dead_code)] // Called from serve.rs webhook handler
    pub async fn find_by_external_ref(&self, external_ref: &str) -> Result<Option<String>> {
        let row = query("SELECT id FROM issues WHERE external_ref = ?")
            .bind(external_ref)
            .fetch_optional(&self.pool)
            .await
            .with_context(|| format!("finding bead by external_ref {external_ref}"))?;
        Ok(row.map(|r| r.get("id")))
    }

    #[allow(dead_code)] // Called from linear.rs sync() — clippy can't trace async
    /// List closed beads that have an external_ref set.
    /// Used by sync to propagate close status to external trackers.
    pub async fn list_closed_linked_beads(&self, repo_name: &str) -> Result<Vec<Bead>> {
        let rows = query(
            r#"SELECT id, title, description, status, priority, issue_type,
                      assignee, external_ref, created_at, updated_at,
                      0 as dep_count, 0 as dependency_count, 0 as comment_count
               FROM issues
               WHERE status = 'closed' AND external_ref IS NOT NULL AND external_ref != ''
               ORDER BY updated_at DESC
               LIMIT 500"#,
        )
        .fetch_all(&self.pool)
        .await
        .context("querying closed linked beads")?;

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
                external_ref: row.try_get("external_ref").ok(),
                files: Vec::new(),
                test_files: Vec::new(),
            })
            .collect();

        Ok(beads)
    }

    /// Execute a raw SQL statement. Best-effort, for operations not covered by typed methods.
    pub async fn execute_raw(&self, sql: &str) -> anyhow::Result<()> {
        query(sql)
            .execute(&self.pool)
            .await
            .with_context(|| format!("executing raw SQL: {}", &sql[..sql.len().min(80)]))?;
        Ok(())
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

    /// Sandboxed Dolt beads database for integration testing.
    ///
    /// Spins up a fresh Dolt instance in a temp directory with the beads schema,
    /// then kills the server on drop. Each `fresh_client()` call returns a new
    /// connection pool — simulating an MCP reconnect.
    struct SandboxBeads {
        config: DoltConfig,
        _tmp: TempDir,
    }

    impl SandboxBeads {
        async fn new() -> Option<Self> {
            if std::process::Command::new("dolt")
                .arg("version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_err()
            {
                eprintln!("skipping: dolt not installed");
                return None;
            }

            let tmp = TempDir::new().unwrap();
            let beads_dir = tmp.path();
            let db_dir = beads_dir.join("dolt").join("beads");
            std::fs::create_dir_all(&db_dir).unwrap();

            // Initialize dolt database
            let status = std::process::Command::new("dolt")
                .args(["init"])
                .current_dir(&db_dir)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .expect("dolt init");
            assert!(status.success(), "dolt init failed");

            std::fs::write(
                beads_dir.join("metadata.json"),
                r#"{"dolt_database": "beads"}"#,
            )
            .unwrap();

            // port=0 → connect() will auto-start
            let config = DoltConfig::from_beads_dir(beads_dir).unwrap();
            let client = DoltClient::connect(&config).await.unwrap();

            // Create beads schema
            for sql in [
                "CREATE TABLE issues (
                    id VARCHAR(128) PRIMARY KEY,
                    title VARCHAR(512) NOT NULL,
                    description TEXT,
                    design TEXT DEFAULT '',
                    acceptance_criteria TEXT DEFAULT '',
                    notes TEXT DEFAULT '',
                    status VARCHAR(32) NOT NULL DEFAULT 'open',
                    priority INT NOT NULL DEFAULT 2,
                    issue_type VARCHAR(32) NOT NULL DEFAULT 'task',
                    assignee VARCHAR(128),
                    external_ref VARCHAR(128),
                    created_at DATETIME NOT NULL,
                    updated_at DATETIME NOT NULL
                )",
                "CREATE TABLE comments (
                    issue_id VARCHAR(128) NOT NULL,
                    text TEXT NOT NULL,
                    author VARCHAR(128) NOT NULL,
                    created_at DATETIME NOT NULL
                )",
                "CREATE TABLE dependencies (
                    issue_id VARCHAR(128) NOT NULL,
                    depends_on_id VARCHAR(128) NOT NULL,
                    PRIMARY KEY (issue_id, depends_on_id)
                )",
                "CREATE TABLE events (
                    issue_id VARCHAR(128) NOT NULL,
                    event_type VARCHAR(64) NOT NULL,
                    actor VARCHAR(128) NOT NULL,
                    comment TEXT,
                    created_at DATETIME NOT NULL
                )",
            ] {
                client.execute_raw(sql).await.unwrap();
            }

            // Commit schema so it's visible to all future connections
            client
                .execute_raw("CALL DOLT_COMMIT('-Am', 'init schema', '--allow-empty')")
                .await
                .unwrap();

            // Re-read config to pick up the port written by auto-start
            let config = DoltConfig::from_beads_dir(beads_dir).unwrap();
            Some(SandboxBeads { config, _tmp: tmp })
        }

        /// Each call returns a fresh connection pool — simulates MCP reconnect.
        async fn fresh_client(&self) -> DoltClient {
            DoltClient::connect(&self.config).await.unwrap()
        }
    }

    impl Drop for SandboxBeads {
        fn drop(&mut self) {
            let pid_file = self.config.beads_dir.join("dolt-server.pid");
            if let Ok(pid_str) = std::fs::read_to_string(&pid_file)
                && let Ok(pid) = pid_str.trim().parse::<i32>()
            {
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
            }
        }
    }

    // ── Sandboxed cross-connection tests ────────────────────

    /// The exact bug scenario: bead created on connection A must be
    /// findable from a completely new connection B (simulating MCP reconnect).
    #[tokio::test]
    async fn create_bead_visible_to_new_connection() {
        let sandbox = match SandboxBeads::new().await {
            Some(s) => s,
            None => return,
        };

        // Session A: create a bead
        let client_a = sandbox.fresh_client().await;
        client_a
            .create_bead(
                "vis-1",
                "Cross-session visibility",
                "Should survive reconnect",
                1,
                "bug",
            )
            .await
            .unwrap();
        drop(client_a);

        // Session B: completely new pool — must see the bead
        let client_b = sandbox.fresh_client().await;
        let found = client_b
            .search_beads("Cross-session", "test")
            .await
            .unwrap();
        assert!(
            found.iter().any(|b| b.id == "vis-1"),
            "bead created in session A must be visible to session B (auto_commit guarantees this)"
        );

        let bead = client_b.get_bead("vis-1", "test").await.unwrap();
        assert!(bead.is_some());
        assert_eq!(bead.unwrap().title, "Cross-session visibility");
    }

    /// Every write path must auto-commit: update_status, close_bead,
    /// add_comment, update_bead_fields. Verified by checking from a fresh connection.
    #[tokio::test]
    async fn all_write_paths_visible_across_connections() {
        let sandbox = match SandboxBeads::new().await {
            Some(s) => s,
            None => return,
        };

        // Setup: create bead
        let setup = sandbox.fresh_client().await;
        setup
            .create_bead("wp-1", "Write paths test", "desc", 2, "task")
            .await
            .unwrap();
        drop(setup);

        // update_status
        let writer = sandbox.fresh_client().await;
        writer.update_status("wp-1", "in_progress").await.unwrap();
        drop(writer);

        let reader = sandbox.fresh_client().await;
        let status = reader.get_status("wp-1").await.unwrap();
        assert_eq!(
            status.as_deref(),
            Some("in_progress"),
            "update_status must auto_commit"
        );
        drop(reader);

        // add_comment
        let writer = sandbox.fresh_client().await;
        writer
            .add_comment("wp-1", "test comment", "test-runner")
            .await
            .unwrap();
        drop(writer);

        let reader = sandbox.fresh_client().await;
        let bead = reader.get_bead("wp-1", "test").await.unwrap().unwrap();
        assert_eq!(bead.comment_count, 1, "add_comment must auto_commit");
        drop(reader);

        // update_bead_fields (PATCH)
        let writer = sandbox.fresh_client().await;
        let update = crate::bead::BeadUpdate {
            title: Some("Updated title".into()),
            ..Default::default()
        };
        writer.update_bead_fields("wp-1", &update).await.unwrap();
        drop(writer);

        let reader = sandbox.fresh_client().await;
        let bead = reader.get_bead("wp-1", "test").await.unwrap().unwrap();
        assert_eq!(
            bead.title, "Updated title",
            "update_bead_fields must auto_commit"
        );
        drop(reader);

        // close_bead
        let writer = sandbox.fresh_client().await;
        writer.close_bead("wp-1").await.unwrap();
        drop(writer);

        let reader = sandbox.fresh_client().await;
        let bead = reader.get_bead("wp-1", "test").await.unwrap().unwrap();
        assert_eq!(bead.status, "closed", "close_bead must auto_commit");
    }

    // ── Existing tests ──────────────────────────────────────

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
    fn parse_dolt_config_no_port_file_returns_port_zero() {
        let dir = TempDir::new().unwrap();
        let config = DoltConfig::from_beads_dir(dir.path()).unwrap();
        assert_eq!(config.port, 0); // No server — auto-start will handle it
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
