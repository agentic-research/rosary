//! SQLite backend for rosary orchestrator state.
//!
//! Replaces DoltBackend for orchestrator state (pipeline phases, dispatch
//! history, hierarchy). No server process needed — embedded SQLite.
//! Beads themselves stay in per-repo Dolt databases.
//!
//! Design: rosary-45518d (CRDT-lattice bead state).

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::store::*;

/// SQLite-backed orchestrator store. Thread-safe via Mutex.
pub struct SqliteBackend {
    conn: Mutex<Connection>,
    path: PathBuf,
}

impl SqliteBackend {
    /// Open or create the SQLite database at the given path.
    /// Creates tables if they don't exist.
    pub fn connect(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).context("opening sqlite backend")?;

        // WAL mode for concurrent reads + single writer
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;

        // Create tables
        conn.execute_batch(SCHEMA)?;

        // Additive migrations — safe to run on every connect (IF NOT EXISTS / column-exists guard)
        let _ = conn.execute_batch("ALTER TABLE dispatches ADD COLUMN chain_hash TEXT;");
        let _ = conn
            .execute_batch("ALTER TABLE thread_members ADD COLUMN scope TEXT NOT NULL DEFAULT '';");
        // ADR-0009: modal evidence tier on cross-repo deps
        let _ = conn.execute_batch(
            "ALTER TABLE dependencies ADD COLUMN evidence_tier TEXT NOT NULL DEFAULT 'asserted';",
        );
        let _ = conn.execute_batch(
            "ALTER TABLE dependencies ADD COLUMN source TEXT NOT NULL DEFAULT 'human';",
        );
        let _ = conn.execute_batch(
            "ALTER TABLE dependencies ADD COLUMN observed_at TEXT NOT NULL DEFAULT (datetime('now'));",
        );

        Ok(SqliteBackend {
            conn: Mutex::new(conn),
            path: path.to_path_buf(),
        })
    }

    /// Path to the SQLite file.
    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS decades (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    source_path TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'proposed'
);

CREATE TABLE IF NOT EXISTS threads (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    decade_id TEXT NOT NULL,
    feature_branch TEXT,
    FOREIGN KEY (decade_id) REFERENCES decades(id)
);

CREATE TABLE IF NOT EXISTS thread_members (
    thread_id TEXT NOT NULL,
    repo TEXT NOT NULL,
    bead_id TEXT NOT NULL,
    scope TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (thread_id, repo, bead_id)
);

CREATE TABLE IF NOT EXISTS pipelines (
    repo TEXT NOT NULL,
    bead_id TEXT NOT NULL,
    pipeline_phase INTEGER NOT NULL DEFAULT 0,
    pipeline_agent TEXT NOT NULL DEFAULT 'dev-agent',
    phase_status TEXT NOT NULL DEFAULT 'pending',
    retries INTEGER NOT NULL DEFAULT 0,
    consecutive_reverts INTEGER NOT NULL DEFAULT 0,
    highest_verify_tier INTEGER,
    last_generation INTEGER NOT NULL DEFAULT 0,
    backoff_until TEXT,
    PRIMARY KEY (repo, bead_id)
);

CREATE TABLE IF NOT EXISTS dispatches (
    id TEXT PRIMARY KEY,
    repo TEXT NOT NULL,
    bead_id TEXT NOT NULL,
    agent TEXT NOT NULL,
    provider TEXT NOT NULL,
    started_at TEXT NOT NULL,
    completed_at TEXT,
    outcome TEXT,
    work_dir TEXT NOT NULL DEFAULT '',
    session_id TEXT,
    workspace_path TEXT,
    chain_hash TEXT
);

CREATE TABLE IF NOT EXISTS dependencies (
    from_repo TEXT NOT NULL,
    from_bead TEXT NOT NULL,
    to_repo TEXT NOT NULL,
    to_bead TEXT NOT NULL,
    dep_type TEXT NOT NULL DEFAULT 'blocks',
    evidence_tier TEXT NOT NULL DEFAULT 'asserted',
    source TEXT NOT NULL DEFAULT 'human',
    observed_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (from_repo, from_bead, to_repo, to_bead)
);

