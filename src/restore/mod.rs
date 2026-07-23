//! Id-**preserving** bead restore from the JSON contract — the inverse of
//! [`crate::import::export_beads_contract_jsonl`], and the bd-free
//! `bd init --from-jsonl` equivalent (ADR-0014, rosary-9d4951).
//!
//! Split out from `import.rs` deliberately: `import` re-keys (fresh ids, for
//! copying beads into another instance), `restore` preserves ids (for
//! reconstructing a clobbered store). Keeping them in one file also pushed
//! `import.rs` over the Golden Rule 2 length gate.

use anyhow::{Context, Result};
use serde_json::Value;

use crate::import::schema_version_warning;
use crate::store::BeadStore;

/// Parse the bead contract as **JSONL** (one JSON object per line) — the format
/// [`crate::import::export_beads_contract_jsonl`] emits and
/// [`restore_beads_from_contract`] consumes. Blank lines are tolerated; a
/// malformed line fails loud with its line number rather than silently dropping
/// a bead. Distinct from [`crate::import::read_beads_json`], which parses a
/// single JSON *array*.
pub fn read_beads_jsonl(file: Option<String>) -> Result<Vec<Value>> {
    let text = match file {
        Some(path) => std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?,
        None => {
            use std::io::Read as _;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            buf
        }
    };
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        out.push(
            serde_json::from_str(line)
                .with_context(|| format!("parsing bead JSONL line {}", i + 1))?,
        );
    }
    Ok(out)
}

/// Parse a contract timestamp (RFC3339, as `bead_to_contract_value` emits).
/// Returns `None` for missing/unparseable — treated as "not newer", so a
/// malformed record can never win a last-writer-wins comparison.
/// Truncated to whole seconds: the store persists timestamps at 1-second
/// resolution, so an incoming record carrying sub-seconds would compare as
/// strictly-newer than its own round-tripped copy and "update" on every sync
/// forever. Comparing at the store's real resolution is what makes a
/// steady-state sync a genuine no-op.
fn parse_ts(s: Option<&str>) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::Timelike as _;
    s.and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&chrono::Utc))
        .and_then(|t| t.with_nanosecond(0))
}

/// Write the record's `created_at`/`updated_at` verbatim. Must run AFTER any
/// write that stamps `now()` (create/status/field updates), or it gets clobbered.
async fn restore_ts(
    store: &crate::bead_sqlite::SqliteBeadStore,
    bead: &Value,
    id: &str,
) -> Result<()> {
    if let (Some(c), Some(u)) = (
        parse_ts(bead["created_at"].as_str()),
        parse_ts(bead["updated_at"].as_str()),
    ) {
        store
            .restore_timestamps(id, c, u)
            .await
            .with_context(|| format!("restoring timestamps for {id}"))?;
    }
    Ok(())
}

