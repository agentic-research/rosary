//! Bounded refresh for the git-tracked public bead projection.

use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
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
        .map(crate::jsonl::join)
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

/// The repo owner's opt-in boundary: a non-Dolt store whose
/// `.beads/beads.jsonl` exists AND is git-tracked. The file existing is not
/// consent; being committed is.
fn is_opted_in(repo_root: &Path, jsonl: &Path) -> bool {
    let beads_dir = crate::resolve_beads_dir(repo_root);
    !crate::bead_backend::is_dolt_backed(&beads_dir) && jsonl.is_file() && is_git_tracked(repo_root)
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
#[expect(
    dead_code,
    reason = "the trunk refresh (P4, rosary-e5c0a0, `publish::trunk`) is its caller and is still a stub; `expect`, not `allow`, so the annotation fails the build once it is stale"
)]
pub async fn refresh_tracked_beads_jsonl(
    store: &dyn BeadStore,
    repo_name: &str,
    repo_root: &Path,
) -> Result<bool> {
    let jsonl = repo_root.join(".beads/beads.jsonl");
    if !is_opted_in(repo_root, &jsonl) {
        return Ok(false);
    }

    let published = crate::restore::read_beads_jsonl(Some(jsonl.to_string_lossy().into_owned()))?;
    let next = export_published_beads_contract_jsonl(store, &published, repo_name).await?;
    atomic_replace(&jsonl, &next)?;
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
#[expect(
    dead_code,
    reason = "reachable only through `publish_ids` until P2 (rosary-e5bfd3) wires the commit-msg hook; `expect`, not `allow`, so the annotation fails the build once it is stale"
)]
pub async fn upsert_tracked_bead(
    store: &dyn BeadStore,
    bead_id: &str,
    repo_name: &str,
    repo_root: &Path,
    allow_insert: bool,
) -> Result<bool> {
    let jsonl = repo_root.join(".beads/beads.jsonl");
    if !is_opted_in(repo_root, &jsonl) {
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

/// Outcome of publishing a named id set into the tracked projection.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[expect(
    dead_code,
    reason = "constructed only by `publish_ids` until P2 (rosary-e5bfd3) wires the commit-msg hook; `expect`, not `allow`, so the annotation fails the build once it is stale"
)]
pub struct PublishReport {
    pub inserted: Vec<String>,
    pub updated: Vec<String>,
    pub unchanged: Vec<String>,
    /// Named but absent from the store (e.g. the `rosary-000000` placeholder
    /// fixtures use). Not an error: a commit subject naming an id the store
    /// does not hold must not fail the commit.
    pub missing: Vec<String>,
}

/// Publish exactly `ids` into an opted-in projection (ADR-0024 amendment A).
///
/// This is the primitive the commit-msg hook (P2, `publish::commit`) and the
/// trunk refresh (P4, `publish::trunk`) call; the decorator in [`crate::publish`]
/// classifies writes but never touches the file. Naming a bead in a commit IS
/// publishing it, so every id is upserted with `allow_insert = true` — the
/// commit subject already made the create/update decision. Repeated ids are
/// published once.
///
/// [`upsert_tracked_bead`]'s opt-in check (Dolt / file missing / untracked)
/// remains the boundary: a repo that has not opted in gets an empty report and
/// an untouched tree.
#[expect(
    dead_code,
    reason = "the commit-msg hook (P2, rosary-e5bfd3, `publish::commit`) is its caller and is still a stub; `expect`, not `allow`, so the annotation fails the build once it is stale"
)]
pub async fn publish_ids(
    store: &dyn BeadStore,
    repo_name: &str,
    repo_root: &Path,
    ids: &[String],
) -> Result<PublishReport> {
    let jsonl = repo_root.join(".beads/beads.jsonl");
    let mut report = PublishReport::default();
    if !is_opted_in(repo_root, &jsonl) {
        return Ok(report);
    }
    // Read once, before the loop: `upsert_tracked_bead` adds each id as it
    // goes, so re-reading would misreport an id it just inserted as "updated".
    let published_before: BTreeSet<String> =
        crate::restore::read_beads_jsonl(Some(jsonl.to_string_lossy().into_owned()))?
            .iter()
            .filter_map(|record| record.get("id").and_then(Value::as_str))
            .map(str::to_owned)
            .collect();
    let mut seen = BTreeSet::new();
    for id in ids {
        if !seen.insert(id.as_str()) {
            continue;
        }
        if store.get_bead(id, repo_name).await?.is_none() {
            report.missing.push(id.clone());
            continue;
        }
        let changed = upsert_tracked_bead(store, id, repo_name, repo_root, true).await?;
        let bucket = match (published_before.contains(id), changed) {
            (false, true) => &mut report.inserted,
            (true, true) => &mut report.updated,
            (_, false) => &mut report.unchanged,
        };
        bucket.push(id.clone());
    }
    Ok(report)
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

    /// The final record is **terminated**, not merely separated.
    ///
    /// Both failures this guards are silent — the export still parses either
    /// way. Unterminated, appending a bead also rewrites the previously-last
    /// line (it gains the `\n`), so the diff-stability the export is sorted to
    /// achieve stops holding exactly where appends land; and pre-commit's
    /// `end-of-file-fixer` deadlocks against the rsry hook that stages this
    /// file, blocking every commit in the repo.
    #[tokio::test]
    async fn export_terminates_the_final_record_so_appends_stay_one_line_diffs() {
        let store =
            crate::bead_sqlite::SqliteBeadStore::connect(std::path::Path::new(":memory:")).unwrap();
        let first = serde_json::json!({"id": "rosary-public1", "status": "open"});
        let second = serde_json::json!({"id": "rosary-public2", "status": "open"});

        let one =
            export_published_beads_contract_jsonl(&store, std::slice::from_ref(&first), "rosary")
                .await
                .unwrap();
        let two = export_published_beads_contract_jsonl(&store, &[first, second], "rosary")
            .await
            .unwrap();

        assert!(
            one.ends_with('\n'),
            "final record must be newline-terminated, got: {one:?}"
        );
        assert!(
            !one.ends_with("\n\n"),
            "exactly one terminator, got: {one:?}"
        );
        // Byte-for-byte prefix: appending a bead leaves every earlier line
        // untouched, which is what makes "this commit added exactly one bead"
        // a true statement about the git diff rather than an aspiration.
        assert!(
            two.starts_with(&one),
            "appending a record rewrote an existing line"
        );
        assert_eq!(two.lines().count(), 2);
    }

    /// No records is an empty file — not a file containing one blank line,
    /// which would parse as a record and fail.
    #[tokio::test]
    async fn empty_export_is_an_empty_file_not_a_blank_line() {
        let store =
            crate::bead_sqlite::SqliteBeadStore::connect(std::path::Path::new(":memory:")).unwrap();
        let output = export_published_beads_contract_jsonl(&store, &[], "rosary")
            .await
            .unwrap();
        assert!(output.is_empty(), "expected empty, got: {output:?}");
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