CREATE TABLE IF NOT EXISTS linear_links (
    repo TEXT NOT NULL,
    bead_id TEXT NOT NULL,
    linear_id TEXT NOT NULL,
    linear_type TEXT NOT NULL DEFAULT 'issue',
    PRIMARY KEY (repo, bead_id)
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_linear_links_linear_id ON linear_links(linear_id);

CREATE TABLE IF NOT EXISTS user_repos (
    user_id TEXT NOT NULL,
    repo_url TEXT NOT NULL,
    repo_name TEXT NOT NULL,
    github_token_ref TEXT,
    PRIMARY KEY (user_id, repo_name)
);
";

// ── HierarchyStore ──────────────────────────────────────

#[async_trait]
impl HierarchyStore for SqliteBackend {
    async fn upsert_decade(&self, decade: &DecadeRecord) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO decades (id, title, source_path, status) VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(id) DO UPDATE SET title = excluded.title, source_path = excluded.source_path, status = excluded.status",
            params![decade.id, decade.title, decade.source_path, decade.status],
        )?;
        Ok(())
    }

    async fn get_decade(&self, id: &str) -> Result<Option<DecadeRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT id, title, source_path, status FROM decades WHERE id = ?1")?;
        let mut rows = stmt.query(params![id])?;
        Ok(rows.next()?.map(|row| DecadeRecord {
            id: row.get(0).unwrap(),
            title: row.get(1).unwrap(),
            source_path: row.get(2).unwrap(),
            status: row.get(3).unwrap(),
        }))
    }

    async fn list_decades(&self, status: Option<&str>) -> Result<Vec<DecadeRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut results = Vec::new();
        match status {
            Some(s) => {
                let mut stmt = conn.prepare(
                    "SELECT id, title, source_path, status FROM decades WHERE status = ?1",
                )?;
                let rows = stmt.query_map(params![s], |row| {
                    Ok(DecadeRecord {
                        id: row.get(0)?,
                        title: row.get(1)?,
                        source_path: row.get(2)?,
                        status: row.get(3)?,
                    })
                })?;
                for row in rows {
                    results.push(row?);
                }
            }
            None => {
                let mut stmt =
                    conn.prepare("SELECT id, title, source_path, status FROM decades")?;
                let rows = stmt.query_map([], |row| {
                    Ok(DecadeRecord {
                        id: row.get(0)?,
                        title: row.get(1)?,
                        source_path: row.get(2)?,
                        status: row.get(3)?,
                    })
                })?;
                for row in rows {
                    results.push(row?);
                }
            }
        }
        Ok(results)
    }

    async fn upsert_thread(&self, thread: &ThreadRecord) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO threads (id, name, decade_id, feature_branch) VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, decade_id = excluded.decade_id, feature_branch = excluded.feature_branch",
            params![thread.id, thread.name, thread.decade_id, thread.feature_branch],
        )?;
        Ok(())
    }

    async fn list_threads(&self, decade_id: &str) -> Result<Vec<ThreadRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, decade_id, feature_branch FROM threads WHERE decade_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![decade_id], |row| {
            Ok(ThreadRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                decade_id: row.get(2)?,
                feature_branch: row.get(3)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    async fn add_bead_to_thread(&self, thread_id: &str, bead: &WorkRef) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO thread_members (thread_id, repo, scope, bead_id) VALUES (?1, ?2, ?3, ?4)",
            params![thread_id, bead.repo, bead.scope, bead.bead_id],
        )?;
        Ok(())
    }

    async fn list_beads_in_thread(&self, thread_id: &str) -> Result<Vec<WorkRef>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT repo, scope, bead_id FROM thread_members WHERE thread_id = ?1 ORDER BY repo, bead_id",
        )?;
        let rows = stmt.query_map(params![thread_id], |row| {
            Ok(WorkRef {
                repo: row.get(0)?,
                scope: row.get::<_, String>(1).unwrap_or_default(),
                bead_id: row.get(2)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    async fn find_thread_for_bead(&self, bead: &WorkRef) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT thread_id FROM thread_members WHERE repo = ?1 AND bead_id = ?2 AND scope = ?3 LIMIT 1",
        )?;
        let mut rows = stmt.query(params![bead.repo, bead.bead_id, bead.scope])?;
        Ok(rows.next()?.map(|row| row.get(0).unwrap()))
    }
}

// ── DispatchStore ───────────────────────────────────────

#[async_trait]
impl DispatchStore for SqliteBackend {
    async fn upsert_pipeline(&self, state: &PipelineState) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let backoff = state.backoff_until.map(|dt| dt.to_rfc3339());
        conn.execute(
            "INSERT OR REPLACE INTO pipelines (repo, bead_id, pipeline_phase, pipeline_agent, phase_status, retries, consecutive_reverts, highest_verify_tier, last_generation, backoff_until)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                state.bead_ref.repo,
                state.bead_ref.bead_id,
                state.pipeline_phase,
                state.pipeline_agent,
                state.phase_status,
                state.retries,
                state.consecutive_reverts,
                state.highest_verify_tier,
                state.last_generation as i64,
                backoff,
            ],
        )?;
        Ok(())
    }

    async fn get_pipeline(&self, bead: &WorkRef) -> Result<Option<PipelineState>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT repo, bead_id, pipeline_phase, pipeline_agent, phase_status, retries, consecutive_reverts, highest_verify_tier, last_generation, backoff_until
             FROM pipelines WHERE repo = ?1 AND bead_id = ?2",
        )?;
        let mut rows = stmt.query(params![bead.repo, bead.bead_id])?;
        Ok(rows.next()?.map(|row| row_to_pipeline(row)).transpose()?)
    }

    async fn list_active_pipelines(&self) -> Result<Vec<PipelineState>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT repo, bead_id, pipeline_phase, pipeline_agent, phase_status, retries, consecutive_reverts, highest_verify_tier, last_generation, backoff_until
             FROM pipelines",
        )?;
        let rows = stmt.query_map([], row_to_pipeline)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    async fn clear_pipeline(&self, bead: &WorkRef) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM pipelines WHERE repo = ?1 AND bead_id = ?2",
            params![bead.repo, bead.bead_id],
        )?;
        Ok(())
    }

    async fn record_dispatch(&self, record: &DispatchRecord) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO dispatches (id, repo, bead_id, agent, provider, started_at, completed_at, outcome, work_dir, session_id, workspace_path, chain_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                record.id,
                record.bead_ref.repo,
                record.bead_ref.bead_id,
                record.agent,
                record.provider,
                record.started_at.to_rfc3339(),
                record.completed_at.map(|dt| dt.to_rfc3339()),
                record.outcome,
                record.work_dir,
                record.session_id,
                record.workspace_path,
                record.chain_hash,
            ],
        )?;
        Ok(())
    }

    async fn upsert_dispatch(&self, record: &DispatchRecord) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO dispatches (id, repo, bead_id, agent, provider, started_at, completed_at, outcome, work_dir, session_id, workspace_path, chain_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(id) DO UPDATE SET
                 completed_at = excluded.completed_at, outcome = excluded.outcome,
                 session_id = excluded.session_id, workspace_path = excluded.workspace_path",
            params![
                record.id,
                record.bead_ref.repo,
                record.bead_ref.bead_id,
                record.agent,
                record.provider,
                record.started_at.to_rfc3339(),
                record.completed_at.map(|dt| dt.to_rfc3339()),
                record.outcome,
                record.work_dir,
                record.session_id,
                record.workspace_path,
                record.chain_hash,
            ],
        )?;
        Ok(())
    }

    async fn complete_dispatch(&self, id: &str, outcome: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE dispatches SET completed_at = ?1, outcome = ?2 WHERE id = ?3",
            params![Utc::now().to_rfc3339(), outcome, id],
        )?;
        Ok(())
    }

    async fn update_dispatch_session(&self, id: &str, session_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE dispatches SET session_id = ?1 WHERE id = ?2",
            params![session_id, id],
        )?;
        Ok(())
    }

    async fn active_dispatches(&self) -> Result<Vec<DispatchRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, repo, bead_id, agent, provider, started_at, completed_at, outcome, work_dir, session_id, workspace_path, chain_hash
             FROM dispatches WHERE completed_at IS NULL",
        )?;
        let rows = stmt.query_map([], row_to_dispatch)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

