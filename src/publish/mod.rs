//! Classify every bead-store write; publish only at commit and on the trunk
//! (ADR-0024 amendment A).
//!
//! ## What this decorator is
//!
//! [`connect_bead_store`](crate::bead_sqlite::connect_bead_store) is the single
//! entry point for all bead I/O, so wrapping its result puts a gate under every
//! write path — the ~50 that exist and every one not yet written. The gate's job
//! is CLASSIFICATION: each `BeadStore` write is named `Create`, `Update` or
//! `Whole` ([`Projected`]), or explicitly a write the tracked projection does not
//! represent. Nothing reaches the store without someone having decided what it
//! means for `.beads/beads.jsonl`.
//!
//! What the gate does NOT do is write that file. Publication happens at exactly
//! the moments git observes:
//!
//! - **At commit** (P2, [`commit`], `rosary-e5bfd3`): the commit-msg hook
//!   publishes the beads the commit subject names, via
//!   [`crate::jsonl_sync::publish_ids`].
//! - **On the trunk after a merge** (P4, [`trunk`], `rosary-e5c0a0`): a full
//!   [`crate::jsonl_sync::refresh_tracked_beads_jsonl`], committed as rosary.
//!
//! A store write in between leaves the working tree clean. The invariant is
//! about commits and the trunk, not writes: *every commit carries the current
//! records of the beads it names, and no others; the trunk carries the current
//! record of every published bead.*
//!
//! ## Why the classification stays even though the write went
//!
//! `BeadStore` has 34 methods and this type implements every one *by hand*.
//! There is deliberately no blanket forwarding impl and no `Deref`, so a new
//! REQUIRED method fails to compile until it is classified here. `create_bead`
//! is a *provided* method (ADR-0021 slice 2, `rosary-c7126b`) and provided
//! methods do not break this impl, so the compile-time half covers required
//! methods only; `tests::every_trait_method_is_classified` reads the trait body
//! out of `store.rs` and covers both. That classification is what P2 and P4
//! inherit: "is this a projected write, and which kind" is decided once, here,
//! as reviewable data, rather than re-derived by each hook.
//!
//! ## History
//!
//! `rosary-8ca6e5` introduced this decorator as a write-through: every
//! projected write re-rendered its record into the tracked file, ending the
//! store-only drift left by `rosary-a7ee3a`'s two hand-wired call sites (49
//! beads in rosary, 30 in cloister). `rosary-3d455a` then measured what
//! write-through costs: the store is branch-independent and the file is
//! branch-dependent, so materializing on every write produces the standing
//! `M .beads/beads.jsonl`, commits carrying unrelated bead records (57/57 mixed
//! commits on `main`), and refused checkouts. ADR-0024 amendment A (accepted
//! 2026-09-12) moved publication to commit time and the trunk; this module kept
//! the gate and dropped the write.
//!
//! ## What it deliberately does not do
//!
//! - **It does not write `.beads/beads.jsonl`** — not on create, update, or
//!   the whole-file kinds. `tests::a_create_leaves_the_projection_untouched`
//!   and its siblings observe that through `git status`, not through the store.
//! - **It does not keep a pending set.** The commit hook derives the ids to
//!   publish from the commit subject (`[bead-id]`), so there is nothing for the
//!   decorator to remember.
//! - **It does not opt a repo into publication.** An absent or untracked file
//!   stays that way; `jsonl_sync`'s tracked-file check remains the owner's
//!   opt-in boundary, applied at publish time.
//! - **It does not touch Dolt repos.** [`Projection::discover`] is `None`
//!   there, and non-canonical stores (coordination `refs/agents/*`, personal
//!   `~/.rsry/personal.db`, ADR-0022) have no repo-rooted projection at all.

use std::path::Path;

use anyhow::Result;
use async_trait::async_trait;

use crate::bead::{Bead, BeadUpdate, Comment};
use crate::store::{BeadStore, NewBead};

pub mod commit;
pub mod push;
pub mod trunk;

#[cfg(test)]
mod tests;

/// What kind of projected write just happened.
///
/// Only the three *writing* kinds live here, because only they are ever
/// constructed. The full 34-method classification — including the read-only and
/// unprojected-write methods — is the concern of
/// `tests::every_trait_method_is_classified`, which owns the wider vocabulary.
/// Modelling "this method does nothing to the projection" as a runtime value
/// nobody constructs would be a comment wearing a type's clothes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Projected {
    /// Broadens the public id set by exactly the bead written. Only creates.
    Create,
    /// Changes a bead already in the projection; never adds one.
    Update,
    /// Mutates the projection but does not name the affected bead — the comment
    /// deletes, which key off `comment_id` and return nothing identifying the
    /// owner. At publish time only a whole-file refresh can carry these.
    Whole,
}

/// Marker: this store has a tracked, non-Dolt projection under a repo root.
///
/// Carries no path because the decorator no longer writes; the commit-msg hook
/// (P2) and the trunk refresh (P4) resolve the file from their own repo root.
struct Projection;

