//! Keep the git-tracked bead projection current by construction (rosary-8ca6e5).
//!
//! ## The defect this replaces
//!
//! `rosary-a7ee3a` built the bounded projection refresh correctly, then wired it
//! into exactly two operations — `bead create` and `bead close`, once each on
//! the CLI and MCP surfaces. Every other write path updated `.beads/beads.db`
//! and left `.beads/beads.jsonl` behind: `update`, `comment`, `link`, `import`,
//! the Linear and GitHub webhook writes, and — loudest — `persist_status`, which
//! is every dispatch state transition the reconciler makes.
//!
//! The cost was measured, not theorised: rosary carried 49 store-only beads and
//! cloister 30, all recovered by hand. A survey of all 20 registered repos found
//! three more still drifting (mache +14, agents +6, q-q-dev +2).
//!
//! ## Why a decorator, and why here
//!
//! The failure mode is not "someone wrote the refresh wrong". It is "someone
//! wrote a new write path and did not know a refresh existed". No amount of care
//! at the call site fixes that, because the call sites are the thing being
//! forgotten. So the refresh moves *underneath* every caller:
//! [`connect_bead_store`](crate::bead_sqlite::connect_bead_store) is already the
//! single entry point for all bead I/O, so wrapping its result covers all 50
//! current call sites — and, more to the point, every one not yet written.
//!
//! ## The gate is the compiler
//!
//! `BeadStore` has 33 methods and this type implements every one *by hand*.
//! There is deliberately no blanket forwarding impl and no `Deref`, because both
//! would let a 34th method appear and silently forward without anyone deciding
//! whether it mutates the projection. As written, adding a REQUIRED method to
//! the trait fails to compile until it is classified here.
//!
//! Caveat added by ADR-0021 slice 2 (`rosary-c7126b`): `create_bead` became a
//! *provided* method, and a provided method does not break this impl when added.
//! So the compile-time half of the guarantee covers required methods only. The
//! test half — `tests::every_trait_method_is_classified`, which reads the trait
//! body out of `store.rs` — covers both, and is what actually holds the line
//! now. Worth knowing before relying on "it won't compile" alone.
//!
//! That is the same property `src/parity` has, obtained more cheaply: a
//! mechanical check nobody has to remember to run, whose failure is a build
//! error rather than a report someone reads. The classification itself is a
//! judgement — [`Projected`] records it as data so it can be reviewed and
//! tested, rather than being implicit in which methods happen to call `mark`.
//!
//! ## What it deliberately does not do
//!
//! - **It does not create the projection.** An absent or untracked
//!   `.beads/beads.jsonl` stays absent; publication remains the repo owner's
//!   opt-in, exactly as `rosary-a7ee3a` established. The ten legacy repos with
//!   no export at all are `rosary-9c0e6c`, a migration, not this.
//! - **It does not publish non-canonical beads.** Coordination beads live in
//!   `refs/agents/*` and personal beads in `~/.rsry/personal.db` (ADR-0022); a
//!   store opened on those paths has no tracked projection, so the wrapper is
//!   inert there by construction rather than by a role check it could get wrong.
//! - **It does not touch Dolt repos.** Their projection is generated elsewhere.

use std::path::{Path, PathBuf};

use anyhow::Result;
use async_trait::async_trait;

use crate::bead::{Bead, BeadUpdate, Comment};
use crate::store::{BeadStore, NewBead};

#[cfg(test)]
mod tests;

/// What a projected write needs done to the tracked file.
///
/// Only the three *writing* kinds live here, because only they are ever
/// constructed. The full 33-method classification — including the read-only and
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
    /// owner. Falls back to the bounded whole-file refresh, which is correct and
    /// rare enough for the cost not to matter.
    Whole,
}

/// Where the tracked projection lives, and what to call the repo in it.
struct Projection {
    repo_root: PathBuf,
    repo_name: String,
}

impl Projection {
    /// Derive the projection from the resolved `.beads/` directory.
    ///
    /// `repo_root` is the PARENT of the resolved beads dir, not the caller's
    /// cwd. That matters under git worktrees: `resolve_beads_dir` follows
    /// `--git-common-dir` back to the main worktree, and the tracked JSONL lives
    /// there, so an agent working in a worktree publishes to the one real file
    /// instead of creating a second one nobody reads.
    fn discover(beads_dir: &Path) -> Option<Self> {
        if beads_dir.join("dolt").is_dir() {
            return None;
        }
        let repo_root = beads_dir.parent()?.to_path_buf();
        // Matches how every CLI call site derives it (`main.rs`): the repo
        // directory name. Only a label stamped onto each rendered record.
        let repo_name = repo_root.file_name()?.to_string_lossy().into_owned();
        Some(Self {
            repo_root,
            repo_name,
        })
    }
}

/// A `BeadStore` that keeps `.beads/beads.jsonl` in step with every write.
pub struct PublishingBeadStore {
    inner: Box<dyn BeadStore>,
    /// `None` when there is nothing to publish to — a Dolt repo, or a store
    /// outside a repo (`:memory:`, the personal store). The wrapper is then a
    /// pure pass-through.
    projection: Option<Projection>,
}

impl PublishingBeadStore {
    /// Wrap `inner`, publishing into the projection implied by `beads_dir`.
    pub fn new(inner: Box<dyn BeadStore>, beads_dir: &Path) -> Self {
        Self {
            projection: Projection::discover(beads_dir),
            inner,
        }
    }

    /// Republish after a write. Failures are surfaced, never swallowed: a
    /// silently-skipped publish is precisely the bug this module exists to end.
    async fn publish(&self, kind: Projected, bead_id: Option<&str>) -> Result<()> {
        let Some(projection) = &self.projection else {
            return Ok(());
        };
        match (kind, bead_id) {
            (Projected::Create | Projected::Update, Some(id)) => {
                crate::jsonl_sync::upsert_tracked_bead(
                    self.inner.as_ref(),
                    id,
                    &projection.repo_name,
                    &projection.repo_root,
                    kind == Projected::Create,
                )
                .await?;
            }
            (Projected::Whole, _) => {
                crate::jsonl_sync::refresh_tracked_beads_jsonl(
                    self.inner.as_ref(),
                    &projection.repo_name,
                    &projection.repo_root,
                )
                .await?;
            }
            (Projected::Create | Projected::Update, None) => {
                unreachable!("a projected bead write always names its bead")
            }
        }
        Ok(())
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
    /// the owning bead IS recoverable and this takes the precise path rather
    /// than the whole-file fallback its siblings need.
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
    // that names the owning bead — so the bounded whole-file refresh is the
    // only correct option. Rare enough that its cost does not matter.
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
