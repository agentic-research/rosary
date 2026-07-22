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

/// Result of an id-preserving restore.
pub struct RestoreResult {
    pub restored: usize,
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
        skipped_existing: 0,
        dependencies: 0,
        comments: 0,
    };
    // Only wire edges/comments (pass 2) for ids we actually restored here.
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
        if store.get_bead(id, repo_name).await?.is_some() {
            result.skipped_existing += 1;
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
mod tests {
    use super::*;
    use crate::import::bead_to_contract_value;
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

    /// rosary-9d4951: the id-PRESERVING restore is the inverse of the contract
    /// export — unlike `import_bead` (which re-keys), a restored bead keeps its
    /// ORIGINAL id, status, dependency edge, and comment, and a second restore
    /// is a no-op (idempotent by id, never clobbers).
    #[tokio::test]
    async fn restore_from_contract_preserves_id_status_and_deps() {
        let store =
            crate::bead_sqlite::SqliteBeadStore::connect(std::path::Path::new(":memory:")).unwrap();
        let mut b = make_bead("ley-line-open-4aeb4f", "research", "ley-line-open");
        b.title = "Research: Turso".to_string();
        b.status = "closed".to_string();
        b.priority = 1;
        // A cross-repo / not-yet-restored dep target — restore_dependency must
        // preserve it verbatim (no existence check), like the migration.
        let deps = vec!["ley-line-open-71b20c".to_string()];
        let comments = vec![sample_comment(
            "c1",
            "ley-line-open-4aeb4f",
            "the finding",
            "bob",
        )];
        let v = bead_to_contract_value(&b, &deps, &comments);

        let r = restore_beads_from_contract(&[v.clone()], &store, "ley-line-open")
            .await
            .unwrap();
        assert_eq!(r.restored, 1);
        assert_eq!(r.dependencies, 1);
        assert_eq!(r.comments, 1);
        assert_eq!(r.skipped_existing, 0);

        // Id preserved verbatim (the whole point — import_bead would re-key).
        let got = store
            .get_bead("ley-line-open-4aeb4f", "ley-line-open")
            .await
            .unwrap()
            .expect("bead restored under its original id");
        assert_eq!(got.id, "ley-line-open-4aeb4f");
        assert_eq!(got.priority, 1);
        assert_eq!(
            crate::bead::BeadState::from(got.status.as_str()),
            crate::bead::BeadState::Done,
            "status restored verbatim, not reset to open"
        );
        assert_eq!(
            store
                .get_dependencies("ley-line-open-4aeb4f")
                .await
                .unwrap(),
            vec!["ley-line-open-71b20c".to_string()],
            "dependency edge preserved verbatim (dangling target ok)"
        );
        assert!(
            store
                .list_comments("ley-line-open-4aeb4f", false)
                .await
                .unwrap()
                .iter()
                .any(|c| c.text == "the finding"),
            "comment preserved"
        );

        // Idempotent: a second restore skips the now-present id, no clobber, no
        // duplicate comment.
        let r2 = restore_beads_from_contract(&[v], &store, "ley-line-open")
            .await
            .unwrap();
        assert_eq!(r2.restored, 0, "already-present id is skipped");
        assert_eq!(r2.skipped_existing, 1);
        assert_eq!(
            store
                .list_comments("ley-line-open-4aeb4f", false)
                .await
                .unwrap()
                .len(),
            1,
            "no duplicate comment on re-restore"
        );
    }
}
