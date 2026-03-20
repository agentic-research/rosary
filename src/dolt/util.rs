use anyhow::Context;
use sqlx_core::query::query;
use sqlx_core::row::Row;

use super::DoltClient;

impl DoltClient {
    /// Parse files and test_files from the notes JSON column.
    pub(crate) fn parse_files_from_notes(row: &sqlx_mysql::MySqlRow) -> (Vec<String>, Vec<String>) {
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

    /// Enable dolt_transaction_commit at the server level (GLOBAL) so ALL
    /// connections auto-create Dolt commits. This handles multiple rsry
    /// processes sharing the same Dolt server (MCP stdio + HTTP + agent MCP).
    ///
    /// With this set, writes are immediately visible to other connections
    /// without a separate commit step — no data loss on timeout.
    pub(crate) async fn enable_auto_dolt_commit(&self) {
        let result = query("SET GLOBAL dolt_transaction_commit = 1")
            .execute(&self.pool)
            .await;
        if let Err(e) = result {
            eprintln!("[dolt] warning: failed to enable dolt_transaction_commit: {e}");
        }
    }

    /// Explicit Dolt commit — only used as fallback when dolt_transaction_commit
    /// is not available. Prefer enable_auto_dolt_commit() at connection time.
    pub(crate) async fn auto_commit(&self, _message: &str) {
        // No-op: dolt_transaction_commit handles this automatically.
        // If enable_auto_dolt_commit() failed at connect time, writes
        // are still visible within the same session but may not persist
        // across connections until the session closes.
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
