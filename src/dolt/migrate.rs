//! Schema migrations for beads databases.
//!
//! Migrations are versioned SQL statements applied in order. Each migration
//! runs exactly once per database (tracked via an event with type "migration").
//! Designed for Dolt's `CREATE TABLE IF NOT EXISTS` pattern — migrations
//! handle ALTER TABLE and new columns that can't use IF NOT EXISTS.

use anyhow::{Context, Result};
use sqlx_core::query::query;
use sqlx_core::row::Row;

use super::DoltClient;

/// A schema migration with a unique version tag and SQL to execute.
struct Migration {
    /// Unique version string (e.g., "001_add_user_id"). Must be stable.
    version: &'static str,
    /// SQL statement(s) to execute. Use `;` to separate multiple statements.
    sql: &'static str,
    /// Human-readable description for logging.
    description: &'static str,
}

/// All migrations in order. Append new ones at the end — never reorder or remove.
const MIGRATIONS: &[Migration] = &[
    Migration {
        version: "001_add_user_id",
        sql: "ALTER TABLE issues ADD COLUMN user_id VARCHAR(128) DEFAULT NULL",
        description: "Add user_id column for multi-tenant scoping",
    },
    Migration {
        version: "002_observations",
        sql: "CREATE TABLE IF NOT EXISTS observations (
            bead_id VARCHAR(128) NOT NULL,
            agent VARCHAR(128) NOT NULL,
            phase INT NOT NULL DEFAULT 0,
            verdict VARCHAR(32) NOT NULL,
            detail TEXT DEFAULT '',
            content_hash VARCHAR(64) DEFAULT '',
            created_at DATETIME NOT NULL,
            INDEX idx_bead_id (bead_id),
            INDEX idx_verdict (verdict)
        )",
        description: "Append-only agent observations for CRDT-lattice bead state (rosary-45518d)",
    },
];

impl DoltClient {
    /// Run all pending migrations on this database.
    /// Idempotent — skips already-applied migrations.
    pub async fn migrate(&self) -> Result<Vec<String>> {
        let mut applied = Vec::new();

        for migration in MIGRATIONS {
            if self.migration_applied(migration.version).await? {
                continue;
            }

            eprintln!(
                "[migrate] applying {} — {}",
                migration.version, migration.description
            );

            // Execute migration SQL (may be multiple statements separated by ;)
            for stmt in migration
                .sql
                .split(';')
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                if let Err(e) = query(stmt).execute(&self.pool).await {
                    // Dolt may already have the column from a previous partial run
                    let err_str = e.to_string();
                    if err_str.contains("duplicate column") || err_str.contains("already exists") {
                        eprintln!(
                            "[migrate] {}: already applied (idempotent)",
                            migration.version
                        );
                    } else {
                        return Err(e).with_context(|| {
                            format!("migration {} failed: {stmt}", migration.version)
                        });
                    }
                }
            }

            // Record migration in events table
            self.log_event("_schema", "migration", migration.version)
                .await;

            applied.push(migration.version.to_string());
            eprintln!("[migrate] applied {}", migration.version);
        }

        Ok(applied)
    }

    /// Check if a migration has already been applied.
    async fn migration_applied(&self, version: &str) -> Result<bool> {
        let row = query(
            "SELECT COUNT(*) as cnt FROM events WHERE event_type = 'migration' AND comment = ?",
        )
        .bind(version)
        .fetch_one(&self.pool)
        .await
        .context("checking migration status")?;

        let count: i64 = row.try_get("cnt").unwrap_or(0);
        Ok(count > 0)
    }
}
