//! Backend store traits for rosary.
//!
//! Two trait families:
//!
//! **Orchestrator state** (cross-repo, single global DB):
//! - [`HierarchyStore`]: decades, threads, bead-to-thread membership
//! - [`DispatchStore`]: pipeline state, dispatch history, backoff
//! - [`LinkageStore`]: cross-repo dependencies, Linear linkage
//! - [`BackendStore`]: unified supertrait
//!
//! **Bead CRUD** (per-repo `.beads/` directory):
//! - [`BeadStore`]: create, read, update, search, close, comment, dependencies

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ── Data types ──────────────────────────────────────────

/// A reference to a bead across repos.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BeadRef {
    pub repo: String,
    pub bead_id: String,
}

/// Persistent record of a decade (ADR-level organizing primitive).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecadeRecord {
    /// e.g. "ADR-003"
    pub id: String,
    pub title: String,
    /// Path to the source ADR markdown file.
    pub source_path: String,
    /// proposed, active, completed, superseded
    pub status: String,
}

/// Persistent record of a thread within a decade.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadRecord {
    /// e.g. "ADR-003/implementation"
    pub id: String,
    pub name: String,
    pub decade_id: String,
    pub feature_branch: Option<String>,
}

/// Pipeline state for a single bead — replaces in-memory BeadTracker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PipelineState {
    pub bead_ref: BeadRef,
    /// Index into the agent sequence (dev=0, staging=1, prod=2, feature=3).
    pub pipeline_phase: u8,
    /// Current agent name (e.g. "dev-agent").
    pub pipeline_agent: String,
    /// Sub-state within the current phase. Eliminates ambiguity during recovery:
    /// - pending: phase selected, not yet dispatched
    /// - executing: agent spawned and running
    /// - completed: agent exited, verification passed
    /// - failed: agent exited, verification failed or timeout
    pub phase_status: String,
    pub retries: u32,
    pub consecutive_reverts: u32,
    pub highest_verify_tier: Option<u8>,
    /// Content hash — changes signal rescan needed.
    pub last_generation: u64,
    /// When this bead becomes eligible for retry after backoff.
    pub backoff_until: Option<DateTime<Utc>>,
}

/// Record of a single dispatch (agent execution).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DispatchRecord {
    /// UUID v4
    pub id: String,
    pub bead_ref: BeadRef,
    pub agent: String,
    /// claude, gemini, acp
    pub provider: String,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    /// success, failure, timeout
    pub outcome: Option<String>,
    pub work_dir: String,
    /// Claude Code session ID (from --output-format json). Enables --resume.
    pub session_id: Option<String>,
    /// jj workspace path (distinct from work_dir repo root).
    pub workspace_path: Option<String>,
}

/// Cross-repo dependency between beads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrossRepoDep {
    pub from: BeadRef,
    pub to: BeadRef,
    /// blocks, relates_to
    pub dep_type: String,
}

/// Mapping between a bead and its Linear representation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinearLink {
    pub bead_ref: BeadRef,
    /// e.g. "AGE-330"
    pub linear_id: String,
    /// issue, sub_issue, milestone
    pub linear_type: String,
}

// ── Traits ──────────────────────────────────────────────

/// Decades, threads, bead-to-thread membership.
/// Drives BDR accretion and Linear milestone/issue projection.
#[async_trait]
pub trait HierarchyStore: Send + Sync {
    async fn upsert_decade(&self, decade: &DecadeRecord) -> Result<()>;
    async fn get_decade(&self, id: &str) -> Result<Option<DecadeRecord>>;
    async fn list_decades(&self, status: Option<&str>) -> Result<Vec<DecadeRecord>>;

    async fn upsert_thread(&self, thread: &ThreadRecord) -> Result<()>;
    async fn list_threads(&self, decade_id: &str) -> Result<Vec<ThreadRecord>>;

    async fn add_bead_to_thread(&self, thread_id: &str, bead: &BeadRef) -> Result<()>;
    async fn list_beads_in_thread(&self, thread_id: &str) -> Result<Vec<BeadRef>>;
    async fn find_thread_for_bead(&self, bead: &BeadRef) -> Result<Option<String>>;
}

