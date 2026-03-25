//! SQLite-backed per-repo bead store.
//!
//! Each repo gets its own `.beads/beads.db` file. No server process needed.
//! Schema mirrors the Dolt `issues`/`dependencies`/`comments`/`events` tables.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{NaiveDateTime, TimeZone, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::bead::{Bead, BeadUpdate};
use crate::store::BeadStore;

/// Parse files and test_files from the notes JSON column.
/// Shared between SQLite and Dolt implementations.
pub fn parse_files_from_notes(notes: Option<&str>) -> (Vec<String>, Vec<String>) {
    let parsed: Option<serde_json::Value> = notes.and_then(|s| serde_json::from_str(s).ok());
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

/// Parse a datetime string from SQLite into a chrono DateTime<Utc>.
fn parse_datetime(s: &str) -> chrono::DateTime<Utc> {
    // Try ISO 8601 formats: "2024-01-15 12:34:56" or "2024-01-15T12:34:56"
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S"))
        .map(|ndt| Utc.from_utc_datetime(&ndt))
        .unwrap_or_else(|_| Utc::now())
}

/// Read a Bead from a rusqlite Row.
fn bead_from_row(row: &rusqlite::Row<'_>, repo_name: &str) -> rusqlite::Result<Bead> {
    let notes: Option<String> = row.get("notes")?;
    let (files, test_files) = parse_files_from_notes(notes.as_deref());
    let created_str: String = row.get("created_at")?;
    let updated_str: String = row.get("updated_at")?;

    Ok(Bead {
        id: row.get("id")?,
        title: row.get("title")?,
        description: row
            .get::<_, Option<String>>("description")?
            .unwrap_or_default(),
        status: row.get("status")?,
        priority: row.get::<_, i32>("priority").unwrap_or(2) as u8,
        issue_type: row
            .get::<_, Option<String>>("issue_type")?
            .unwrap_or_else(|| "task".into()),
        owner: row.get("assignee")?,
        repo: repo_name.to_string(),
        created_at: parse_datetime(&created_str),
        updated_at: parse_datetime(&updated_str),
        dependency_count: row.get::<_, i64>("dependency_count").unwrap_or(0) as u32,
        dependent_count: row.get::<_, i64>("dep_count").unwrap_or(0) as u32,
        comment_count: row.get::<_, i64>("comment_count").unwrap_or(0) as u32,
        branch: None,
        pr_url: None,
        jj_change_id: None,
        external_ref: row.get("external_ref")?,
        files,
        test_files,
    })
}

/// Connect to the bead store for a repo's `.beads/` directory.
///
/// Default: Dolt (per-repo version-controlled database — branch-per-agent,
/// cell-level merge, commit history). Falls back to SQLite if Dolt is
/// unavailable (no server, no port file) or if `beads.db` exists and Dolt
/// doesn't.
///
/// SQLite is useful for: tests, offline/lightweight repos, portable exports.
/// Dolt is the production default for repos with active agent dispatch.
pub async fn connect_bead_store(beads_dir: &Path) -> Result<Box<dyn BeadStore>> {
    // Try Dolt first (production default)
    let dolt_dir = beads_dir.join("dolt");
    if dolt_dir.exists() {
        match crate::dolt::DoltConfig::from_beads_dir(beads_dir) {
            Ok(config) => match crate::dolt::DoltClient::connect(&config).await {
                Ok(client) => return Ok(Box::new(crate::bead_dolt::DoltBeadStore::new(client))),
                Err(e) => {
                    eprintln!(
                        "[bead] Dolt connect failed for {}, trying SQLite fallback: {e}",
                        beads_dir.display()
                    );
                }
            },
            Err(e) => {
                eprintln!(
                    "[bead] Dolt config error for {}, trying SQLite fallback: {e}",
                    beads_dir.display()
                );
            }
        }
    }

    // Fallback: SQLite (lightweight, no server needed)
    let sqlite_path = beads_dir.join("beads.db");
    let store = SqliteBeadStore::connect(&sqlite_path)?;
    Ok(Box::new(store))
}

pub struct SqliteBeadStore {
    conn: Mutex<Connection>,
    #[allow(dead_code)]
    path: PathBuf,
}

impl SqliteBeadStore {
    /// Open or create the bead database at the given path.
    pub fn connect(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).context("opening sqlite bead store")?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(SqliteBeadStore {
            conn: Mutex::new(conn),
            path: path.to_path_buf(),
        })
    }
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS issues (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    description TEXT DEFAULT '',
    design TEXT DEFAULT '',
    acceptance_criteria TEXT DEFAULT '',
    notes TEXT DEFAULT '',
    status TEXT NOT NULL DEFAULT 'open',
    priority INTEGER NOT NULL DEFAULT 2,
    issue_type TEXT NOT NULL DEFAULT 'task',
    assignee TEXT,
    external_ref TEXT,
    user_id TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS dependencies (
    issue_id TEXT NOT NULL,
    depends_on_id TEXT NOT NULL,
    PRIMARY KEY (issue_id, depends_on_id)
);

CREATE TABLE IF NOT EXISTS comments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    issue_id TEXT NOT NULL,
    text TEXT NOT NULL,
    author TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    issue_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    actor TEXT NOT NULL DEFAULT 'rosary',
    comment TEXT,
    created_at TEXT NOT NULL
);
";

/// SQL for listing beads with dependency/comment counts (non-closed).
const LIST_BEADS_SQL: &str = "
SELECT i.id, i.title, i.description, i.status, i.priority, i.issue_type,
       i.assignee, i.external_ref, i.notes, i.created_at, i.updated_at,
       COALESCE(dep.cnt, 0) as dep_count,
       COALESCE(deps.cnt, 0) as dependency_count,
       COALESCE(cmt.cnt, 0) as comment_count
FROM issues i
LEFT JOIN (SELECT depends_on_id, COUNT(*) as cnt FROM dependencies GROUP BY depends_on_id) dep
     ON dep.depends_on_id = i.id
LEFT JOIN (SELECT d.issue_id, COUNT(*) as cnt
          FROM dependencies d
          JOIN issues dep_i ON dep_i.id = d.depends_on_id
          WHERE dep_i.status NOT IN ('closed', 'done')
          GROUP BY d.issue_id) deps
     ON deps.issue_id = i.id
LEFT JOIN (SELECT issue_id, COUNT(*) as cnt FROM comments GROUP BY issue_id) cmt
     ON cmt.issue_id = i.id
WHERE i.status NOT IN ('closed', 'done')
ORDER BY i.priority ASC, i.created_at DESC
";

#[async_trait]
impl BeadStore for SqliteBeadStore {
    async fn list_beads(&self, repo_name: &str) -> Result<Vec<Bead>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(LIST_BEADS_SQL)?;
        let beads = stmt
            .query_map([], |row| bead_from_row(row, repo_name))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(beads)
    }

    async fn list_beads_scoped(&self, repo_name: &str, user_id: Option<&str>) -> Result<Vec<Bead>> {
        match user_id {
            Some(uid) => {
                let conn = self.conn.lock().unwrap();
                let sql_scoped = "
                    SELECT i.id, i.title, i.description, i.status, i.priority, i.issue_type,
                           i.assignee, i.external_ref, i.notes, i.created_at, i.updated_at,
                           COALESCE(dep.cnt, 0) as dep_count,
                           COALESCE(deps.cnt, 0) as dependency_count,
                           COALESCE(cmt.cnt, 0) as comment_count
                    FROM issues i
                    LEFT JOIN (SELECT depends_on_id, COUNT(*) as cnt FROM dependencies GROUP BY depends_on_id) dep
                         ON dep.depends_on_id = i.id
                    LEFT JOIN (SELECT d.issue_id, COUNT(*) as cnt
                              FROM dependencies d
                              JOIN issues dep_i ON dep_i.id = d.depends_on_id
                              WHERE dep_i.status NOT IN ('closed', 'done')
                              GROUP BY d.issue_id) deps
                         ON deps.issue_id = i.id
                    LEFT JOIN (SELECT issue_id, COUNT(*) as cnt FROM comments GROUP BY issue_id) cmt
                         ON cmt.issue_id = i.id
                    WHERE i.status NOT IN ('closed', 'done') AND i.user_id = ?1
                    ORDER BY i.priority ASC, i.created_at DESC
                ";
                let mut stmt = conn.prepare(sql_scoped)?;
                let beads = stmt
                    .query_map(params![uid], |row| bead_from_row(row, repo_name))?
                    .filter_map(|r| r.ok())
                    .collect();
                Ok(beads)
            }
            None => self.list_beads(repo_name).await,
        }
    }

    async fn get_bead(&self, id: &str, repo_name: &str) -> Result<Option<Bead>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT i.id, i.title, i.description, i.status, i.priority, i.issue_type,
                    i.assignee, i.external_ref, i.notes, i.created_at, i.updated_at,
                    (SELECT COUNT(*) FROM dependencies d WHERE d.depends_on_id = i.id) as dep_count,
                    (SELECT COUNT(*) FROM dependencies d
                            JOIN issues dep_i ON dep_i.id = d.depends_on_id
                            WHERE d.issue_id = i.id
                            AND dep_i.status NOT IN ('closed', 'done')) as dependency_count,
                    (SELECT COUNT(*) FROM comments c WHERE c.issue_id = i.id) as comment_count
             FROM issues i
             WHERE i.id = ?1",
        )?;
        let bead = stmt
            .query_row(params![id], |row| bead_from_row(row, repo_name))
            .optional()?;
        Ok(bead)
    }

    async fn create_bead(
        &self,
        id: &str,
        title: &str,
        description: &str,
        priority: u8,
        issue_type: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO issues (id, title, description, design, acceptance_criteria, notes, status, priority, issue_type, created_at, updated_at)
             VALUES (?1, ?2, ?3, '', '', '', 'open', ?4, ?5, datetime('now'), datetime('now'))",
            params![id, title, description, priority as i32, issue_type],
        )
        .with_context(|| format!("creating bead {id}"))?;
        Ok(())
    }

    async fn create_bead_full(
        &self,
        id: &str,
        title: &str,
        description: &str,
        priority: u8,
        issue_type: &str,
        owner: &str,
        files: &[String],
        test_files: &[String],
        depends_on: &[String],
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let tx = conn.unchecked_transaction()?;

        tx.execute(
            "INSERT INTO issues (id, title, description, design, acceptance_criteria, notes, status, priority, issue_type, created_at, updated_at)
             VALUES (?1, ?2, ?3, '', '', '', 'open', ?4, ?5, datetime('now'), datetime('now'))",
            params![id, title, description, priority as i32, issue_type],
        )?;

        tx.execute(
            "UPDATE issues SET assignee = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![owner, id],
        )?;

        if !files.is_empty() || !test_files.is_empty() {
            let file_json = serde_json::json!({ "files": files, "test_files": test_files });
            tx.execute(
                "UPDATE issues SET notes = ?1, updated_at = datetime('now') WHERE id = ?2",
                params![file_json.to_string(), id],
            )?;
        }

        for dep_id in depends_on {
            tx.execute(
                "INSERT OR IGNORE INTO dependencies (issue_id, depends_on_id) VALUES (?1, ?2)",
                params![id, dep_id],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    async fn update_bead_fields(&self, id: &str, update: &BeadUpdate) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut updated_fields = Vec::new();

        // Build dynamic SET clauses. We execute each field update separately
        // to avoid dynamic bind complexity with rusqlite.
        if let Some(ref title) = update.title {
            conn.execute(
                "UPDATE issues SET title = ?1, updated_at = datetime('now') WHERE id = ?2",
                params![title, id],
            )?;
            updated_fields.push("title".to_string());
        }
        if let Some(ref description) = update.description {
            conn.execute(
                "UPDATE issues SET description = ?1, updated_at = datetime('now') WHERE id = ?2",
                params![description, id],
            )?;
            updated_fields.push("description".to_string());
        }
        if let Some(priority) = update.priority {
            conn.execute(
                "UPDATE issues SET priority = ?1, updated_at = datetime('now') WHERE id = ?2",
                params![priority as i32, id],
            )?;
            updated_fields.push("priority".to_string());
        }
        if let Some(ref issue_type) = update.issue_type {
            conn.execute(
                "UPDATE issues SET issue_type = ?1, updated_at = datetime('now') WHERE id = ?2",
                params![issue_type, id],
            )?;
            updated_fields.push("issue_type".to_string());
        }
        if let Some(ref owner) = update.owner {
            conn.execute(
                "UPDATE issues SET assignee = ?1, updated_at = datetime('now') WHERE id = ?2",
                params![owner, id],
            )?;
            updated_fields.push("owner".to_string());
        }
        if update.files.is_some() || update.test_files.is_some() {
            // Read existing notes to preserve fields not being updated
            let existing_notes: serde_json::Value = conn
                .query_row(
                    "SELECT notes FROM issues WHERE id = ?1",
                    params![id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .ok()
                .flatten()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_else(|| serde_json::json!({}));

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
            let test_files_val = update
                .test_files
                .as_ref()
                .map(|f| serde_json::json!(f))
                .unwrap_or_else(|| {
                    existing_notes
                        .get("test_files")
                        .cloned()
                        .unwrap_or(serde_json::json!([]))
                });
            let notes_json = serde_json::json!({ "files": files, "test_files": test_files_val });
            conn.execute(
                "UPDATE issues SET notes = ?1, updated_at = datetime('now') WHERE id = ?2",
                params![notes_json.to_string(), id],
            )?;
            if update.files.is_some() {
                updated_fields.push("files".to_string());
            }
            if update.test_files.is_some() {
                updated_fields.push("test_files".to_string());
            }
        }

        Ok(updated_fields)
    }

    async fn update_status(&self, id: &str, status: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE issues SET status = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![status, id],
        )
        .with_context(|| format!("updating status for {id}"))?;
        Ok(())
    }

    async fn get_status(&self, id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let status = conn
            .query_row(
                "SELECT status FROM issues WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(status)
    }

    async fn close_bead(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE issues SET status = 'closed', updated_at = datetime('now') WHERE id = ?1",
            params![id],
        )
        .with_context(|| format!("closing bead {id}"))?;
        Ok(())
    }

    async fn set_assignee(&self, id: &str, assignee: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE issues SET assignee = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![assignee, id],
        )?;
        Ok(())
    }

    async fn set_user_id(&self, id: &str, user_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE issues SET user_id = ?1 WHERE id = ?2",
            params![user_id, id],
        )?;
        Ok(())
    }

    async fn set_files(&self, id: &str, files: &[String], test_files: &[String]) -> Result<()> {
        let file_json = serde_json::json!({ "files": files, "test_files": test_files });
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE issues SET notes = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![file_json.to_string(), id],
        )?;
        Ok(())
    }

    async fn search_beads(
        &self,
        query_str: &str,
        repo_name: &str,
        limit: u32,
    ) -> Result<Vec<Bead>> {
        let conn = self.conn.lock().unwrap();
        let words: Vec<String> = query_str
            .split_whitespace()
            .map(|w| format!("%{}%", w.to_lowercase()))
            .collect();

        let where_clause = if words.is_empty() {
            "1=1".to_string()
        } else {
            words
                .iter()
                .enumerate()
                .map(|(i, _)| {
                    let p = (i * 2) + 1;
                    format!(
                        "(LOWER(i.title) LIKE ?{p} OR LOWER(i.description) LIKE ?{})",
                        p + 1
                    )
                })
                .collect::<Vec<_>>()
                .join(" AND ")
        };

        let sql = format!(
            "SELECT i.id, i.title, i.description, i.status, i.priority, i.issue_type,
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
             WHERE {where_clause}
             ORDER BY i.priority ASC, i.created_at DESC
             LIMIT {limit}"
        );

        let mut stmt = conn.prepare(&sql)?;
        // Build params: each word appears twice (title + description)
        let param_values: Vec<Box<dyn rusqlite::types::ToSql>> = words
            .iter()
            .flat_map(|w| {
                vec![
                    Box::new(w.clone()) as Box<dyn rusqlite::types::ToSql>,
                    Box::new(w.clone()) as Box<dyn rusqlite::types::ToSql>,
                ]
            })
            .collect();
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();

        let beads = stmt
            .query_map(param_refs.as_slice(), |row| bead_from_row(row, repo_name))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(beads)
    }

    async fn get_external_ref(&self, id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let result = conn
            .query_row(
                "SELECT external_ref FROM issues WHERE id = ?1 AND external_ref IS NOT NULL AND external_ref != ''",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(result)
    }

    async fn set_external_ref(&self, id: &str, external_ref: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE issues SET external_ref = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![external_ref, id],
        )?;
        Ok(())
    }

    async fn find_by_external_ref(&self, external_ref: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let result = conn
            .query_row(
                "SELECT id FROM issues WHERE external_ref = ?1",
                params![external_ref],
                |row| row.get(0),
            )
            .optional()?;
        Ok(result)
    }

    async fn list_closed_linked_beads(&self, repo_name: &str) -> Result<Vec<Bead>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, title, description, status, priority, issue_type,
                    assignee, external_ref, '' as notes, created_at, updated_at,
                    0 as dep_count, 0 as dependency_count, 0 as comment_count
             FROM issues
             WHERE status = 'closed' AND external_ref IS NOT NULL AND external_ref != ''
             ORDER BY updated_at DESC
             LIMIT 500",
        )?;
        let beads = stmt
            .query_map([], |row| bead_from_row(row, repo_name))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(beads)
    }

    async fn add_dependency(&self, issue_id: &str, depends_on_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO dependencies (issue_id, depends_on_id) VALUES (?1, ?2)",
            params![issue_id, depends_on_id],
        )?;
        Ok(())
    }

    async fn remove_dependency(&self, issue_id: &str, depends_on_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM dependencies WHERE issue_id = ?1 AND depends_on_id = ?2",
            params![issue_id, depends_on_id],
        )?;
        Ok(())
    }

    async fn get_dependencies(&self, issue_id: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT depends_on_id FROM dependencies WHERE issue_id = ?1")?;
        let deps = stmt
            .query_map(params![issue_id], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(deps)
    }

    async fn get_dependents(&self, issue_id: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT issue_id FROM dependencies WHERE depends_on_id = ?1")?;
        let deps = stmt
            .query_map(params![issue_id], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(deps)
    }

    async fn add_comment(&self, issue_id: &str, body: &str, author: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO comments (issue_id, text, author, created_at) VALUES (?1, ?2, ?3, datetime('now'))",
            params![issue_id, body, author],
        )?;
        Ok(())
    }

    async fn log_event(&self, issue_id: &str, event_type: &str, detail: &str) {
        let conn = self.conn.lock().unwrap();
        let result = conn.execute(
            "INSERT INTO events (issue_id, event_type, actor, comment, created_at) VALUES (?1, ?2, 'rosary', ?3, datetime('now'))",
            params![issue_id, event_type, detail],
        );
        if let Err(e) = result {
            eprintln!("warning: failed to log event for {issue_id}: {e}");
        }
    }

    async fn get_latest_event(&self, issue_id: &str, event_type: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let result = conn
            .query_row(
                "SELECT comment FROM events WHERE issue_id = ?1 AND event_type = ?2 ORDER BY created_at DESC LIMIT 1",
                params![issue_id, event_type],
                |row| row.get(0),
            )
            .optional()?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> SqliteBeadStore {
        SqliteBeadStore::connect(Path::new(":memory:")).unwrap()
    }

    #[tokio::test]
    async fn create_and_get_bead() {
        let store = test_store();
        store
            .create_bead("test-1", "Test bead", "A description", 2, "task")
            .await
            .unwrap();

        let bead = store.get_bead("test-1", "rosary").await.unwrap().unwrap();
        assert_eq!(bead.id, "test-1");
        assert_eq!(bead.title, "Test bead");
        assert_eq!(bead.status, "open");
        assert_eq!(bead.priority, 2);
    }

    #[tokio::test]
    async fn list_beads_excludes_closed() {
        let store = test_store();
        store.create_bead("a", "Open", "", 1, "task").await.unwrap();
        store
            .create_bead("b", "Closed", "", 2, "task")
            .await
            .unwrap();
        store.close_bead("b").await.unwrap();

        let beads = store.list_beads("repo").await.unwrap();
        assert_eq!(beads.len(), 1);
        assert_eq!(beads[0].id, "a");
    }

    #[tokio::test]
    async fn create_bead_full_with_deps() {
        let store = test_store();
        store
            .create_bead("dep-1", "Dep", "", 1, "task")
            .await
            .unwrap();
        store
            .create_bead_full(
                "main-1",
                "Main",
                "desc",
                1,
                "feature",
                "agent",
                &["src/main.rs".into()],
                &["src/main_test.rs".into()],
                &["dep-1".into()],
            )
            .await
            .unwrap();

        let bead = store.get_bead("main-1", "repo").await.unwrap().unwrap();
        assert_eq!(bead.owner.as_deref(), Some("agent"));
        assert_eq!(bead.files, vec!["src/main.rs"]);
        assert_eq!(bead.test_files, vec!["src/main_test.rs"]);

        let deps = store.get_dependencies("main-1").await.unwrap();
        assert_eq!(deps, vec!["dep-1"]);
    }

    #[tokio::test]
    async fn search_beads_by_title() {
        let store = test_store();
        store
            .create_bead("a", "Fix dispatch bug", "", 1, "bug")
            .await
            .unwrap();
        store
            .create_bead("b", "Add feature X", "", 2, "feature")
            .await
            .unwrap();

        let results = store.search_beads("dispatch", "repo", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "a");
    }

    #[tokio::test]
    async fn update_status_and_close() {
        let store = test_store();
        store.create_bead("x", "Test", "", 1, "task").await.unwrap();

        store.update_status("x", "dispatched").await.unwrap();
        assert_eq!(
            store.get_status("x").await.unwrap().as_deref(),
            Some("dispatched")
        );

        store.close_bead("x").await.unwrap();
        assert_eq!(
            store.get_status("x").await.unwrap().as_deref(),
            Some("closed")
        );
    }

    #[tokio::test]
    async fn comments_and_events() {
        let store = test_store();
        store.create_bead("c", "Test", "", 1, "task").await.unwrap();

        store
            .add_comment("c", "progress note", "dev-agent")
            .await
            .unwrap();
        store.log_event("c", "dispatched", "agent started").await;

        let event = store.get_latest_event("c", "dispatched").await.unwrap();
        assert_eq!(event.as_deref(), Some("agent started"));
    }

    #[tokio::test]
    async fn external_ref_roundtrip() {
        let store = test_store();
        store.create_bead("e", "Test", "", 1, "task").await.unwrap();
        store.set_external_ref("e", "AGE-42").await.unwrap();

        assert_eq!(
            store.get_external_ref("e").await.unwrap().as_deref(),
            Some("AGE-42")
        );
        assert_eq!(
            store
                .find_by_external_ref("AGE-42")
                .await
                .unwrap()
                .as_deref(),
            Some("e")
        );
    }

    #[tokio::test]
    async fn dependency_lifecycle() {
        let store = test_store();
        store.create_bead("a", "A", "", 1, "task").await.unwrap();
        store.create_bead("b", "B", "", 1, "task").await.unwrap();

        store.add_dependency("b", "a").await.unwrap();
        assert_eq!(store.get_dependencies("b").await.unwrap(), vec!["a"]);
        assert_eq!(store.get_dependents("a").await.unwrap(), vec!["b"]);

        store.remove_dependency("b", "a").await.unwrap();
        assert!(store.get_dependencies("b").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn update_bead_fields() {
        let store = test_store();
        store
            .create_bead("u", "Original", "", 2, "task")
            .await
            .unwrap();

        let update = BeadUpdate {
            title: Some("Updated".into()),
            priority: Some(1),
            ..Default::default()
        };
        let fields = store.update_bead_fields("u", &update).await.unwrap();
        assert!(fields.contains(&"title".to_string()));
        assert!(fields.contains(&"priority".to_string()));

        let bead = store.get_bead("u", "repo").await.unwrap().unwrap();
        assert_eq!(bead.title, "Updated");
        assert_eq!(bead.priority, 1);
    }
}
