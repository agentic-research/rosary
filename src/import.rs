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

/// Version of the bead JSON contract emitted by [`bead_to_contract_value`].
/// Per ADR-0014 D2, the contract carries an integer `schema_version` and
/// evolves under additive-change discipline. Bump only on additive changes;
/// importers tolerate older/missing values and warn on newer ones.
pub const BEAD_CONTRACT_SCHEMA_VERSION: i64 = 1;

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
        "schema_version": BEAD_CONTRACT_SCHEMA_VERSION,
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
///
/// **Fidelity (rosary-c67538):** this importer preserves title, description,
/// priority, issue_type, files/test_files, **owner**, **created_by**,
/// **comment bodies** (with author), and a **terminal (done/closed) status**.
/// It deliberately does NOT preserve: the original **id** (a fresh one is
/// minted), **dependency edges** (source ids would dangle under fresh ids —
/// needs a two-pass id remap), **comment timestamps/authorship exactness**
/// (reset to now), or non-terminal transient states. For a byte-exact,
/// id-preserving restore, use `bd init --from-jsonl` on the export — the
/// contract carries full fidelity even though this rosary importer is
/// intentionally approximate.
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
    // Preserve the source owner/created_by when present (previously dropped —
    // rosary-c67538); fall back to the issue-type default agent.
    let owner = bead["owner"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| crate::dispatch::default_agent(issue_type));
    let created_by = bead["created_by"].as_str().filter(|s| !s.is_empty());
    let str_array = |key: &str| -> Vec<String> {
        bead.get(key)
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let files = str_array("files");
    let test_files = str_array("test_files");

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
            // Dependency edges are NOT re-wired: exported dep ids reference the
            // SOURCE store, and this importer mints fresh ids, so wiring them
            // would create dangling/phantom blockers. Faithful dep restore
            // needs `bd init --from-jsonl` (id-preserving) — see fn doc.
            &[],
            created_by,
            "",
            &[],
        )
        .await?;

    // Re-attach comment bodies (with original author) — previously dropped.
    // Comment timestamps reset to now (add_comment has no created_at param);
    // for byte-exact comment history use `bd init --from-jsonl`.
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
            let author = c.get("author").and_then(|v| v.as_str()).unwrap_or("import");
            client.add_comment(&id, body, author).await?;
        }
    }

    // Preserve a terminal (done/closed) status (previously every import landed
    // as open). Non-terminal transient states (dispatched/blocked/…) still
    // import as open — they describe live orchestration, not durable state.
    let status = bead["status"].as_str().unwrap_or("open");
    if crate::bead::BeadState::from(status) == crate::bead::BeadState::Done {
        client.close_bead(&id).await?;
    }

    Ok(Some(id))
}