/// Pipeline state, dispatch history, backoff.
/// Replaces in-memory BeadTracker + SessionRegistry.
#[async_trait]
pub trait DispatchStore: Send + Sync {
    async fn upsert_pipeline(&self, state: &PipelineState) -> Result<()>;
    async fn get_pipeline(&self, bead: &BeadRef) -> Result<Option<PipelineState>>;
    async fn list_active_pipelines(&self) -> Result<Vec<PipelineState>>;
    async fn clear_pipeline(&self, bead: &BeadRef) -> Result<()>;

    async fn record_dispatch(&self, record: &DispatchRecord) -> Result<()>;
    /// Upsert a dispatch record (insert or update). Used by migration to handle
    /// both active and completed dispatches idempotently.
    async fn upsert_dispatch(&self, record: &DispatchRecord) -> Result<()>;
    async fn complete_dispatch(&self, id: &str, outcome: &str) -> Result<()>;
    /// Update the session_id on a dispatch record (captured after agent starts).
    async fn update_dispatch_session(&self, id: &str, session_id: &str) -> Result<()>;
    async fn active_dispatches(&self) -> Result<Vec<DispatchRecord>>;
}

/// Cross-repo dependencies and Linear linkage.
/// Replaces overloaded `external_ref` field and mirror-bead pattern.
#[async_trait]
pub trait LinkageStore: Send + Sync {
    async fn add_dependency(&self, dep: &CrossRepoDep) -> Result<()>;
    async fn dependencies_of(&self, bead: &BeadRef) -> Result<Vec<CrossRepoDep>>;
    async fn dependents_of(&self, bead: &BeadRef) -> Result<Vec<CrossRepoDep>>;

    async fn upsert_linear_link(&self, link: &LinearLink) -> Result<()>;
    async fn find_by_linear_id(&self, linear_id: &str) -> Result<Option<LinearLink>>;
}

/// Per-user repo registration for multi-tenant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserRepo {
    pub user_id: String,
    pub repo_url: String,
    pub repo_name: String,
    /// Reference to encrypted GitHub token in KV (not the token itself).
    pub github_token_ref: Option<String>,
}

/// Per-user repo registration store.
#[async_trait]
pub trait UserRepoStore: Send + Sync {
    async fn register_repo(&self, repo: &UserRepo) -> Result<()>;
    async fn list_user_repos(&self, user_id: &str) -> Result<Vec<UserRepo>>;
    async fn unregister_repo(&self, user_id: &str, repo_name: &str) -> Result<()>;
}

// ── Bead CRUD trait ──────────────────────────────────────

/// Per-repo bead storage — CRUD, search, dependencies, comments, events.
///
/// Each repo has its own BeadStore (SQLite file or Dolt server).
/// Implementations: [`crate::bead_sqlite::SqliteBeadStore`],
/// [`crate::bead_dolt::DoltBeadStore`].
#[async_trait]
pub trait BeadStore: Send + Sync {
    // ── CRUD ──
    async fn list_beads(&self, repo_name: &str) -> Result<Vec<crate::bead::Bead>>;
    async fn list_beads_scoped(
        &self,
        repo_name: &str,
        user_id: Option<&str>,
    ) -> Result<Vec<crate::bead::Bead>>;
    async fn get_bead(&self, id: &str, repo_name: &str) -> Result<Option<crate::bead::Bead>>;
    async fn create_bead(
        &self,
        id: &str,
        title: &str,
        description: &str,
        priority: u8,
        issue_type: &str,
    ) -> Result<()>;
    #[allow(clippy::too_many_arguments)]
    async fn create_bead_full(
        &self,
        id: &str,
        title: &str,
        description: &str,
        priority: u8,
        issue_type: &str,
        owner: &str,
        files: &[String],
        test_files: &[String],
        depends_on: &[String],
    ) -> Result<()>;

