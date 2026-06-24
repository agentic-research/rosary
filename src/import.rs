//! Shared bead import logic — used by both CLI and MCP handlers.

use anyhow::{Context, Result};
use serde_json::Value;

use crate::store::BeadStore;

/// Read a JSON bead array from a file path or stdin.
pub fn read_beads_json(file: Option<String>) -> Result<Vec<Value>> {
    let json_str = match file {
        Some(path) => std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?,
        None => {
            use std::io::Read as _;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            buf
        }
    };
    Ok(serde_json::from_str(&json_str)?)
}

/// Serialize beads to the export JSON format (includes repo, status for cross-repo round-trip).
pub fn export_beads_json(beads: &[crate::bead::Bead]) -> Vec<Value> {
    beads
        .iter()
        .map(|b| {
            serde_json::json!({
                "repo": b.repo,
                "title": b.title,
                "description": b.description,
                "priority": b.priority,
                "issue_type": b.issue_type,
                "status": b.status,
                "files": b.files,
                "test_files": b.test_files,
            })
        })
        .collect()
}

/// Map a bead (plus its dependency ids and comments) to the documented bead
/// JSON contract — the same shape `bd export` emits and `bd init --from-jsonl`
/// ingests. This is rosary's "speak beads" interop surface (ADR-0014 D2):
/// rosary owns the store, but exports a full-fidelity, ecosystem-compatible
/// snapshot. From this JSONL, `bd init --from-jsonl` can load the beads into
/// Dolt — without rosary depending on `bd`.
///
/// Unlike [`export_beads_json`] (a lossy rosary↔rosary title-dedup format), this
/// preserves id, timestamps, dependencies, and comments so no data is lost.
/// rosary-specific fields (repo/files/test_files/scope/external_ref/branch/
/// pr_url) ride alongside the contract fields for lossless rosary round-trip;
/// `bd` ignores keys it doesn't recognize.
pub fn bead_to_contract_value(
    bead: &crate::bead::Bead,
    deps: &[String],
    comments: &[crate::bead::Comment],
) -> Value {
    serde_json::json!({
        // --- documented bead JSON contract ---
        "id": bead.id,
        "title": bead.title,
        "description": bead.description,
        "status": bead.status,
        "priority": bead.priority,
        "issue_type": bead.issue_type,
        "owner": bead.owner,
        "created_at": bead.created_at.to_rfc3339(),
        "updated_at": bead.updated_at.to_rfc3339(),
        "created_by": bead.created_by,
        "dependency_count": bead.dependency_count,
        "dependent_count": bead.dependent_count,
        "comment_count": bead.comment_count,
        "dependencies": deps,
        "comments": comments,
        // --- rosary-specific extras (lossless rosary round-trip) ---
        "repo": bead.repo,
        "files": bead.files,
        "test_files": bead.test_files,
        "scope": bead.scope,
        "external_ref": bead.external_ref,
        "branch": bead.branch,
        "pr_url": bead.pr_url,
    })
}

/// Export the given beads to the documented bead JSON contract as **JSONL**
/// (one bead per line) — the format `bd init --from-jsonl` ingests. Full
/// fidelity: fetches each bead's dependencies and comments (including
/// soft-deleted ones, to preserve the audit trail). Fails loud if a
/// dependency/comment fetch errors rather than silently dropping data.
pub async fn export_beads_contract_jsonl(
    store: &dyn BeadStore,
    beads: &[crate::bead::Bead],
) -> Result<String> {
    let mut lines = Vec::with_capacity(beads.len());
    for b in beads {
        let deps = store
            .get_dependencies(&b.id)
            .await
            .with_context(|| format!("fetching dependencies for {}", b.id))?;
        let comments = store
            .list_comments(&b.id, true)
            .await
            .with_context(|| format!("fetching comments for {}", b.id))?;
        lines.push(serde_json::to_string(&bead_to_contract_value(
            b, &deps, &comments,
        ))?);
    }
    Ok(lines.join("\n"))
}

/// Result of importing a batch of beads into a single repo.
pub struct ImportResult {
    pub imported: usize,
    pub skipped: usize,
    pub ids: Vec<String>,
}