impl Projection {
    /// `None` for Dolt-backed stores (their projection is generated elsewhere)
    /// and for stores with no named repo directory above the beads dir
    /// (`:memory:`, the personal store).
    fn discover(beads_dir: &Path) -> Option<Self> {
        if crate::bead_backend::is_dolt_backed(beads_dir) {
            return None;
        }
        beads_dir.parent()?.file_name().map(|_| Self)
    }
}

/// A `BeadStore` that classifies every write against the tracked projection
/// without writing it. Publication is the commit hook's and the trunk's job.
pub struct PublishingBeadStore {
    inner: Box<dyn BeadStore>,
    /// `None` when there is no projection to classify against — a Dolt repo,
    /// or a store outside a repo (`:memory:`, the personal store). The wrapper
    /// is then a pure pass-through.
    projection: Option<Projection>,
}

impl PublishingBeadStore {
    /// Wrap `inner`, classifying writes against the projection implied by
    /// `beads_dir`.
    pub fn new(inner: Box<dyn BeadStore>, beads_dir: &Path) -> Self {
        Self {
            projection: Projection::discover(beads_dir),
            inner,
        }
    }

    /// Record what a write meant for the projection. Writes NOTHING.
    ///
    /// Kept as the single seam every projected method passes through: it is
    /// where the classification is enforced (a `Create`/`Update` that does not
    /// name its bead is a bug, not a silent skip). Under ADR-0024 amendment A
    /// the per-write behaviour is "none" — the file is written by
    /// `jsonl_sync::publish_ids` at commit and by
    /// `jsonl_sync::refresh_tracked_beads_jsonl` on the trunk.
    async fn publish(&self, kind: Projected, bead_id: Option<&str>) -> Result<()> {
        if self.projection.is_none() {
            return Ok(());
        }
        match (kind, bead_id) {
            (Projected::Create | Projected::Update, Some(_)) | (Projected::Whole, _) => Ok(()),
            (Projected::Create | Projected::Update, None) => {
                unreachable!("a projected bead write always names its bead")
            }
        }
    }
}

#[async_trait]
impl BeadStore for PublishingBeadStore {
    // --- Projected::Read -------------------------------------------------
    async fn list_beads(&self, repo_name: &str) -> Result<Vec<Bead>> {
        self.inner.list_beads(repo_name).await
    }
    async fn list_all_beads(&self, repo_name: &str) -> Result<Vec<Bead>> {
        self.inner.list_all_beads(repo_name).await
    }
    async fn list_beads_scoped(&self, repo_name: &str, user_id: Option<&str>) -> Result<Vec<Bead>> {
        self.inner.list_beads_scoped(repo_name, user_id).await
    }
    async fn get_bead(&self, id: &str, repo_name: &str) -> Result<Option<Bead>> {
        self.inner.get_bead(id, repo_name).await
    }
    async fn get_status(&self, id: &str) -> Result<Option<String>> {
        self.inner.get_status(id).await
    }
    async fn search_beads(&self, query: &str, repo_name: &str, limit: u32) -> Result<Vec<Bead>> {
        self.inner.search_beads(query, repo_name, limit).await
    }
    async fn search_beads_fts(
        &self,
        query: &str,
        repo_name: &str,
        limit: u32,
    ) -> Result<Vec<Bead>> {
        self.inner.search_beads_fts(query, repo_name, limit).await
    }
    async fn get_external_ref(&self, id: &str) -> Result<Option<String>> {
        self.inner.get_external_ref(id).await
    }
    async fn find_by_external_ref(&self, external_ref: &str) -> Result<Option<String>> {
        self.inner.find_by_external_ref(external_ref).await
    }
    async fn list_closed_linked_beads(&self, repo_name: &str) -> Result<Vec<Bead>> {
        self.inner.list_closed_linked_beads(repo_name).await
    }
    async fn get_dependencies(&self, issue_id: &str) -> Result<Vec<String>> {
        self.inner.get_dependencies(issue_id).await
    }
    async fn get_dependents(&self, issue_id: &str) -> Result<Vec<String>> {
        self.inner.get_dependents(issue_id).await
    }
    async fn get_children(&self, issue_id: &str) -> Result<Vec<String>> {
        self.inner.get_children(issue_id).await
    }
    async fn list_comments(&self, issue_id: &str, include_deleted: bool) -> Result<Vec<Comment>> {
        self.inner.list_comments(issue_id, include_deleted).await
    }
    async fn get_latest_event(&self, issue_id: &str, event_type: &str) -> Result<Option<String>> {
        self.inner.get_latest_event(issue_id, event_type).await
    }
    async fn list_event_details(&self, issue_id: &str, event_type: &str) -> Result<Vec<String>> {
        self.inner.list_event_details(issue_id, event_type).await
    }

