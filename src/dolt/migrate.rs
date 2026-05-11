#![allow(dead_code)] // Dolt migrations — kept for repos still using Dolt backend
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
    /// Optional SELECT that must succeed after apply.
    /// Catches partial applies (migration marked done but schema still wrong).
    verify: Option<&'static str>,
}

/// All migrations in order. Append new ones at the end — never reorder or remove.
const MIGRATIONS: &[Migration] = &[
    Migration {
        version: "001_add_user_id",
        sql: "ALTER TABLE issues ADD COLUMN user_id VARCHAR(128) DEFAULT NULL",
        description: "Add user_id column for multi-tenant scoping",
        verify: None,
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
        verify: None,
    },
    Migration {
        version: "003_add_created_by",
        sql: "ALTER TABLE issues ADD COLUMN created_by VARCHAR(255) DEFAULT NULL",
        description: "Git username of bead creator captured at creation time",
        verify: None,
    },
    Migration {
        version: "004_add_scope",
        sql: "ALTER TABLE issues ADD COLUMN scope VARCHAR(255) NOT NULL DEFAULT ''",
        description: "Team/folder scope for monorepo multi-team contexts",
        verify: None,
    },
    Migration {
        version: "005_comments_audit_columns",
        // DEFAULT (uuid()) expression is unreliable across Dolt versions. Use an
        // explicit UPDATE to backfill existing rows instead of a column default.
        // Step order matters: nullable ADD → backfill → MODIFY NOT NULL → PK.
        sql: "ALTER TABLE comments ADD COLUMN id CHAR(36) FIRST;
              UPDATE comments SET id = UUID() WHERE id IS NULL OR id = '';
              ALTER TABLE comments MODIFY COLUMN id CHAR(36) NOT NULL;
              ALTER TABLE comments ADD PRIMARY KEY (id);
              ALTER TABLE comments ADD COLUMN edited_at DATETIME NULL;
              ALTER TABLE comments ADD COLUMN edit_reason TEXT NULL;
              ALTER TABLE comments ADD COLUMN original_text TEXT NULL;
              ALTER TABLE comments ADD COLUMN deleted_at DATETIME NULL;
              ALTER TABLE comments ADD COLUMN delete_reason TEXT NULL",
        description: "Audit-trail columns + UUID id for comment update/delete (rosary-a96b06)",
        verify: Some(
            "SELECT id, edited_at, edit_reason, original_text, deleted_at, delete_reason \
             FROM comments LIMIT 0",
        ),
    },
    Migration {
        version: "006_drop_events_issue_fk",
        // The events table is best-effort audit logging. Synthetic IDs like
        // `_schema` (used to record migration application) violate the FK on
        // every rsry invocation, producing a noisy warning. Dropping the FK
        // is the right call: events is append-only audit, orphaned rows
        // after a hypothetical bead delete are harmless, and the SQLite
        // backend never had this constraint either. The IF EXISTS guard is
        // for older Dolt versions where the constraint may not be present.
        sql: "ALTER TABLE events DROP FOREIGN KEY fk_events_issue",
        description: "Drop fk_events_issue so synthetic _schema event log stops warning",
        verify: None,
    },
];

impl DoltClient {
    /// Run all pending migrations on this database.
    /// Idempotent — skips already-applied migrations.
    pub async fn migrate(&self) -> Result<Vec<String>> {
        let mut applied = Vec::new();

        for migration in MIGRATIONS {
            if self.migration_applied(migration.version).await? {
                // Already recorded as applied — but verify anyway if we can.
                // A partial apply (SQL failed, but error was silently swallowed)
                // would mark the migration done while leaving the schema broken.
                if let Some(verify_sql) = migration.verify
                    && let Err(e) = query(verify_sql).execute(&self.pool).await
                {
                    eprintln!(
                        "[migrate] WARNING: {} is marked applied but verify failed: {e}",
                        migration.version
                    );
                    eprintln!(
                        "[migrate] WARNING: schema may be partially applied — run manual repair"
                    );
                }
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
                    let err_lower = e.to_string().to_lowercase();
                    // Only treat as already-applied when the error is structurally
                    // about duplication — not any error containing a vague keyword.
                    // "already exists" and "does not exist" were removed: too broad,
                    // they masked real failures (e.g. DEFAULT (uuid()) not supported).
                    let already_applied = err_lower.contains("duplicate column name")
                        || err_lower.contains("duplicate key name")
                        || err_lower.contains("multiple primary key")
                        // DROP FOREIGN KEY on one that was already dropped / never existed.
                        || (err_lower.contains("can't drop")
                            && (err_lower.contains("doesn't exist")
                                || err_lower.contains("does not exist")))
                        // Dolt-specific FK missing: "foreign key `name` was not found".
                        || (err_lower.contains("foreign key")
                            && err_lower.contains("was not found"));
                    if already_applied {
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

            // Verify schema is actually in place before recording as applied.
            // This prevents partial applies from silently being marked done.
            if let Some(verify_sql) = migration.verify {
                query(verify_sql)
                    .execute(&self.pool)
                    .await
                    .with_context(|| {
                        format!(
                            "migration {} verify failed — schema not fully applied",
                            migration.version
                        )
                    })?;
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