/// Tolerant `schema_version` check over a batch (ADR-0014 additive discipline).
/// Returns `Some(warning)` when the batch needs operator attention, else `None`:
/// missing / current / older versions import silently; a **newer** integer
/// version warns (may carry fields we don't read yet); a **present-but-
/// unparseable** version (float, string, null) also warns, since a malformed
/// producer shouldn't fail silently. Pure + returns the message so it's unit-
/// testable without capturing stderr.
pub fn schema_version_warning(beads: &[Value]) -> Option<String> {
    let mut max_int: Option<i64> = None;
    let mut saw_unparseable = false;
    for b in beads {
        match b.get("schema_version") {
            None => {}
            Some(v) => match v.as_i64() {
                Some(n) => max_int = Some(max_int.map_or(n, |m| m.max(n))),
                None => saw_unparseable = true,
            },
        }
    }
    if let Some(n) = max_int.filter(|n| *n > BEAD_CONTRACT_SCHEMA_VERSION) {
        return Some(format!(
            "input schema_version {n} is newer than supported {BEAD_CONTRACT_SCHEMA_VERSION}; \
             importing known fields only — upgrade rsry if data looks incomplete"
        ));
    }
    if saw_unparseable {
        return Some(
            "input has a non-integer schema_version; treating those records as unversioned \
             and importing known fields only"
                .to_string(),
        );
    }
    None
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

    if let Some(warning) = schema_version_warning(beads) {
        eprintln!("[import] warning: {warning}");
    }

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
        // ADR-0014 D2: the contract is versioned with an integer schema_version.
        assert_eq!(v["schema_version"], BEAD_CONTRACT_SCHEMA_VERSION);
    }

    /// rosary-c67538: rosary re-import preserves owner, created_by, comment
    /// bodies, and a terminal (closed) status — previously all dropped.
    #[tokio::test]
    async fn import_preserves_owner_created_by_comments_and_closed_status() {
        let dst =
            crate::bead_sqlite::SqliteBeadStore::connect(std::path::Path::new(":memory:")).unwrap();
        let mut b = make_bead("rosary-src001", "bug", "rosary");
        b.title = "Round-trip me".to_string();
        b.owner = Some("staging-agent".to_string());
        b.created_by = Some("jamestexas".to_string());
        b.status = "closed".to_string();
        let comments = vec![sample_comment("c1", "rosary-src001", "the finding", "bob")];
        let v = bead_to_contract_value(&b, &[], &comments);

        let new_id = import_bead(&v, &dst, "rosary").await.unwrap().unwrap();
        let got = dst.get_bead(&new_id, "rosary").await.unwrap().unwrap();

        assert_eq!(
            got.owner.as_deref(),
            Some("staging-agent"),
            "owner preserved"
        );
        assert_eq!(
            got.created_by.as_deref(),
            Some("jamestexas"),
            "created_by preserved"
        );
        assert_eq!(
            crate::bead::BeadState::from(got.status.as_str()),
            crate::bead::BeadState::Done,
            "closed status preserved"
        );
        let imported_comments = dst.list_comments(&new_id, false).await.unwrap();
        assert!(
            imported_comments.iter().any(|c| c.text == "the finding"),
            "comment body preserved"
        );
    }

    /// ADR-0014 additive-change discipline: import tolerates a record whose
    /// `schema_version` is newer than we support (warns, imports known fields)
    /// and one with the field absent (legacy / pre-versioning).
    #[tokio::test]
    async fn import_tolerates_newer_and_missing_schema_version() {
        // Separate stores so the time-based id generator can't collide on two
        // creates in the same millisecond — keeps the test about version skew.
        let newer_store =
            crate::bead_sqlite::SqliteBeadStore::connect(std::path::Path::new(":memory:")).unwrap();
        let legacy_store =
            crate::bead_sqlite::SqliteBeadStore::connect(std::path::Path::new(":memory:")).unwrap();

        // Newer than supported, with an unknown field — must import (known
        // fields only) without erroring.
        let newer = serde_json::json!({
            "schema_version": BEAD_CONTRACT_SCHEMA_VERSION + 99,
            "title": "from the future",
            "issue_type": "task",
            "future_field": "ignored",
        });
        // No version field at all (legacy / pre-versioning) — must import.
        let legacy = serde_json::json!({ "title": "no version field", "issue_type": "task" });

        let r1 = import_beads(std::slice::from_ref(&newer), &newer_store, "rosary")
            .await
            .unwrap();
        let r2 = import_beads(std::slice::from_ref(&legacy), &legacy_store, "rosary")
            .await
            .unwrap();
        assert_eq!(r1.imported, 1, "newer-version record imports");
        assert_eq!(r2.imported, 1, "missing-version record imports");
    }

    #[test]
    fn schema_version_warning_fires_only_when_needed() {
        // current / missing / older → no warning
        assert!(
            schema_version_warning(&[
                serde_json::json!({"schema_version": BEAD_CONTRACT_SCHEMA_VERSION})
            ])
            .is_none()
        );
        assert!(schema_version_warning(&[serde_json::json!({"title": "x"})]).is_none());
        // newer integer → warns, message names the version
        let w = schema_version_warning(&[
            serde_json::json!({"schema_version": BEAD_CONTRACT_SCHEMA_VERSION + 5}),
        ])
        .expect("newer version must warn");
        assert!(w.contains(&(BEAD_CONTRACT_SCHEMA_VERSION + 5).to_string()));
        // present-but-unparseable (string / float) → warns
        assert!(schema_version_warning(&[serde_json::json!({"schema_version": "2"})]).is_some());
        assert!(schema_version_warning(&[serde_json::json!({"schema_version": 2.5})]).is_some());
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
