//! Cross-repo bead relocation (`rsry bead move`).
//!
//! Moves a bead from one repo's store to another's, in-process, **without ever
//! touching the `bd` CLI** (ADR-0014). This is the generalization of the
//! "promotion carries provenance forward" leaf (L5) in
//! `docs/problems/rosary-capture-commit-spine.md`, extended from
//! Global→repo (promote) to repo→repo (move).
//!
//! Motivating incident (2026-06-25): a `bd create --repo ley-line-open` reached
//! an empty `embeddeddolt/` dir ("no database selected") and silently mis-filed
//! LLO beads into mache's store. With `rsry move`, the fix is one command and
//! `bd` is never the answer.
//!
//! The move is **lossless and provenance-preserving**: the destination bead
//! carries the source's `derived_from`, `created_by`, files, scope, comments,
//! and status, plus a `moved from <old-id> (<repo>)` provenance entry. The
//! source is tombstoned (commented, `moved_to` event, closed) rather than
//! deleted, so the audit trail survives on both sides.

use crate::store::BeadStore;
use anyhow::{Context, Result};
use bdr::provenance::ProvenanceRef;

/// Outcome of a successful [`move_bead`], for CLI/MCP reporting.
pub struct MoveOutcome {
    /// The id assigned to the relocated bead in the destination store.
    pub new_id: String,
    /// Status carried over from the source bead.
    pub status: String,
    /// Number of *live* comments copied from source to destination
    /// (soft-deleted comments are not carried over).
    pub comments_copied: usize,
    /// Ids the moved bead depended on. These edges lived in the source store
    /// and become **cross-repo** after the move — surfaced, never silently
    /// dropped. (Full cross-repo dep re-linking via LinkageStore is follow-up.)
    pub dangling_dependencies: Vec<String>,
    /// Ids in the source store that depended on the moved bead — now orphaned
    /// in source. Surfaced for the operator to re-link.
    pub orphaned_dependents: Vec<String>,
}