// ── LinkageStore ────────────────────────────────────────

#[async_trait]
impl LinkageStore for SqliteBackend {
    async fn add_dependency(&self, dep: &CrossRepoDep) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        // Per-repo cycle check (ADR-0009)
        if dep.from.repo == dep.to.repo {
            let mut stmt = conn.prepare(
                "SELECT from_bead, to_bead FROM dependencies WHERE from_repo = ?1 AND to_repo = ?1",
            )?;
            let edges: Vec<(String, String)> = stmt
                .query_map(params![dep.from.repo], |r| Ok((r.get(0)?, r.get(1)?)))?
                .filter_map(|r| r.ok())
                .collect();
            if sqlite_dfs_can_reach(&edges, &dep.to.bead_id, &dep.from.bead_id) {
                anyhow::bail!(
                    "cycle: adding {}/{} → {}/{} would create a per-repo cycle",
                    dep.from.repo,
                    dep.from.bead_id,
                    dep.to.repo,
                    dep.to.bead_id
                );
            }
        }
        conn.execute(
            "INSERT OR REPLACE INTO dependencies
               (from_repo, from_bead, to_repo, to_bead, dep_type, evidence_tier, source, observed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'))",
            params![
                dep.from.repo,
                dep.from.bead_id,
                dep.to.repo,
                dep.to.bead_id,
                dep.dep_type,
                dep.evidence_tier.as_str(),
                dep.source
            ],
        )?;
        Ok(())
    }