/// Parse a JSON bead value into fields and create it via the BeadStore.
/// Returns `Some(id)` if created, `None` if skipped (duplicate title).
pub async fn import_bead(
    bead: &Value,
    client: &dyn BeadStore,
    repo_name: &str,
) -> Result<Option<String>> {
    let title = bead["title"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("bead title required"))?;

    // Dedup: skip if exact title match exists
    let existing = client.search_beads(title, repo_name, 10).await?;
    if existing.iter().any(|b| b.title == title) {
        return Ok(None);
    }

    let description = bead["description"].as_str().unwrap_or("");
    let priority = bead["priority"].as_u64().unwrap_or(2) as u8;
    let issue_type = bead["issue_type"].as_str().unwrap_or("task");
    let owner = crate::dispatch::default_agent(issue_type);
    let files: Vec<String> = bead
        .get("files")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let test_files: Vec<String> = bead
        .get("test_files")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let id = crate::generate_bead_id(repo_name);
    client
        .create_bead_full(
            &id,
            title,
            description,
            priority,
            issue_type,
            owner,
            &files,
            &test_files,
            &[],
            None,
            "",
            &[],
        )
        .await?;

    Ok(Some(id))
}

/// Import a batch of beads into a single repo. Returns counts + created IDs.
pub async fn import_beads(
    beads: &[Value],
    client: &dyn BeadStore,
    repo_name: &str,
) -> Result<ImportResult> {
    let mut result = ImportResult {
        imported: 0,
        skipped: 0,
        ids: Vec::new(),
    };

    for bead in beads {
        match import_bead(bead, client, repo_name).await? {
            Some(id) => {
                result.ids.push(id);
                result.imported += 1;
            }
            None => result.skipped += 1,
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::make_bead;

    fn sample_comment(id: &str, issue: &str, text: &str, author: &str) -> crate::bead::Comment {
        crate::bead::Comment {
            id: id.to_string(),
            issue_id: issue.to_string(),
            text: text.to_string(),
            author: author.to_string(),
            created_at: chrono::Utc::now(),
            edited_at: None,
            edit_reason: None,
            original_text: None,
            deleted_at: None,
            delete_reason: None,
        }
    }

    /// rosary-656967: the contract export is full-fidelity — id, timestamps,
    /// dependencies, and comments survive (unlike the lossy `export_beads_json`).
    #[test]
    fn contract_value_preserves_id_deps_and_comments() {
        let mut b = make_bead("rosary-abc123", "bug", "rosary");
        b.title = "Fix the thing".to_string();
        b.status = "closed".to_string();
        b.priority = 1;
        b.dependency_count = 1;
        b.comment_count = 1;
        let deps = vec!["rosary-dep001".to_string()];
        let comments = vec![sample_comment("c1", "rosary-abc123", "looks good", "alice")];

        let v = bead_to_contract_value(&b, &deps, &comments);

        // Documented contract fields (the lossy export drops id/timestamps).
        assert_eq!(v["id"], "rosary-abc123");
        assert_eq!(v["title"], "Fix the thing");
        assert_eq!(v["status"], "closed");
        assert_eq!(v["priority"], 1);
        assert_eq!(v["issue_type"], "bug");
        assert!(v["created_at"].is_string(), "created_at must be exported");
        assert!(v["updated_at"].is_string(), "updated_at must be exported");
        // No data loss: dependencies + comments carried.
        assert_eq!(v["dependencies"][0], "rosary-dep001");
        assert_eq!(v["comments"][0]["text"], "looks good");
        assert_eq!(v["comments"][0]["author"], "alice");
        // rosary-specific extras ride alongside for lossless round-trip.
        assert_eq!(v["repo"], "rosary");
    }

    /// Contrast: the legacy export drops id/timestamps/deps/comments — this test
    /// pins WHY the contract export exists (data-loss avoidance, ADR-0014).
    #[test]
    fn legacy_export_is_lossy_by_comparison() {
        let b = make_bead("rosary-abc123", "bug", "rosary");
        let legacy = &export_beads_json(std::slice::from_ref(&b))[0];
        assert!(legacy.get("id").is_none(), "legacy export omits id");
        assert!(
            legacy.get("comments").is_none(),
            "legacy export omits comments"
        );
    }
}
