//! Bounded refresh for the git-tracked public bead projection.

use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crate::store::BeadStore;

async fn live_contract_value(store: &dyn BeadStore, bead: &crate::bead::Bead) -> Result<Value> {
    let deps = store
        .get_dependencies(&bead.id)
        .await
        .with_context(|| format!("fetching dependencies for {}", bead.id))?;
    let comments = store
        .list_comments(&bead.id, true)
        .await
        .with_context(|| format!("fetching comments for {}", bead.id))?;
    Ok(crate::import::bead_to_contract_value(
        bead, &deps, &comments,
    ))
}

fn serialize_records(records: BTreeMap<String, Value>) -> Result<String> {
    records
        .into_values()
        .map(|record| serde_json::to_string(&record).map_err(Into::into))
        .collect::<Result<Vec<_>>>()
        .map(|lines| lines.join("\n"))
}

/// Render only records already present in a public JSONL projection.
///
/// Live records replace published records with the same id. Published records
/// missing locally are preserved, and local-only records are never added.
pub async fn export_published_beads_contract_jsonl(
    store: &dyn BeadStore,
    published: &[Value],
    repo_name: &str,
) -> Result<String> {
    let live = store.list_all_beads(repo_name).await?;
    let live_by_id: HashMap<&str, &crate::bead::Bead> =
        live.iter().map(|bead| (bead.id.as_str(), bead)).collect();
    let mut records = BTreeMap::new();

    for record in published {
        let id = record
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("published bead JSONL record is missing string id"))?;
        let value = match live_by_id.get(id) {
            Some(bead) => live_contract_value(store, bead).await?,
            None => record.clone(),
        };
        anyhow::ensure!(
            records.insert(id.to_string(), value).is_none(),
            "published bead JSONL contains duplicate id {id}"
        );
    }
    serialize_records(records)
}