    // ── Field updates ──
    async fn update_bead_fields(
        &self,
        id: &str,
        update: &crate::bead::BeadUpdate,
    ) -> Result<Vec<String>>;
    async fn update_status(&self, id: &str, status: &str) -> Result<()>;
    async fn get_status(&self, id: &str) -> Result<Option<String>>;
    async fn close_bead(&self, id: &str) -> Result<()>;
    async fn set_assignee(&self, id: &str, assignee: &str) -> Result<()>;
    async fn set_user_id(&self, id: &str, user_id: &str) -> Result<()>;
    async fn set_files(&self, id: &str, files: &[String], test_files: &[String]) -> Result<()>;

    // ── Search ──
    async fn search_beads(
        &self,
        query: &str,
        repo_name: &str,
        limit: u32,
    ) -> Result<Vec<crate::bead::Bead>>;

    // ── External references (Linear linkage) ──
    async fn get_external_ref(&self, id: &str) -> Result<Option<String>>;
    async fn set_external_ref(&self, id: &str, external_ref: &str) -> Result<()>;
    async fn find_by_external_ref(&self, external_ref: &str) -> Result<Option<String>>;
    async fn list_closed_linked_beads(&self, repo_name: &str) -> Result<Vec<crate::bead::Bead>>;

    // ── Dependencies ──
    async fn add_dependency(&self, issue_id: &str, depends_on_id: &str) -> Result<()>;
    async fn remove_dependency(&self, issue_id: &str, depends_on_id: &str) -> Result<()>;
    async fn get_dependencies(&self, issue_id: &str) -> Result<Vec<String>>;
    async fn get_dependents(&self, issue_id: &str) -> Result<Vec<String>>;

    // ── Comments & events ──
    async fn add_comment(&self, issue_id: &str, body: &str, author: &str) -> Result<()>;
    /// Best-effort audit log. Implementations should warn on failure, not error.
    async fn log_event(&self, issue_id: &str, event_type: &str, detail: &str);
    /// Most recent event detail for a bead + event type.
    async fn get_latest_event(&self, issue_id: &str, event_type: &str) -> Result<Option<String>>;
}

// ── Composite traits ───────────────────────────────────

/// Unified supertrait — single trait object for all orchestrator state.
pub trait BackendStore: HierarchyStore + DispatchStore + LinkageStore + UserRepoStore {}

/// Blanket impl: anything implementing all four traits is a BackendStore.
impl<T: HierarchyStore + DispatchStore + LinkageStore + UserRepoStore> BackendStore for T {}

/// Bulk export — used by migration and backup.
/// Separate from the main store traits to keep them focused.
#[async_trait]
pub trait BackendExport: BackendStore {
    async fn all_threads(&self) -> Result<Vec<ThreadRecord>>;
    async fn all_thread_members(&self) -> Result<Vec<(String, BeadRef)>>;
    async fn all_dispatches(&self) -> Result<Vec<DispatchRecord>>;
    async fn all_dependencies(&self) -> Result<Vec<CrossRepoDep>>;
    async fn all_linear_links(&self) -> Result<Vec<LinearLink>>;
    async fn all_user_repos(&self) -> Result<Vec<UserRepo>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// In-memory implementation of all three store traits for testing.
    struct InMemoryStore {
        decades: Mutex<Vec<DecadeRecord>>,
        threads: Mutex<Vec<ThreadRecord>>,
        /// (thread_id, beads)
        thread_members: Mutex<Vec<(String, BeadRef)>>,
        pipelines: Mutex<Vec<PipelineState>>,
        dispatches: Mutex<Vec<DispatchRecord>>,
        deps: Mutex<Vec<CrossRepoDep>>,
        linear_links: Mutex<Vec<LinearLink>>,
    }