    async fn dependencies_of(&self, bead: &WorkRef) -> Result<Vec<CrossRepoDep>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT from_repo, from_bead, to_repo, to_bead, dep_type, evidence_tier, source, observed_at
             FROM dependencies WHERE from_repo = ?1 AND from_bead = ?2",
        )?;
        let rows = stmt.query_map(params![bead.repo, bead.bead_id], sqlite_row_to_dep)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    async fn dependents_of(&self, bead: &WorkRef) -> Result<Vec<CrossRepoDep>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT from_repo, from_bead, to_repo, to_bead, dep_type, evidence_tier, source, observed_at
             FROM dependencies WHERE to_repo = ?1 AND to_bead = ?2",
        )?;
        let rows = stmt.query_map(params![bead.repo, bead.bead_id], sqlite_row_to_dep)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    async fn all_blocking_deps(&self) -> Result<Vec<CrossRepoDep>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT from_repo, from_bead, to_repo, to_bead, dep_type, evidence_tier, source, observed_at
             FROM dependencies WHERE dep_type = 'blocks' AND evidence_tier IN ('asserted', 'derived')",
        )?;
        let rows = stmt.query_map([], sqlite_row_to_dep)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    async fn remove_cross_repo_dep(&self, from: &WorkRef, to: &WorkRef) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM dependencies WHERE from_repo = ?1 AND from_bead = ?2 AND to_repo = ?3 AND to_bead = ?4",
            params![from.repo, from.bead_id, to.repo, to.bead_id],
        )?;
        Ok(())
    }

    async fn demote_stale_derived(&self, ttl_days: u32) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "UPDATE dependencies SET evidence_tier = 'conjectured'
             WHERE evidence_tier = 'derived'
               AND observed_at < datetime('now', ?1)",
            params![format!("-{ttl_days} days")],
        )?;
        Ok(n as u64)
    }

    async fn upsert_linear_link(&self, link: &LinearLink) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO linear_links (repo, bead_id, linear_id, linear_type) VALUES (?1, ?2, ?3, ?4)",
            params![link.bead_ref.repo, link.bead_ref.bead_id, link.linear_id, link.linear_type],
        )?;
        Ok(())
    }

    async fn find_by_linear_id(&self, linear_id: &str) -> Result<Option<LinearLink>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT repo, bead_id, linear_id, linear_type FROM linear_links WHERE linear_id = ?1",
        )?;
        let mut rows = stmt.query(params![linear_id])?;
        Ok(rows.next()?.map(|row| LinearLink {
            bead_ref: WorkRef {
                repo: row.get(0).unwrap(),
                scope: String::new(),
                bead_id: row.get(1).unwrap(),
            },
            linear_id: row.get(2).unwrap(),
            linear_type: row.get(3).unwrap(),
        }))
    }
}

// ── UserRepoStore ───────────────────────────────────────