/// Relocate `bead_id` from `source` (named `source_repo`) into `dest` (named
/// `dest_repo`), assigning it `new_id` in the destination.
///
/// `new_id` is passed in (rather than generated here) so the operation is
/// deterministic and testable; the CLI generates it via `generate_bead_id`.
///
/// Errors if the bead doesn't exist in the source, or if it has already been
/// moved (guards against double-move creating duplicate tombstones).
pub async fn move_bead(
    source: &dyn BeadStore,
    source_repo: &str,
    dest: &dyn BeadStore,
    dest_repo: &str,
    bead_id: &str,
    new_id: &str,
) -> Result<MoveOutcome> {
    // 1. Resolve + fetch the full source bead (handles short ids).
    let bead = source
        .get_bead(bead_id, source_repo)
        .await?
        .with_context(|| format!("bead {bead_id} not found in {source_repo}"))?;

    // 2. Guard against re-moving an already-relocated bead. The `moved_to`
    // *event* is best-effort (`log_event` may warn and drop), so it can't be
    // the sole idempotency signal — instead scan for the durable tombstone
    // *comment* (`add_comment` errors propagate, so a successful prior move
    // always left one). Fetched once here and reused for the comment copy.
    let source_comments = source.list_comments(&bead.id, true).await?;
    if source_comments
        .iter()
        .any(|c| c.author == "rsry-move" && c.text.starts_with("moved →"))
    {
        anyhow::bail!(
            "bead {} was already moved (tombstone comment present); refusing to move again",
            bead.id
        );
    }

    // 3. Provenance: carry the chain forward, then record the move itself.
    let mut derived_from = bead.derived_from.clone();
    derived_from.push(ProvenanceRef::Manual {
        note: format!("moved from {} ({})", bead.id, source_repo),
    });

    // 4. Create the relocated bead in the destination store.
    // Fall back to the type's default agent when owner is unset OR empty —
    // `create_bead_full` persists assignee non-NULL, and an empty string reads
    // back as `Some("")`, which the reconciler's auto-assign treats as "already
    // assigned" and skips. Matches how `rsry bead create` sets the owner.
    let owner = bead
        .owner
        .as_deref()
        .filter(|o| !o.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| crate::dispatch::default_agent(&bead.issue_type).to_string());
    dest.create_bead_full(crate::store::NewBead {
        id: new_id.to_string(),
        title: bead.title.clone(),
        description: bead.description.clone(),
        priority: bead.priority,
        issue_type: bead.issue_type.clone(),
        owner: owner.to_string(),
        files: bead.files.clone(),
        test_files: bead.test_files.clone(),
        // deps become cross-repo on move; surfaced below, not re-created here
        created_by: bead.created_by.clone(),
        scope: bead.scope.clone(),
        derived_from,
        acceptance_criteria: bead.acceptance_criteria.clone(), // preserve on relocate
        ..Default::default()
    })
    .await
    .with_context(|| format!("creating relocated bead {new_id} in {dest_repo}"))?;

    // 5. Preserve terminal status (a moved closed/done bead stays closed).
    // Execution states (dispatched/verifying/pr_open) are tied to the source
    // repo's worktree + dispatch and are intentionally NOT carried over — a
    // relocated in-flight bead lands `open` for re-triage in the new repo.
    // Use `close_bead` (direct write) rather than `update_status`, since the
    // state machine forbids a direct open→done transition.
    if crate::bead::BeadState::from(bead.status.as_str()) == crate::bead::BeadState::Done {
        dest.close_bead(new_id).await?;
    }

    // 6. Carry the external (Linear) reference forward.
    if let Some(ext) = &bead.external_ref {
        dest.set_external_ref(new_id, ext).await?;
    }

    // 7. Copy live comments only, oldest-first. Soft-deleted comments are
    // intentionally NOT resurrected (re-adding via `add_comment` would expose
    // deliberately-deleted text and can't preserve edited_at/original_text/
    // delete_reason anyway). Provenance of the deletion stays in the source.
    let live_comments: Vec<_> = source_comments
        .iter()
        .filter(|c| c.deleted_at.is_none())
        .collect();
    for c in &live_comments {
        dest.add_comment(new_id, &c.text, &c.author).await?;
    }

    // 8. Surface dependency edges that become cross-repo — never silently drop.
    let dangling_dependencies = source.get_dependencies(&bead.id).await?;
    let orphaned_dependents = source.get_dependents(&bead.id).await?;
    if !dangling_dependencies.is_empty() || !orphaned_dependents.is_empty() {
        let note = format!(
            "cross-repo dependency edges after move from {source_repo}: \
             depends_on={dangling_dependencies:?}, dependents={orphaned_dependents:?} \
             — re-link manually (cross-repo dep rewrite is follow-up work)",
        );
        dest.add_comment(new_id, &note, "rsry-move").await?;
    }

    // 9. Record provenance on the destination side.
    dest.log_event(
        new_id,
        "moved_from",
        &format!("{} ({source_repo})", bead.id),
    )
    .await;

    // 10. Tombstone the source: comment + event + close (preserve, don't delete).
    source
        .add_comment(
            &bead.id,
            &format!("moved → {new_id} ({dest_repo})"),
            "rsry-move",
        )
        .await?;
    source
        .log_event(&bead.id, "moved_to", &format!("{new_id} ({dest_repo})"))
        .await;
    source.close_bead(&bead.id).await?;

    Ok(MoveOutcome {
        new_id: new_id.to_string(),
        status: bead.status,
        comments_copied: live_comments.len(),
        dangling_dependencies,
        orphaned_dependents,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead_sqlite::SqliteBeadStore;
    use std::path::Path;

    fn store() -> SqliteBeadStore {
        SqliteBeadStore::connect(Path::new(":memory:")).unwrap()
    }

    #[tokio::test]
    async fn move_relocates_bead_losslessly() {
        let src = store();
        let dst = store();
        src.create_bead_full(crate::store::NewBead {
            id: "mache-lbn6".into(),
            title: "LLO cousin epic".into(),
            description: "body text".into(),
            priority: 1,
            issue_type: "epic".into(),
            owner: "feature-agent".into(),
            files: vec!["a.rs".to_string()],
            created_by: Some("alice".into()),
            ..Default::default()
        })
        .await
        .unwrap();
        src.add_comment("mache-lbn6", "first note", "bob")
            .await
            .unwrap();

        let outcome = move_bead(
            &src,
            "mache",
            &dst,
            "ley-line-open",
            "mache-lbn6",
            "ley-line-open-aaaaaa",
        )
        .await
        .unwrap();

        // Destination has an equivalent bead, fields carried forward.
        let moved = dst
            .get_bead("ley-line-open-aaaaaa", "ley-line-open")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(moved.title, "LLO cousin epic");
        assert_eq!(moved.priority, 1);
        assert_eq!(moved.issue_type, "epic");
        assert_eq!(moved.created_by.as_deref(), Some("alice"));
        assert_eq!(moved.files, vec!["a.rs".to_string()]);

        // Provenance records the move.
        assert!(
            moved.derived_from.iter().any(|p| matches!(
                p,
                ProvenanceRef::Manual { note } if note.contains("moved from mache-lbn6")
            )),
            "moved bead must carry a moved-from provenance entry"
        );

        // Comment copied.
        assert_eq!(outcome.comments_copied, 1);
        let dcomments = dst.list_comments(&moved.id, false).await.unwrap();
        assert!(dcomments.iter().any(|c| c.text == "first note"));

        // Source tombstoned: closed, absent from the active list.
        let active = src.list_beads("mache").await.unwrap();
        assert!(
            !active.iter().any(|b| b.id == "mache-lbn6"),
            "source bead should be closed after move"
        );
        let src_bead = src.get_bead("mache-lbn6", "mache").await.unwrap().unwrap();
        assert_eq!(src_bead.status, "closed");
    }

    #[tokio::test]
    async fn move_preserves_closed_status() {
        let src = store();
        let dst = store();
        src.create_bead("mache-x1", "done thing", "b", 2, "task")
            .await
            .unwrap();
        src.close_bead("mache-x1").await.unwrap();

        let outcome = move_bead(&src, "mache", &dst, "llo", "mache-x1", "llo-bbbbbb")
            .await
            .unwrap();
        assert_eq!(outcome.status, "closed");
        let moved = dst.get_bead("llo-bbbbbb", "llo").await.unwrap().unwrap();
        assert_eq!(moved.status, "closed");
    }

    #[tokio::test]
    async fn double_move_is_rejected() {
        let src = store();
        let dst = store();
        src.create_bead("mache-d1", "t", "b", 2, "task")
            .await
            .unwrap();
        move_bead(&src, "mache", &dst, "llo", "mache-d1", "llo-cccccc")
            .await
            .unwrap();
        let err = move_bead(&src, "mache", &dst, "llo", "mache-d1", "llo-dddddd").await;
        assert!(err.is_err(), "moving an already-moved bead must error");
    }

    #[tokio::test]
    async fn move_surfaces_cross_repo_deps() {
        let src = store();
        let dst = store();
        src.create_bead("mache-p", "parent", "b", 2, "task")
            .await
            .unwrap();
        src.create_bead("mache-c", "child", "b", 2, "task")
            .await
            .unwrap();
        src.add_dependency("mache-c", "mache-p").await.unwrap();

        let outcome = move_bead(&src, "mache", &dst, "llo", "mache-c", "llo-eeeeee")
            .await
            .unwrap();
        assert_eq!(outcome.dangling_dependencies, vec!["mache-p".to_string()]);
    }

    #[tokio::test]
    async fn move_does_not_resurrect_deleted_comments() {
        let src = store();
        let dst = store();
        src.create_bead("mache-z1", "t", "b", 2, "task")
            .await
            .unwrap();
        src.add_comment("mache-z1", "live note", "bob")
            .await
            .unwrap();
        src.add_comment("mache-z1", "secret deleted", "bob")
            .await
            .unwrap();
        let del_id = src
            .list_comments("mache-z1", false)
            .await
            .unwrap()
            .into_iter()
            .find(|c| c.text == "secret deleted")
            .unwrap()
            .id;
        src.delete_comment(&del_id, Some("oops")).await.unwrap();

        let outcome = move_bead(&src, "mache", &dst, "llo", "mache-z1", "llo-999999")
            .await
            .unwrap();
        assert_eq!(outcome.comments_copied, 1, "only the live comment copies");
        let dcomments = dst.list_comments("llo-999999", true).await.unwrap();
        assert!(dcomments.iter().any(|c| c.text == "live note"));
        assert!(
            !dcomments.iter().any(|c| c.text == "secret deleted"),
            "soft-deleted comment must not be resurrected in the destination"
        );
    }

    #[tokio::test]
    async fn move_missing_bead_errors() {
        let src = store();
        let dst = store();
        let err = move_bead(&src, "mache", &dst, "llo", "nope-123456", "llo-ffffff").await;
        assert!(err.is_err(), "moving a nonexistent bead must error");
    }
}