    impl InMemoryStore {
        fn new() -> Self {
            Self {
                decades: Mutex::new(Vec::new()),
                threads: Mutex::new(Vec::new()),
                thread_members: Mutex::new(Vec::new()),
                pipelines: Mutex::new(Vec::new()),
                dispatches: Mutex::new(Vec::new()),
                deps: Mutex::new(Vec::new()),
                linear_links: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl HierarchyStore for InMemoryStore {
        async fn upsert_decade(&self, decade: &DecadeRecord) -> Result<()> {
            let mut decades = self.decades.lock().unwrap();
            if let Some(existing) = decades.iter_mut().find(|d| d.id == decade.id) {
                *existing = decade.clone();
            } else {
                decades.push(decade.clone());
            }
            Ok(())
        }

        async fn get_decade(&self, id: &str) -> Result<Option<DecadeRecord>> {
            let decades = self.decades.lock().unwrap();
            Ok(decades.iter().find(|d| d.id == id).cloned())
        }

        async fn list_decades(&self, status: Option<&str>) -> Result<Vec<DecadeRecord>> {
            let decades = self.decades.lock().unwrap();
            Ok(match status {
                Some(s) => decades.iter().filter(|d| d.status == s).cloned().collect(),
                None => decades.clone(),
            })
        }

        async fn upsert_thread(&self, thread: &ThreadRecord) -> Result<()> {
            let mut threads = self.threads.lock().unwrap();
            if let Some(existing) = threads.iter_mut().find(|t| t.id == thread.id) {
                *existing = thread.clone();
            } else {
                threads.push(thread.clone());
            }
            Ok(())
        }

        async fn list_threads(&self, decade_id: &str) -> Result<Vec<ThreadRecord>> {
            let threads = self.threads.lock().unwrap();
            Ok(threads
                .iter()
                .filter(|t| t.decade_id == decade_id)
                .cloned()
                .collect())
        }

        async fn add_bead_to_thread(&self, thread_id: &str, bead: &BeadRef) -> Result<()> {
            let mut members = self.thread_members.lock().unwrap();
            if !members.iter().any(|(tid, b)| tid == thread_id && b == bead) {
                members.push((thread_id.to_string(), bead.clone()));
            }
            Ok(())
        }

        async fn list_beads_in_thread(&self, thread_id: &str) -> Result<Vec<BeadRef>> {
            let members = self.thread_members.lock().unwrap();
            Ok(members
                .iter()
                .filter(|(tid, _)| tid == thread_id)
                .map(|(_, b)| b.clone())
                .collect())
        }

        async fn find_thread_for_bead(&self, bead: &BeadRef) -> Result<Option<String>> {
            let members = self.thread_members.lock().unwrap();
            Ok(members
                .iter()
                .find(|(_, b)| b == bead)
                .map(|(tid, _)| tid.clone()))
        }
    }

    #[async_trait]
    impl DispatchStore for InMemoryStore {
        async fn upsert_pipeline(&self, state: &PipelineState) -> Result<()> {
            let mut pipelines = self.pipelines.lock().unwrap();
            if let Some(existing) = pipelines.iter_mut().find(|p| p.bead_ref == state.bead_ref) {
                *existing = state.clone();
            } else {
                pipelines.push(state.clone());
            }
            Ok(())
        }

        async fn get_pipeline(&self, bead: &BeadRef) -> Result<Option<PipelineState>> {
            let pipelines = self.pipelines.lock().unwrap();
            Ok(pipelines.iter().find(|p| &p.bead_ref == bead).cloned())
        }

        async fn list_active_pipelines(&self) -> Result<Vec<PipelineState>> {
            let pipelines = self.pipelines.lock().unwrap();
            Ok(pipelines.clone())
        }

        async fn clear_pipeline(&self, bead: &BeadRef) -> Result<()> {
            let mut pipelines = self.pipelines.lock().unwrap();
            pipelines.retain(|p| &p.bead_ref != bead);
            Ok(())
        }

        async fn record_dispatch(&self, record: &DispatchRecord) -> Result<()> {
            let mut dispatches = self.dispatches.lock().unwrap();
            dispatches.push(record.clone());
            Ok(())
        }

        async fn upsert_dispatch(&self, record: &DispatchRecord) -> Result<()> {
            let mut dispatches = self.dispatches.lock().unwrap();
            if let Some(existing) = dispatches.iter_mut().find(|d| d.id == record.id) {
                *existing = record.clone();
            } else {
                dispatches.push(record.clone());
            }
            Ok(())
        }

        async fn complete_dispatch(&self, id: &str, outcome: &str) -> Result<()> {
            let mut dispatches = self.dispatches.lock().unwrap();
            if let Some(d) = dispatches.iter_mut().find(|d| d.id == id) {
                d.completed_at = Some(Utc::now());
                d.outcome = Some(outcome.to_string());
            }
            Ok(())
        }

        async fn update_dispatch_session(&self, id: &str, session_id: &str) -> Result<()> {
            let mut dispatches = self.dispatches.lock().unwrap();
            if let Some(d) = dispatches.iter_mut().find(|d| d.id == id) {
                d.session_id = Some(session_id.to_string());
            }
            Ok(())
        }

        async fn active_dispatches(&self) -> Result<Vec<DispatchRecord>> {
            let dispatches = self.dispatches.lock().unwrap();
            Ok(dispatches
                .iter()
                .filter(|d| d.completed_at.is_none())
                .cloned()
                .collect())
        }
    }

    #[async_trait]
    impl LinkageStore for InMemoryStore {
        async fn add_dependency(&self, dep: &CrossRepoDep) -> Result<()> {
            let mut deps = self.deps.lock().unwrap();
            if !deps.iter().any(|d| d.from == dep.from && d.to == dep.to) {
                deps.push(dep.clone());
            }
            Ok(())
        }

        async fn dependencies_of(&self, bead: &BeadRef) -> Result<Vec<CrossRepoDep>> {
            let deps = self.deps.lock().unwrap();
            Ok(deps.iter().filter(|d| &d.from == bead).cloned().collect())
        }

        async fn dependents_of(&self, bead: &BeadRef) -> Result<Vec<CrossRepoDep>> {
            let deps = self.deps.lock().unwrap();
            Ok(deps.iter().filter(|d| &d.to == bead).cloned().collect())
        }

        async fn upsert_linear_link(&self, link: &LinearLink) -> Result<()> {
            let mut links = self.linear_links.lock().unwrap();
            if let Some(existing) = links.iter_mut().find(|l| l.bead_ref == link.bead_ref) {
                *existing = link.clone();
            } else {
                links.push(link.clone());
            }
            Ok(())
        }

        async fn find_by_linear_id(&self, linear_id: &str) -> Result<Option<LinearLink>> {
            let links = self.linear_links.lock().unwrap();
            Ok(links.iter().find(|l| l.linear_id == linear_id).cloned())
        }
    }

    // ── HierarchyStore tests ────────────────────────────

    #[tokio::test]
    async fn decade_upsert_and_get() {
        let store = InMemoryStore::new();
        let decade = DecadeRecord {
            id: "ADR-003".into(),
            title: "Linear hierarchy mapping".into(),
            source_path: "docs/adr/0003-linear-hierarchy-mapping.md".into(),
            status: "proposed".into(),
        };
        store.upsert_decade(&decade).await.unwrap();

        let got = store.get_decade("ADR-003").await.unwrap();
        assert_eq!(got, Some(decade.clone()));

        // Upsert updates existing
        let mut updated = decade;
        updated.status = "active".into();
        store.upsert_decade(&updated).await.unwrap();
        let got = store.get_decade("ADR-003").await.unwrap().unwrap();
        assert_eq!(got.status, "active");
    }

    #[tokio::test]
    async fn decade_list_with_filter() {
        let store = InMemoryStore::new();
        store
            .upsert_decade(&DecadeRecord {
                id: "ADR-001".into(),
                title: "A".into(),
                source_path: "a.md".into(),
                status: "active".into(),
            })
            .await
            .unwrap();
        store
            .upsert_decade(&DecadeRecord {
                id: "ADR-002".into(),
                title: "B".into(),
                source_path: "b.md".into(),
                status: "proposed".into(),
            })
            .await
            .unwrap();

        let all = store.list_decades(None).await.unwrap();
        assert_eq!(all.len(), 2);

        let active = store.list_decades(Some("active")).await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, "ADR-001");
    }

    #[tokio::test]
    async fn get_nonexistent_decade() {
        let store = InMemoryStore::new();
        let got = store.get_decade("nope").await.unwrap();
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn thread_upsert_and_list() {
        let store = InMemoryStore::new();
        let thread = ThreadRecord {
            id: "ADR-003/implementation".into(),
            name: "Linear hierarchy: Implementation".into(),
            decade_id: "ADR-003".into(),
            feature_branch: Some("feat/linear-hierarchy".into()),
        };
        store.upsert_thread(&thread).await.unwrap();

        let threads = store.list_threads("ADR-003").await.unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0], thread);

        // Other decade has no threads
        let empty = store.list_threads("ADR-999").await.unwrap();
        assert!(empty.is_empty());
    }