    // --- Projected::Create -----------------------------------------------
    async fn create_bead(
        &self,
        id: &str,
        title: &str,
        description: &str,
        priority: u8,
        issue_type: &str,
    ) -> Result<()> {
        self.inner
            .create_bead(id, title, description, priority, issue_type)
            .await?;
        self.publish(Projected::Create, Some(id)).await
    }
    async fn create_bead_full(&self, bead: NewBead) -> Result<()> {
        let id = bead.id.clone();
        self.inner.create_bead_full(bead).await?;
        self.publish(Projected::Create, Some(&id)).await
    }

    // --- Projected::Update -----------------------------------------------
    async fn update_bead_fields(&self, id: &str, update: &BeadUpdate) -> Result<Vec<String>> {
        let changed = self.inner.update_bead_fields(id, update).await?;
        self.publish(Projected::Update, Some(id)).await?;
        Ok(changed)
    }
    async fn update_status(&self, id: &str, status: &str) -> Result<()> {
        self.inner.update_status(id, status).await?;
        self.publish(Projected::Update, Some(id)).await
    }
    /// A correction changes the record, so it is classified like any other
    /// update — the corrected status reaches the tracked export the next time
    /// the bead is named in a commit, not a stale one.
    async fn set_status_verbatim(&self, id: &str, status: &str) -> Result<()> {
        self.inner.set_status_verbatim(id, status).await?;
        self.publish(Projected::Update, Some(id)).await
    }
    async fn close_bead(&self, id: &str) -> Result<()> {
        self.inner.close_bead(id).await?;
        self.publish(Projected::Update, Some(id)).await
    }
    async fn set_assignee(&self, id: &str, assignee: &str) -> Result<()> {
        self.inner.set_assignee(id, assignee).await?;
        self.publish(Projected::Update, Some(id)).await
    }
    async fn set_user_id(&self, id: &str, user_id: &str) -> Result<()> {
        self.inner.set_user_id(id, user_id).await?;
        self.publish(Projected::Update, Some(id)).await
    }
    async fn set_files(&self, id: &str, files: &[String], test_files: &[String]) -> Result<()> {
        self.inner.set_files(id, files, test_files).await?;
        self.publish(Projected::Update, Some(id)).await
    }
    async fn set_external_ref(&self, id: &str, external_ref: &str) -> Result<()> {
        self.inner.set_external_ref(id, external_ref).await?;
        self.publish(Projected::Update, Some(id)).await
    }
    async fn add_dependency(&self, issue_id: &str, depends_on_id: &str) -> Result<()> {
        self.inner.add_dependency(issue_id, depends_on_id).await?;
        self.publish(Projected::Update, Some(issue_id)).await
    }
    async fn add_dependency_typed(
        &self,
        issue_id: &str,
        depends_on_id: &str,
        dep_type: &str,
    ) -> Result<()> {
        self.inner
            .add_dependency_typed(issue_id, depends_on_id, dep_type)
            .await?;
        self.publish(Projected::Update, Some(issue_id)).await
    }
    async fn remove_dependency(&self, issue_id: &str, depends_on_id: &str) -> Result<()> {
        self.inner
            .remove_dependency(issue_id, depends_on_id)
            .await?;
        self.publish(Projected::Update, Some(issue_id)).await
    }
    async fn add_comment(&self, issue_id: &str, body: &str, author: &str) -> Result<()> {
        self.inner.add_comment(issue_id, body, author).await?;
        self.publish(Projected::Update, Some(issue_id)).await
    }
    /// Keyed by `comment_id`, but the returned `Comment` carries `issue_id`, so
    /// the owning bead IS recoverable and this classifies as a named update
    /// rather than the whole-file kind its siblings need.
    async fn update_comment(
        &self,
        comment_id: &str,
        body: &str,
        reason: Option<&str>,
    ) -> Result<Comment> {
        let comment = self.inner.update_comment(comment_id, body, reason).await?;
        self.publish(Projected::Update, Some(&comment.issue_id))
            .await?;
        Ok(comment)
    }

    // --- Projected::Whole -------------------------------------------------
    // Keyed off `comment_id`, and unlike `update_comment` these return nothing
    // that names the owning bead — so at publish time only a whole-file refresh
    // (the trunk's P4) can carry them.
    async fn delete_comment(&self, comment_id: &str, reason: Option<&str>) -> Result<()> {
        self.inner.delete_comment(comment_id, reason).await?;
        self.publish(Projected::Whole, None).await
    }
    async fn hard_delete_comment(&self, comment_id: &str) -> Result<()> {
        self.inner.hard_delete_comment(comment_id).await?;
        self.publish(Projected::Whole, None).await
    }

    // --- Projected::UnprojectedWrite --------------------------------------
    async fn log_event(&self, issue_id: &str, event_type: &str, detail: &str) {
        self.inner.log_event(issue_id, event_type, detail).await;
    }
}