/// Apply a strictly-newer incoming record onto an existing bead (LWW winner):
/// scalar fields, then status verbatim (bypassing the transition guard — a
/// sync reconstructs a peer's state, it doesn't transition), then the source
/// timestamps last.
async fn apply_incoming(
    store: &crate::bead_sqlite::SqliteBeadStore,
    bead: &Value,
    id: &str,
) -> Result<()> {
    let update = crate::bead::BeadUpdate {
        title: bead["title"].as_str().map(str::to_string),
        description: bead["description"].as_str().map(str::to_string),
        priority: bead["priority"].as_u64().map(|p| p as u8),
        issue_type: bead["issue_type"].as_str().map(str::to_string),
        owner: bead["owner"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        files: None,
        test_files: None,
        acceptance_criteria: bead["acceptance_criteria"].as_str().map(str::to_string),
    };
    if !update.is_empty() {
        store
            .update_bead_fields(id, &update)
            .await
            .with_context(|| format!("applying incoming fields to {id}"))?;
    }
    if let Some(status) = bead["status"].as_str() {
        store
            .restore_status(id, status)
            .await
            .with_context(|| format!("applying incoming status to {id}"))?;
    }
    restore_ts(store, bead, id).await
}

/// Result of an id-preserving restore.
pub struct RestoreResult {
    pub restored: usize,
    /// Existing beads whose incoming record was NEWER and was applied (LWW).
    pub updated: usize,
    /// Existing beads left alone because the local copy was same-or-newer.
    pub skipped_existing: usize,
    pub dependencies: usize,
    pub comments: usize,
}

/// Id-**preserving** restore from the bead JSON contract — the inverse of
/// [`crate::import::export_beads_contract_jsonl`], and the bd-free
/// `bd init --from-jsonl` equivalent (ADR-0014, rosary-9d4951). Where
/// [`crate::import::import_bead`] deliberately re-keys (so cross-instance copies
/// never collide), this writes each bead under its **original** id with status,
/// dependency edges, and comment bodies verbatim — the primitive a store
/// recovery needs. It reuses the same verbatim writes as the Dolt→SQLite
/// migration (`restore_status` bypasses the transition guard; `restore_dependency`
/// bypasses the edge-existence check, so cross-repo / dangling edges survive).
///
/// **Idempotent by id:** a bead whose id already exists is skipped, so a restore
/// never clobbers a live bead and a re-run over a partially-restored store fills
/// only the gaps. Two passes so dependency targets exist before edges reference
/// them; edges + comments are wired only for the beads actually restored (never
/// re-appending comments onto a skipped, already-present bead).
///
/// SQLite-typed (not `dyn BeadStore`) because the verbatim primitives are
/// SQLite's — restore-from-jsonl is the SQLite recovery path (the ADR-0021
/// migration targets SQLite); Dolt repos recover via `dolt backup` / branches.
pub async fn restore_beads_from_contract(
    beads: &[Value],
    store: &crate::bead_sqlite::SqliteBeadStore,
    repo_name: &str,
) -> Result<RestoreResult> {
    if let Some(warning) = schema_version_warning(beads) {
        eprintln!("[restore] warning: {warning}");
    }

    let mut result = RestoreResult {
        restored: 0,
        updated: 0,
        skipped_existing: 0,
        dependencies: 0,
        comments: 0,
    };
    // Wire edges/comments (pass 2) for every bead we created OR updated — not
    // for ones we left alone, so a no-op sync stays a no-op.
    let mut restored_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    let str_array = |bead: &Value, key: &str| -> Vec<String> {
        bead.get(key)
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };

    // Pass 1: beads. Skip any id already present (idempotent, never clobbers).
    for bead in beads {
        let id = bead["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("bead contract record missing `id`"))?;
        let incoming_updated = parse_ts(bead["updated_at"].as_str());

        // ── Already present: last-writer-wins on updated_at ──
        // A restore must never silently lose a peer's state transition (the
        // "vigil closed it but no other machine knows" case, rosary-4ebf52),
        // nor clobber a local edit that is newer than the incoming record.
        if let Some(local) = store.get_bead(id, repo_name).await? {
            let local_updated = Some(local.updated_at);
            // Tie => keep local. Convergent: two machines holding byte-equal
            // records both no-op, so a steady-state sync writes nothing.
            if incoming_updated.is_none() || incoming_updated <= local_updated {
                result.skipped_existing += 1;
                continue;
            }
            apply_incoming(store, bead, id).await?;
            restored_ids.insert(id.to_string());
            result.updated += 1;
            continue;
        }

        let issue_type = bead["issue_type"].as_str().unwrap_or("task");
        store
            .create_bead_full(crate::store::NewBead {
                id: id.to_string(),
                title: bead["title"].as_str().unwrap_or("").to_string(),
                description: bead["description"].as_str().unwrap_or("").to_string(),
                priority: bead["priority"].as_u64().unwrap_or(2) as u8,
                issue_type: issue_type.to_string(),
                owner: bead["owner"].as_str().unwrap_or("").to_string(),
                files: str_array(bead, "files"),
                test_files: str_array(bead, "test_files"),
                depends_on: Vec::new(), // edges added in pass 2, after targets exist
                created_by: bead["created_by"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
                scope: bead["scope"].as_str().unwrap_or("").to_string(),
                derived_from: Vec::new(),
                acceptance_criteria: bead["acceptance_criteria"]
                    .as_str()
                    .unwrap_or("")
                    .to_string(),
            })
            .await
            .with_context(|| format!("restoring bead {id}"))?;

        // create_bead_full hardcodes status='open' — restore the real status
        // verbatim (a restore reconstructs existing state, it doesn't transition).
        let status = bead["status"].as_str().unwrap_or("open");
        if status != "open" {
            store
                .restore_status(id, status)
                .await
                .with_context(|| format!("restoring status for {id}"))?;
        }
        if let Some(ext) = bead["external_ref"].as_str().filter(|e| !e.is_empty()) {
            store
                .set_external_ref(id, ext)
                .await
                .with_context(|| format!("restoring external_ref for {id}"))?;
        }
        // LAST: create_bead_full and restore_status both stamp now(); rewrite
        // the source timestamps verbatim so LWW stays meaningful and a
        // re-export is byte-identical to what we ingested.
        restore_ts(store, bead, id).await?;
        restored_ids.insert(id.to_string());
        result.restored += 1;
    }

    // Pass 2: dependency edges + comment bodies, for restored beads only.
    for bead in beads {
        let id = bead["id"].as_str().unwrap_or("");
        if !restored_ids.contains(id) {
            continue;
        }
        for dep in str_array(bead, "dependencies") {
            store
                .restore_dependency(id, &dep)
                .await
                .with_context(|| format!("restoring dependency {id} -> {dep}"))?;
            result.dependencies += 1;
        }
        if let Some(comments) = bead.get("comments").and_then(|v| v.as_array()) {
            // Comments are append-only and this pass now also runs for beads
            // updated by LWW, so adding blindly would duplicate every comment
            // on every sync. Dedup on (author, text) against what's stored —
            // which keeps the sync idempotent for comments too.
            let existing: std::collections::HashSet<(String, String)> = store
                .list_comments(id, true)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|c| (c.author, c.text))
                .collect();
            for c in comments {
                let body = c
                    .get("text")
                    .or_else(|| c.get("body"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if body.trim().is_empty() {
                    continue;
                }
                let author = c
                    .get("author")
                    .and_then(|v| v.as_str())
                    .unwrap_or("restore");
                if existing.contains(&(author.to_string(), body.to_string())) {
                    continue;
                }
                store
                    .add_comment(id, body, author)
                    .await
                    .with_context(|| format!("restoring comment on {id}"))?;
                result.comments += 1;
            }
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests;