#[async_trait]
impl UserRepoStore for SqliteBackend {
    async fn register_repo(&self, repo: &UserRepo) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO user_repos (user_id, repo_url, repo_name, github_token_ref) VALUES (?1, ?2, ?3, ?4)",
            params![repo.user_id, repo.repo_url, repo.repo_name, repo.github_token_ref],
        )?;
        Ok(())
    }

    async fn list_user_repos(&self, user_id: &str) -> Result<Vec<UserRepo>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT user_id, repo_url, repo_name, github_token_ref FROM user_repos WHERE user_id = ?1")?;
        let rows = stmt.query_map(params![user_id], |row| {
            Ok(UserRepo {
                user_id: row.get(0)?,
                repo_url: row.get(1)?,
                repo_name: row.get(2)?,
                github_token_ref: row.get(3)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    async fn unregister_repo(&self, user_id: &str, repo_name: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM user_repos WHERE user_id = ?1 AND repo_name = ?2",
            params![user_id, repo_name],
        )?;
        Ok(())
    }
}

// ── BackendExport ──────────────────────────────────────

#[async_trait]
impl BackendExport for SqliteBackend {
    async fn all_threads(&self) -> Result<Vec<ThreadRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT id, name, decade_id, feature_branch FROM threads ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            Ok(ThreadRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                decade_id: row.get(2)?,
                feature_branch: row.get(3)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    async fn all_thread_members(&self) -> Result<Vec<(String, WorkRef)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT thread_id, repo, scope, bead_id FROM thread_members ORDER BY thread_id, repo, bead_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                WorkRef {
                    repo: row.get(1)?,
                    scope: row.get::<_, String>(2).unwrap_or_default(),
                    bead_id: row.get(3)?,
                },
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    async fn all_dispatches(&self) -> Result<Vec<DispatchRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, repo, bead_id, agent, provider, started_at, completed_at, outcome, work_dir, session_id, workspace_path, chain_hash
             FROM dispatches ORDER BY started_at ASC",
        )?;
        let rows = stmt.query_map([], row_to_dispatch)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    async fn all_dependencies(&self) -> Result<Vec<CrossRepoDep>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT from_repo, from_bead, to_repo, to_bead, dep_type, evidence_tier, source, observed_at FROM dependencies",
        )?;
        let rows = stmt.query_map([], sqlite_row_to_dep)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    async fn all_linear_links(&self) -> Result<Vec<LinearLink>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT repo, bead_id, linear_id, linear_type FROM linear_links")?;
        let rows = stmt.query_map([], |row| {
            Ok(LinearLink {
                bead_ref: WorkRef {
                    repo: row.get(0)?,
                    scope: String::new(),
                    bead_id: row.get(1)?,
                },
                linear_id: row.get(2)?,
                linear_type: row.get(3)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    async fn all_user_repos(&self) -> Result<Vec<UserRepo>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT user_id, repo_url, repo_name, github_token_ref FROM user_repos")?;
        let rows = stmt.query_map([], |row| {
            Ok(UserRepo {
                user_id: row.get(0)?,
                repo_url: row.get(1)?,
                repo_name: row.get(2)?,
                github_token_ref: row.get(3)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

// ── Row helpers ─────────────────────────────────────────

fn sqlite_row_to_dep(row: &rusqlite::Row) -> rusqlite::Result<CrossRepoDep> {
    let tier_str: String = row.get(5).unwrap_or_else(|_| "asserted".into());
    let observed_str: String = row.get(7).unwrap_or_else(|_| "1970-01-01T00:00:00".into());
    let observed_at = chrono::DateTime::parse_from_rfc3339(&observed_str)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::Utc::now());
    Ok(CrossRepoDep {
        from: WorkRef {
            repo: row.get(0)?,
            scope: String::new(),
            bead_id: row.get(1)?,
        },
        to: WorkRef {
            repo: row.get(2)?,
            scope: String::new(),
            bead_id: row.get(3)?,
        },
        dep_type: row.get(4)?,
        evidence_tier: tier_str.parse().unwrap_or(EvidenceTier::Asserted),
        source: row.get(6).unwrap_or_else(|_| "human".into()),
        observed_at,
    })
}

fn sqlite_dfs_can_reach(edges: &[(String, String)], start: &str, target: &str) -> bool {
    use std::collections::HashSet;
    let mut visited = HashSet::new();
    let mut stack = vec![start.to_string()];
    while let Some(node) = stack.pop() {
        if node == target {
            return true;
        }
        if !visited.insert(node.clone()) {
            continue;
        }
        for (from, to) in edges {
            if from == &node {
                stack.push(to.clone());
            }
        }
    }
    false
}

fn row_to_pipeline(row: &rusqlite::Row) -> rusqlite::Result<PipelineState> {
    let backoff_str: Option<String> = row.get(9)?;
    Ok(PipelineState {
        bead_ref: WorkRef {
            repo: row.get(0)?,
            scope: String::new(),
            bead_id: row.get(1)?,
        },
        pipeline_phase: row.get::<_, u8>(2)?,
        pipeline_agent: row.get(3)?,
        phase_status: row.get(4)?,
        retries: row.get(5)?,
        consecutive_reverts: row.get(6)?,
        highest_verify_tier: row.get(7)?,
        last_generation: row.get::<_, i64>(8)? as u64,
        backoff_until: backoff_str.and_then(|s| {
            DateTime::parse_from_rfc3339(&s)
                .ok()
                .map(|dt| dt.with_timezone(&Utc))
        }),
    })
}

fn row_to_dispatch(row: &rusqlite::Row) -> rusqlite::Result<DispatchRecord> {
    let started_str: String = row.get(5)?;
    let completed_str: Option<String> = row.get(6)?;
    Ok(DispatchRecord {
        id: row.get(0)?,
        bead_ref: WorkRef {
            repo: row.get(1)?,
            scope: String::new(),
            bead_id: row.get(2)?,
        },
        agent: row.get(3)?,
        provider: row.get(4)?,
        started_at: DateTime::parse_from_rfc3339(&started_str)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    5,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
        completed_at: completed_str.and_then(|s| {
            DateTime::parse_from_rfc3339(&s)
                .ok()
                .map(|dt| dt.with_timezone(&Utc))
        }),
        outcome: row.get(7)?,
        work_dir: row.get(8)?,
        session_id: row.get(9)?,
        workspace_path: row.get(10)?,
        chain_hash: row.get(11)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_backend() -> (SqliteBackend, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let backend = SqliteBackend::connect(&dir.path().join("test.db")).unwrap();
        (backend, dir)
    }

    #[tokio::test]
    async fn pipeline_crud() {
        let (store, _dir) = temp_backend();
        let bead = WorkRef {
            repo: "rosary".into(),
            bead_id: "rsry-001".into(),
            scope: String::new(),
        };
        let state = PipelineState {
            bead_ref: bead.clone(),
            pipeline_phase: 0,
            pipeline_agent: "dev-agent".into(),
            phase_status: "executing".into(),
            retries: 0,
            consecutive_reverts: 0,
            highest_verify_tier: None,
            last_generation: 42,
            backoff_until: None,
        };

        store.upsert_pipeline(&state).await.unwrap();
        let got = store.get_pipeline(&bead).await.unwrap().unwrap();
        assert_eq!(got.pipeline_phase, 0);
        assert_eq!(got.last_generation, 42);

        store.clear_pipeline(&bead).await.unwrap();
        assert!(store.get_pipeline(&bead).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn dispatch_lifecycle() {
        let (store, _dir) = temp_backend();
        let record = DispatchRecord {
            id: "d-001".into(),
            bead_ref: WorkRef {
                repo: "rosary".into(),
                bead_id: "rsry-001".into(),
                scope: String::new(),
            },
            agent: "dev-agent".into(),
            provider: "claude".into(),
            started_at: Utc::now(),
            completed_at: None,
            outcome: None,
            work_dir: "/tmp/work".into(),
            session_id: None,
            workspace_path: None,
            chain_hash: None,
        };

        store.record_dispatch(&record).await.unwrap();
        let active = store.active_dispatches().await.unwrap();
        assert_eq!(active.len(), 1);

        store.complete_dispatch("d-001", "success").await.unwrap();
        assert!(store.active_dispatches().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn hierarchy_crud() {
        let (store, _dir) = temp_backend();
        let decade = DecadeRecord {
            id: "ADR-001".into(),
            title: "Test".into(),
            source_path: "test.md".into(),
            status: "active".into(),
        };
        store.upsert_decade(&decade).await.unwrap();
        assert_eq!(
            store.get_decade("ADR-001").await.unwrap().unwrap().title,
            "Test"
        );

        let thread = ThreadRecord {
            id: "ADR-001/impl".into(),
            name: "Implementation".into(),
            decade_id: "ADR-001".into(),
            feature_branch: None,
        };
        store.upsert_thread(&thread).await.unwrap();
        assert_eq!(store.list_threads("ADR-001").await.unwrap().len(), 1);

        let bead = WorkRef {
            repo: "rosary".into(),
            bead_id: "rsry-001".into(),
            scope: String::new(),
        };
        store
            .add_bead_to_thread("ADR-001/impl", &bead)
            .await
            .unwrap();
        assert_eq!(
            store.find_thread_for_bead(&bead).await.unwrap(),
            Some("ADR-001/impl".into())
        );
    }

    #[tokio::test]
    async fn reparent_thread_moves_decade() {
        // End-to-end test for store::reparent_thread() using the SQLite backend.
        // Catches the regression where Dolt upsert_thread silently dropped
        // decade_id updates (Copilot review on PR #183).
        let (store, _dir) = temp_backend();

        // Two decades, one thread parked under the first.
        for id in ["ungrouped", "agentic-provenance"] {
            store
                .upsert_decade(&DecadeRecord {
                    id: id.into(),
                    title: id.into(),
                    source_path: String::new(),
                    status: "active".into(),
                })
                .await
                .unwrap();
        }
        store
            .upsert_thread(&ThreadRecord {
                id: "agentic-provenance/agent-identity".into(),
                name: "Agent identity thread".into(),
                decade_id: "ungrouped".into(),
                feature_branch: Some("rosary/agentic-provenance-agent-identity".into()),
            })
            .await
            .unwrap();

        // Re-parent into agentic-provenance.
        crate::store::reparent_thread(
            &store,
            "agentic-provenance/agent-identity",
            "agentic-provenance",
            None,
        )
        .await
        .unwrap();

        // Old decade no longer lists the thread; new decade does.
        assert!(
            store
                .list_threads("ungrouped")
                .await
                .unwrap()
                .iter()
                .all(|t| t.id != "agentic-provenance/agent-identity"),
            "ungrouped must no longer contain the moved thread"
        );
        let new = store.list_threads("agentic-provenance").await.unwrap();
        assert!(
            new.iter()
                .any(|t| t.id == "agentic-provenance/agent-identity"),
            "agentic-provenance must contain the moved thread"
        );

        // Optional rename: provide new_name.
        crate::store::reparent_thread(
            &store,
            "agentic-provenance/agent-identity",
            "agentic-provenance",
            Some("Renamed thread"),
        )
        .await
        .unwrap();
        let renamed = store.list_threads("agentic-provenance").await.unwrap();
        let t = renamed
            .iter()
            .find(|t| t.id == "agentic-provenance/agent-identity")
            .unwrap();
        assert_eq!(t.name, "Renamed thread");
    }

    #[tokio::test]
    async fn reparent_thread_auto_creates_target_decade() {
        let (store, _dir) = temp_backend();
        store
            .upsert_decade(&DecadeRecord {
                id: "src".into(),
                title: "src".into(),
                source_path: String::new(),
                status: "active".into(),
            })
            .await
            .unwrap();
        store
            .upsert_thread(&ThreadRecord {
                id: "x".into(),
                name: "X".into(),
                decade_id: "src".into(),
                feature_branch: None,
            })
            .await
            .unwrap();

        // Target decade doesn't exist yet — helper should create it.
        crate::store::reparent_thread(&store, "x", "freshly-created", None)
            .await
            .unwrap();
        assert!(
            store.get_decade("freshly-created").await.unwrap().is_some(),
            "missing target decade must be auto-created"
        );
    }

    #[tokio::test]
    async fn reparent_thread_errors_on_missing_thread() {
        let (store, _dir) = temp_backend();
        let err = crate::store::reparent_thread(&store, "nonexistent", "wherever", None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("thread not found"));
    }

    #[tokio::test]
    async fn linkage_crud() {
        let (store, _dir) = temp_backend();
        let from = WorkRef {
            repo: "rosary".into(),
            bead_id: "rsry-001".into(),
            scope: String::new(),
        };
        let to = WorkRef {
            repo: "mache".into(),
            bead_id: "mch-001".into(),
            scope: String::new(),
        };

        store
            .add_dependency(&CrossRepoDep {
                from: from.clone(),
                to: to.clone(),
                dep_type: "blocks".into(),
                evidence_tier: EvidenceTier::Asserted,
                source: "human".into(),
                observed_at: chrono::Utc::now(),
            })
            .await
            .unwrap();
        assert_eq!(store.dependencies_of(&from).await.unwrap().len(), 1);
        assert_eq!(store.dependents_of(&to).await.unwrap().len(), 1);

        let link = LinearLink {
            bead_ref: from,
            linear_id: "AGE-330".into(),
            linear_type: "issue".into(),
        };
        store.upsert_linear_link(&link).await.unwrap();
        assert_eq!(
            store
                .find_by_linear_id("AGE-330")
                .await
                .unwrap()
                .unwrap()
                .linear_type,
            "issue"
        );
    }

    #[tokio::test]
    async fn list_decades_with_filter() {
        let (store, _dir) = temp_backend();
        store
            .upsert_decade(&DecadeRecord {
                id: "ADR-001".into(),
                title: "A".into(),
                source_path: "a.md".into(),
                status: "active".into(),
            })
            .await
            .unwrap();
        store
            .upsert_decade(&DecadeRecord {
                id: "ADR-002".into(),
                title: "B".into(),
                source_path: "b.md".into(),
                status: "proposed".into(),
            })
            .await
            .unwrap();

        assert_eq!(store.list_decades(None).await.unwrap().len(), 2);
        assert_eq!(store.list_decades(Some("active")).await.unwrap().len(), 1);
        assert!(store.list_decades(Some("nope")).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn upsert_decade_preserves_threads() {
        let (store, _dir) = temp_backend();
        let decade = DecadeRecord {
            id: "ADR-001".into(),
            title: "Original".into(),
            source_path: "a.md".into(),
            status: "proposed".into(),
        };
        store.upsert_decade(&decade).await.unwrap();

        let thread = ThreadRecord {
            id: "ADR-001/impl".into(),
            name: "Impl".into(),
            decade_id: "ADR-001".into(),
            feature_branch: None,
        };
        store.upsert_thread(&thread).await.unwrap();

        // Update decade — threads must survive (ON CONFLICT, not REPLACE)
        let updated = DecadeRecord {
            id: "ADR-001".into(),
            title: "Updated".into(),
            source_path: "a.md".into(),
            status: "active".into(),
        };
        store.upsert_decade(&updated).await.unwrap();

        let threads = store.list_threads("ADR-001").await.unwrap();
        assert_eq!(threads.len(), 1, "thread must survive decade upsert");
        assert_eq!(
            store.get_decade("ADR-001").await.unwrap().unwrap().title,
            "Updated"
        );
    }

    #[tokio::test]
    async fn list_active_pipelines_multi() {
        let (store, _dir) = temp_backend();
        for i in 0..3 {
            store
                .upsert_pipeline(&PipelineState {
                    bead_ref: WorkRef {
                        repo: "rosary".into(),
                        bead_id: format!("rsry-{i:03}"),
                        scope: String::new(),
                    },
                    pipeline_phase: 0,
                    pipeline_agent: "dev-agent".into(),
                    phase_status: "executing".into(),
                    retries: 0,
                    consecutive_reverts: 0,
                    highest_verify_tier: None,
                    last_generation: 0,
                    backoff_until: None,
                })
                .await
                .unwrap();
        }
        assert_eq!(store.list_active_pipelines().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn pipeline_backoff_roundtrip() {
        let (store, _dir) = temp_backend();
        let now = Utc::now();
        let bead = WorkRef {
            repo: "rosary".into(),
            bead_id: "rsry-bo".into(),
            scope: String::new(),
        };
        store
            .upsert_pipeline(&PipelineState {
                bead_ref: bead.clone(),
                pipeline_phase: 0,
                pipeline_agent: "dev-agent".into(),
                phase_status: "backoff".into(),
                retries: 2,
                consecutive_reverts: 0,
                highest_verify_tier: None,
                last_generation: 0,
                backoff_until: Some(now),
            })
            .await
            .unwrap();
        let got = store.get_pipeline(&bead).await.unwrap().unwrap();
        assert!(got.backoff_until.is_some());
        // RFC3339 roundtrip loses sub-second precision, so check within 1s
        let diff = (got.backoff_until.unwrap() - now).num_seconds().abs();
        assert!(diff <= 1, "backoff_until roundtrip drift: {diff}s");
    }

    #[tokio::test]
    async fn dispatch_update_session() {
        let (store, _dir) = temp_backend();
        let record = DispatchRecord {
            id: "d-sess".into(),
            bead_ref: WorkRef {
                repo: "rosary".into(),
                bead_id: "rsry-001".into(),
                scope: String::new(),
            },
            agent: "dev-agent".into(),
            provider: "claude".into(),
            started_at: Utc::now(),
            completed_at: None,
            outcome: None,
            work_dir: "/tmp/work".into(),
            session_id: None,
            workspace_path: None,
            chain_hash: None,
        };
        store.record_dispatch(&record).await.unwrap();

        store
            .update_dispatch_session("d-sess", "sess-abc-123")
            .await
            .unwrap();

        let active = store.active_dispatches().await.unwrap();
        assert_eq!(active[0].session_id.as_deref(), Some("sess-abc-123"));
    }

    #[tokio::test]
    async fn user_repo_crud() {
        let (store, _dir) = temp_backend();
        let repo = UserRepo {
            user_id: "user-1".into(),
            repo_url: "https://github.com/example/repo".into(),
            repo_name: "repo".into(),
            github_token_ref: Some("kv:tok-abc".into()),
        };

        store.register_repo(&repo).await.unwrap();
        let repos = store.list_user_repos("user-1").await.unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].repo_name, "repo");
        assert_eq!(repos[0].github_token_ref.as_deref(), Some("kv:tok-abc"));

        // Other user sees nothing
        assert!(store.list_user_repos("user-2").await.unwrap().is_empty());

        // Unregister
        store.unregister_repo("user-1", "repo").await.unwrap();
        assert!(store.list_user_repos("user-1").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn user_repo_upsert_overwrites() {
        let (store, _dir) = temp_backend();
        let repo = UserRepo {
            user_id: "user-1".into(),
            repo_url: "https://github.com/example/repo".into(),
            repo_name: "repo".into(),
            github_token_ref: Some("old-tok".into()),
        };
        store.register_repo(&repo).await.unwrap();

        // Re-register with new token
        let updated = UserRepo {
            github_token_ref: Some("new-tok".into()),
            ..repo
        };
        store.register_repo(&updated).await.unwrap();

        let repos = store.list_user_repos("user-1").await.unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].github_token_ref.as_deref(), Some("new-tok"));
    }

    /// Regression (GAP 4): when a reconciler crashes between record_dispatch() and
    /// complete_dispatch(), the dispatch record is left with completed_at=NULL.
    /// On the next startup, recover_stuck_beads calls complete_dispatch("abandoned")
    /// on each orphaned record. This test verifies that "abandoned" records are
    /// no longer returned by active_dispatches(), which is the query the recovery
    /// loop uses to detect orphans.
    #[tokio::test]
    async fn abandoned_dispatch_not_in_active_dispatches() {
        let (store, _dir) = temp_backend();

        // Simulate: previous reconciler created a dispatch record and crashed
        let record = DispatchRecord {
            id: "orphan-001".into(),
            bead_ref: WorkRef {
                repo: "rosary".into(),
                bead_id: "rsry-crashed".into(),
                scope: String::new(),
            },
            agent: "dev-agent".into(),
            provider: "claude".into(),
            started_at: Utc::now(),
            completed_at: None, // crash — never completed
            outcome: None,
            work_dir: "/tmp/work".into(),
            session_id: None,
            workspace_path: None,
            chain_hash: None,
        };
        store.record_dispatch(&record).await.unwrap();

        // Before recovery: appears as orphan
        let orphans = store.active_dispatches().await.unwrap();
        assert_eq!(
            orphans.len(),
            1,
            "orphaned record must appear in active_dispatches"
        );

        // Recovery action: mark abandoned
        store
            .complete_dispatch("orphan-001", "abandoned")
            .await
            .unwrap();

        // After recovery: no longer orphaned
        let orphans = store.active_dispatches().await.unwrap();
        assert!(
            orphans.is_empty(),
            "abandoned dispatch must not appear in active_dispatches (GAP 4 regression)"
        );
    }
}