    #[tokio::test]
    async fn bead_thread_membership() {
        let store = InMemoryStore::new();
        let bead1 = BeadRef {
            repo: "rosary".into(),
            bead_id: "rsry-abc".into(),
        };
        let bead2 = BeadRef {
            repo: "mache".into(),
            bead_id: "mch-def".into(),
        };

        store
            .add_bead_to_thread("ADR-003/impl", &bead1)
            .await
            .unwrap();
        store
            .add_bead_to_thread("ADR-003/impl", &bead2)
            .await
            .unwrap();
        // Idempotent
        store
            .add_bead_to_thread("ADR-003/impl", &bead1)
            .await
            .unwrap();

        let members = store.list_beads_in_thread("ADR-003/impl").await.unwrap();
        assert_eq!(members.len(), 2);

        let found = store.find_thread_for_bead(&bead1).await.unwrap();
        assert_eq!(found, Some("ADR-003/impl".into()));

        let not_found = store
            .find_thread_for_bead(&BeadRef {
                repo: "x".into(),
                bead_id: "y".into(),
            })
            .await
            .unwrap();
        assert!(not_found.is_none());
    }

    // ── DispatchStore tests ─────────────────────────────

    #[tokio::test]
    async fn pipeline_lifecycle() {
        let store = InMemoryStore::new();
        let bead = BeadRef {
            repo: "rosary".into(),
            bead_id: "rsry-001".into(),
        };
        let state = PipelineState {
            bead_ref: bead.clone(),
            pipeline_phase: 0,
            pipeline_agent: "dev-agent".into(),
            phase_status: "executing".into(),
            retries: 0,
            consecutive_reverts: 0,
            highest_verify_tier: None,
            last_generation: 42,
            backoff_until: None,
        };

        // Upsert + get
        store.upsert_pipeline(&state).await.unwrap();
        let got = store.get_pipeline(&bead).await.unwrap().unwrap();
        assert_eq!(got.pipeline_phase, 0);
        assert_eq!(got.last_generation, 42);

        // Update phase
        let mut advanced = state.clone();
        advanced.pipeline_phase = 1;
        advanced.pipeline_agent = "staging-agent".into();
        store.upsert_pipeline(&advanced).await.unwrap();
        let got = store.get_pipeline(&bead).await.unwrap().unwrap();
        assert_eq!(got.pipeline_phase, 1);

        // List active
        let active = store.list_active_pipelines().await.unwrap();
        assert_eq!(active.len(), 1);

        // Clear
        store.clear_pipeline(&bead).await.unwrap();
        assert!(store.get_pipeline(&bead).await.unwrap().is_none());
        assert!(store.list_active_pipelines().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn dispatch_record_and_complete() {
        let store = InMemoryStore::new();
        let record = DispatchRecord {
            id: "d-001".into(),
            bead_ref: BeadRef {
                repo: "rosary".into(),
                bead_id: "rsry-001".into(),
            },
            agent: "dev-agent".into(),
            provider: "claude".into(),
            started_at: Utc::now(),
            completed_at: None,
            outcome: None,
            work_dir: "/tmp/work".into(),
            session_id: None,
            workspace_path: None,
        };

        store.record_dispatch(&record).await.unwrap();

        let active = store.active_dispatches().await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, "d-001");

        store.complete_dispatch("d-001", "success").await.unwrap();

        let active = store.active_dispatches().await.unwrap();
        assert!(active.is_empty());
    }

    #[tokio::test]
    async fn dispatch_update_session_id() {
        let store = InMemoryStore::new();
        let record = DispatchRecord {
            id: "d-002".into(),
            bead_ref: BeadRef {
                repo: "rosary".into(),
                bead_id: "rsry-002".into(),
            },
            agent: "dev-agent".into(),
            provider: "claude".into(),
            started_at: Utc::now(),
            completed_at: None,
            outcome: None,
            work_dir: "/tmp/work".into(),
            session_id: None,
            workspace_path: Some("/tmp/.rsry-workspaces/rsry-002".into()),
        };

        store.record_dispatch(&record).await.unwrap();

        // Session ID not set yet
        let active = store.active_dispatches().await.unwrap();
        assert!(active[0].session_id.is_none());
        assert_eq!(
            active[0].workspace_path.as_deref(),
            Some("/tmp/.rsry-workspaces/rsry-002")
        );

        // Update session_id after agent starts
        store
            .update_dispatch_session("d-002", "sess-abc-123")
            .await
            .unwrap();

        let active = store.active_dispatches().await.unwrap();
        assert_eq!(active[0].session_id.as_deref(), Some("sess-abc-123"));
    }

    #[tokio::test]
    async fn upsert_dispatch_idempotent() {
        let store = InMemoryStore::new();
        let record = DispatchRecord {
            id: "d-upsert".into(),
            bead_ref: BeadRef {
                repo: "rosary".into(),
                bead_id: "rsry-001".into(),
            },
            agent: "dev-agent".into(),
            provider: "claude".into(),
            started_at: Utc::now(),
            completed_at: None,
            outcome: None,
            work_dir: "/tmp/work".into(),
            session_id: None,
            workspace_path: None,
        };

        // Insert via upsert
        store.upsert_dispatch(&record).await.unwrap();
        assert_eq!(store.active_dispatches().await.unwrap().len(), 1);

        // Upsert again with completion — updates, doesn't duplicate
        let mut completed = record.clone();
        completed.completed_at = Some(Utc::now());
        completed.outcome = Some("success".into());
        store.upsert_dispatch(&completed).await.unwrap();

        // Still one dispatch, now completed
        assert!(store.active_dispatches().await.unwrap().is_empty());
    }

    // ── LinkageStore tests ──────────────────────────────

    #[tokio::test]
    async fn cross_repo_dependency() {
        let store = InMemoryStore::new();
        let from = BeadRef {
            repo: "rosary".into(),
            bead_id: "rsry-001".into(),
        };
        let to = BeadRef {
            repo: "mache".into(),
            bead_id: "mch-001".into(),
        };

        let dep = CrossRepoDep {
            from: from.clone(),
            to: to.clone(),
            dep_type: "blocks".into(),
        };
        store.add_dependency(&dep).await.unwrap();
        // Idempotent
        store.add_dependency(&dep).await.unwrap();

        let deps = store.dependencies_of(&from).await.unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].to, to);

        let dependents = store.dependents_of(&to).await.unwrap();
        assert_eq!(dependents.len(), 1);
        assert_eq!(dependents[0].from, from);
    }

    #[tokio::test]
    async fn linear_link_upsert_and_find() {
        let store = InMemoryStore::new();
        let link = LinearLink {
            bead_ref: BeadRef {
                repo: "rosary".into(),
                bead_id: "rsry-001".into(),
            },
            linear_id: "AGE-330".into(),
            linear_type: "issue".into(),
        };

        store.upsert_linear_link(&link).await.unwrap();

        let found = store.find_by_linear_id("AGE-330").await.unwrap();
        assert_eq!(found, Some(link.clone()));

        let not_found = store.find_by_linear_id("AGE-999").await.unwrap();
        assert!(not_found.is_none());

        // Upsert changes type
        let mut updated = link;
        updated.linear_type = "sub_issue".into();
        store.upsert_linear_link(&updated).await.unwrap();
        let found = store.find_by_linear_id("AGE-330").await.unwrap().unwrap();
        assert_eq!(found.linear_type, "sub_issue");
    }
}