fn is_git_tracked(repo_root: &Path) -> bool {
    std::process::Command::new("git")
        .args(["ls-files", "--error-unmatch", "--", ".beads/beads.jsonl"])
        .current_dir(repo_root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn atomic_replace(path: &Path, content: &str) -> Result<()> {
    let tmp = path.with_file_name(format!(
        ".beads.jsonl.tmp-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    std::fs::write(&tmp, content).with_context(|| format!("writing {}", tmp.display()))?;
    if let Err(error) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error).with_context(|| format!("replacing {}", path.display()));
    }
    Ok(())
}

/// Atomically refresh an opted-in, tracked JSONL projection in place.
pub async fn refresh_tracked_beads_jsonl(
    store: &dyn BeadStore,
    repo_name: &str,
    repo_root: &Path,
) -> Result<bool> {
    let beads_dir = crate::resolve_beads_dir(repo_root);
    let jsonl = repo_root.join(".beads/beads.jsonl");
    if beads_dir.join("dolt").is_dir() || !jsonl.is_file() || !is_git_tracked(repo_root) {
        return Ok(false);
    }

    let published = crate::restore::read_beads_jsonl(Some(jsonl.to_string_lossy().into_owned()))?;
    let next = export_published_beads_contract_jsonl(store, &published, repo_name).await?;
    atomic_replace(&jsonl, &next)?;
    Ok(true)
}

/// Publish one newly-created bead into an already opted-in projection.
///
/// Unlike [`refresh_tracked_beads_jsonl`], this deliberately broadens the
/// public id set by exactly `bead_id`. The tracked-file check remains the
/// repository owner's opt-in boundary.
pub async fn publish_created_bead_to_tracked_jsonl(
    store: &dyn BeadStore,
    bead_id: &str,
    repo_name: &str,
    repo_root: &Path,
) -> Result<bool> {
    let beads_dir = crate::resolve_beads_dir(repo_root);
    let jsonl = repo_root.join(".beads/beads.jsonl");
    if beads_dir.join("dolt").is_dir() || !jsonl.is_file() || !is_git_tracked(repo_root) {
        return Ok(false);
    }

    let mut published =
        crate::restore::read_beads_jsonl(Some(jsonl.to_string_lossy().into_owned()))?;
    if !published
        .iter()
        .any(|record| record.get("id").and_then(Value::as_str) == Some(bead_id))
    {
        published.push(serde_json::json!({ "id": bead_id }));
    }
    let next = export_published_beads_contract_jsonl(store, &published, repo_name).await?;
    let mut verify = || {
        anyhow::ensure!(
            next.lines().any(|line| {
                serde_json::from_str::<Value>(line)
                    .ok()
                    .and_then(|record| record.get("id").cloned())
                    .and_then(|id| id.as_str().map(str::to_owned))
                    .as_deref()
                    == Some(bead_id)
            }),
            "rendered public projection is missing newly-created bead {bead_id}"
        );
        Ok(())
    };
    let mut publish =
        |_: &crate::dispatch::commit_point::VerificationReceipt| atomic_replace(&jsonl, &next);
    crate::dispatch::commit_point::commit_external_mutation(&mut verify, &mut publish)?;
    Ok(true)
}

/// Re-render exactly ONE bead's record in the tracked projection.
///
/// The bounded whole-file paths above re-read every bead plus its dependencies
/// and comments — for rosary that is ~2400 queries. That cost is fine once per
/// command, but [`crate::publish`] calls this after *every* projected store
/// write, so the per-write cost has to be O(1) in the bead count. Here it is
/// three queries and one file rewrite.
///
/// `allow_insert` is the publication boundary, and it is the caller's decision,
/// not a guess: a CREATE deliberately broadens the public id set by exactly
/// this bead, while an UPDATE must never add an id the owner has not published
/// (the rosary-a7ee3a semantics). With `allow_insert = false` an absent id is a
/// no-op, not an error.
///
/// Returns whether the file changed.
pub async fn upsert_tracked_bead(
    store: &dyn BeadStore,
    bead_id: &str,
    repo_name: &str,
    repo_root: &Path,
    allow_insert: bool,
) -> Result<bool> {
    let beads_dir = crate::resolve_beads_dir(repo_root);
    let jsonl = repo_root.join(".beads/beads.jsonl");
    if beads_dir.join("dolt").is_dir() || !jsonl.is_file() || !is_git_tracked(repo_root) {
        return Ok(false);
    }

    let published = crate::restore::read_beads_jsonl(Some(jsonl.to_string_lossy().into_owned()))?;
    let already_published = published
        .iter()
        .any(|record| record.get("id").and_then(Value::as_str) == Some(bead_id));
    if !already_published && !allow_insert {
        return Ok(false);
    }

    // Render this bead from the store. `get_bead` and `list_all_beads` share
    // `bead_read_sql` and `bead_from_row`, so the single-record render is
    // field-identical to the whole-file render — pinned by
    // `upsert_matches_full_refresh_field_for_field` rather than assumed.
    let Some(bead) = store.get_bead(bead_id, repo_name).await? else {
        // Nothing to project. A write that leaves no readable bead is not this
        // function's problem to diagnose, but it must not blank the record.
        return Ok(false);
    };
    let rendered = live_contract_value(store, &bead).await?;

    let mut records = BTreeMap::new();
    for record in published {
        let id = record
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("published bead JSONL record is missing string id"))?;
        anyhow::ensure!(
            records.insert(id.to_string(), record.clone()).is_none(),
            "published bead JSONL contains duplicate id {id}"
        );
    }
    records.insert(bead_id.to_string(), rendered);
    let next = serialize_records(records)?;

    let existing = std::fs::read_to_string(&jsonl).unwrap_or_default();
    if existing == next {
        return Ok(false);
    }
    atomic_replace(&jsonl, &next)?;
    Ok(true)
}

#[cfg(test)]
mod jsonl_sync_tests {
    use super::*;

    #[tokio::test]
    async fn preserves_missing_public_records_and_excludes_local_only_records() {
        let store =
            crate::bead_sqlite::SqliteBeadStore::connect(std::path::Path::new(":memory:")).unwrap();
        store
            .create_bead_full(crate::store::NewBead {
                id: "rosary-local1".to_string(),
                title: "must stay private".to_string(),
                issue_type: "bug".to_string(),
                files: vec!["src/private.rs".to_string()],
                test_files: vec!["tests/private.rs".to_string()],
                acceptance_criteria: "cargo test".to_string(),
                ..Default::default()
            })
            .await
            .unwrap();
        let published = vec![
            serde_json::json!({"id": "rosary-public2", "status": "open", "marker": "keep-b"}),
            serde_json::json!({"id": "rosary-public1", "status": "closed", "marker": "keep-a"}),
        ];

        let output = export_published_beads_contract_jsonl(&store, &published, "rosary")
            .await
            .unwrap();
        let records: Vec<Value> = output
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();

        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["id"], "rosary-public1");
        assert_eq!(records[0]["marker"], "keep-a");
        assert_eq!(records[1]["id"], "rosary-public2");
        assert_eq!(records[1]["marker"], "keep-b");
        assert!(!output.contains("rosary-local1"));
    }

    #[tokio::test]
    async fn duplicate_public_ids_fail_without_collapsing_records() {
        let store =
            crate::bead_sqlite::SqliteBeadStore::connect(std::path::Path::new(":memory:")).unwrap();
        let published = vec![
            serde_json::json!({"id": "rosary-public1", "status": "open"}),
            serde_json::json!({"id": "rosary-public1", "status": "closed"}),
        ];

        let error = export_published_beads_contract_jsonl(&store, &published, "rosary")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("duplicate id rosary-public1"));
    }
}
