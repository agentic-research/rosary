use std::collections::hash_map::DefaultHasher;
use std::fmt;
use std::hash::{Hash, Hasher};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Bead lifecycle states — modeled as a Labeled Transition System.
///
/// Transitions:
///   backlog → open (human/agent promotes after refinement)
///   open → queued (triage selects)
///   queued → dispatched (semaphore acquired)
///   dispatched → verifying (agent exits)
///   verifying → done (all tiers pass)
///   verifying → rejected (tier fails)
///   verifying → blocked (needs human / partial)
///   rejected → open (retry after backoff)
///   blocked → open (dependency resolved / manual unblock)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BeadState {
    /// Refinement zone — human + agent shape the work. Never auto-dispatched.
    Backlog,
    Open,
    Queued,
    Dispatched,
    Verifying,
    Done,
    Rejected,
    Blocked,
    Stale,
}

impl BeadState {
    /// Valid successor states from this state.
    #[allow(dead_code)] // API surface — used in tests, will be used for transition validation
    pub fn valid_transitions(self) -> &'static [BeadState] {
        match self {
            BeadState::Backlog => &[BeadState::Open],
            BeadState::Open => &[BeadState::Queued],
            BeadState::Queued => &[BeadState::Dispatched],
            BeadState::Dispatched => &[BeadState::Verifying],
            BeadState::Verifying => &[BeadState::Done, BeadState::Rejected, BeadState::Blocked],
            BeadState::Rejected => &[BeadState::Open],
            BeadState::Blocked => &[BeadState::Open],
            BeadState::Done => &[],
            BeadState::Stale => &[BeadState::Open],
        }
    }

    /// Check if transitioning to `next` is valid.
    #[allow(dead_code)]
    pub fn can_transition_to(self, next: BeadState) -> bool {
        self.valid_transitions().contains(&next)
    }

    /// Whether this state is terminal (no further transitions).
    #[allow(dead_code)]
    pub fn is_terminal(self) -> bool {
        self.valid_transitions().is_empty()
    }

    /// Map bead state to a Linear state type + preferred name.
    /// Returns (type, preferred_name) — the tracker resolves to an actual state ID.
    /// Type is stable across all Linear teams; name is a hint for teams that have it.
    pub fn to_linear_type(self) -> (&'static str, &'static str) {
        match self {
            BeadState::Backlog => ("backlog", "Backlog"),
            BeadState::Open | BeadState::Rejected | BeadState::Stale => ("unstarted", "Todo"),
            BeadState::Queued => ("unstarted", "Todo"),
            BeadState::Dispatched => ("started", "In Progress"),
            BeadState::Verifying => ("started", "In Review"),
            BeadState::Done => ("completed", "Done"),
            BeadState::Blocked => ("backlog", "Backlog"),
        }
    }

    /// Map a Linear state type to a BeadState.
    /// Type-based mapping is stable across all Linear configurations.
    /// Optional name hint refines within a type (e.g., "In Review" → Verifying
    /// vs "In Progress" → Dispatched, both type=started).
    pub fn from_linear_type(state_type: &str, state_name: &str) -> Self {
        match state_type {
            "completed" => BeadState::Done,
            "canceled" => BeadState::Done,
            "started" => {
                // Refine by name within the "started" type
                if state_name.to_lowercase().contains("review") {
                    BeadState::Verifying
                } else {
                    BeadState::Dispatched
                }
            }
            "backlog" => BeadState::Backlog,
            "unstarted" => BeadState::Open,
            _ => BeadState::Open,
        }
    }
}

impl fmt::Display for BeadState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            BeadState::Backlog => "backlog",
            BeadState::Open => "open",
            BeadState::Queued => "queued",
            BeadState::Dispatched => "dispatched",
            BeadState::Verifying => "verifying",
            BeadState::Done => "done",
            BeadState::Rejected => "rejected",
            BeadState::Blocked => "blocked",
            BeadState::Stale => "stale",
        };
        write!(f, "{s}")
    }
}

impl From<&str> for BeadState {
    fn from(s: &str) -> Self {
        match s {
            "backlog" => BeadState::Backlog,
            "open" => BeadState::Open,
            "queued" => BeadState::Queued,
            "dispatched" => BeadState::Dispatched,
            "verifying" => BeadState::Verifying,
            "done" | "closed" => BeadState::Done,
            "rejected" => BeadState::Rejected,
            "blocked" => BeadState::Blocked,
            "stale" => BeadState::Stale,
            "in_progress" => BeadState::Dispatched, // legacy mapping
            _ => BeadState::Open,
        }
    }
}

/// All bead types require scopes (files or directories) for overlap detection.
///
/// Files: `src/reconcile.rs` (exact path, no trailing slash)
/// Directories: `crates/bdr/` or `src/` (trailing slash = prefix match)
/// Repo-wide: `./` (blocks all dispatch in that repo — use sparingly)
///
/// This enables parallel dispatch: beads with non-overlapping scopes can
/// run concurrently, while overlapping scopes serialize execution.
pub fn requires_files(_issue_type: &str) -> bool {
    true
}

/// PATCH-style update for bead fields. Only `Some` fields are written;
/// `None` fields are left unchanged. Used by `rsry_bead_update` MCP tool
/// and the `IssueTracker::update_fields` trait method.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BeadUpdate {
    pub title: Option<String>,
    pub description: Option<String>,
    pub priority: Option<u8>,
    pub issue_type: Option<String>,
    pub owner: Option<String>,
    pub files: Option<Vec<String>>,
    pub test_files: Option<Vec<String>>,
}

impl BeadUpdate {
    /// Returns true if no fields are set (nothing to update).
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.description.is_none()
            && self.priority.is_none()
            && self.issue_type.is_none()
            && self.owner.is_none()
            && self.files.is_none()
            && self.test_files.is_none()
    }
}

/// A bead is a file-scoped work item tracked in a repo's .beads/ directory.
/// This is the common representation used across scanner, sync, and dispatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bead {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub priority: u8,
    pub issue_type: String,
    pub owner: Option<String>,
    pub repo: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub dependency_count: u32,
    pub dependent_count: u32,
    pub comment_count: u32,
    /// Git branch or jj bookmark name associated with this bead.
    pub branch: Option<String>,
    /// GitHub/GitLab PR URL associated with this bead.
    pub pr_url: Option<String>,
    /// jj change ID (immutable, preferred over branch for jj workflows).
    pub jj_change_id: Option<String>,
    /// External reference for cross-repo tracking (e.g., "kiln:ll-packaging").
    /// Format: "repo_name:label" — repo_name maps to a repo in rosary.toml.
    pub external_ref: Option<String>,
    /// Source files this bead touches (scopes agent dispatch).
    #[serde(default)]
    pub files: Vec<String>,
    /// Test files to validate the change.
    #[serde(default)]
    pub test_files: Vec<String>,
}

impl Bead {
    /// Content-based generation hash. Changes when semantic content changes,
    /// but not when status/timestamps change. Used for idempotency —
    /// if generation matches last processed, skip re-dispatch.
    pub fn generation(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.id.hash(&mut hasher);
        self.title.hash(&mut hasher);
        self.description.hash(&mut hasher);
        self.priority.hash(&mut hasher);
        hasher.finish()
    }

    /// Parse the status string into a typed BeadState.
    pub fn state(&self) -> BeadState {
        BeadState::from(self.status.as_str())
    }

    /// Parse from `bd list --json` output
    #[allow(dead_code)] // used in tests and future CLI integration
    pub fn from_bd_json(value: &serde_json::Value, repo: &str) -> Option<Self> {
        Some(Bead {
            id: value.get("id")?.as_str()?.to_string(),
            title: value.get("title")?.as_str()?.to_string(),
            description: value
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            status: value
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("open")
                .to_string(),
            priority: value.get("priority").and_then(|v| v.as_u64()).unwrap_or(2) as u8,
            issue_type: value
                .get("issue_type")
                .and_then(|v| v.as_str())
                .unwrap_or("task")
                .to_string(),
            owner: value
                .get("owner")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            repo: repo.to_string(),
            created_at: parse_datetime(value.get("created_at")),
            updated_at: parse_datetime(value.get("updated_at")),
            dependency_count: value
                .get("dependency_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            dependent_count: value
                .get("dependent_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            comment_count: value
                .get("comment_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            branch: value
                .get("branch")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            pr_url: value
                .get("pr_url")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            jj_change_id: value
                .get("jj_change_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            external_ref: value
                .get("external_ref")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            files: value
                .get("files")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
            test_files: value
                .get("test_files")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    pub fn is_ready(&self) -> bool {
        self.status == "open" && self.dependency_count == 0
    }

    /// A bead is blocked if it has unresolved dependencies OR its status is explicitly "blocked".
    /// This is the single definition — used by both status counts and list filtering.
    pub fn is_blocked(&self) -> bool {
        self.status == "blocked" || (self.status == "open" && self.dependency_count > 0)
    }
}

impl fmt::Display for Bead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {} ({})", self.id, self.title, self.status,)?;
        if let Some(ref branch) = self.branch {
            write!(f, " branch={branch}")?;
        }
        if let Some(ref pr_url) = self.pr_url {
            write!(f, " pr={pr_url}")?;
        }
        if let Some(ref jj_id) = self.jj_change_id {
            write!(f, " jj={jj_id}")?;
        }
        Ok(())
    }
}

#[allow(dead_code)]
fn parse_datetime(v: Option<&serde_json::Value>) -> DateTime<Utc> {
    v.and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_bd_json_output() {
        let val = json!({
            "id": "mache-tgl",
            "title": "[graphfs.go] Replace interface{} with any",
            "description": "Trivial modernization",
            "status": "open",
            "priority": 2,
            "issue_type": "chore",
            "owner": "jamestexas",
            "created_at": "2026-03-12T22:13:27Z",
            "updated_at": "2026-03-12T22:13:27Z",
            "dependency_count": 0,
            "dependent_count": 0,
            "comment_count": 0
        });

        let bead = Bead::from_bd_json(&val, "mache").unwrap();
        assert_eq!(bead.id, "mache-tgl");
        assert_eq!(bead.repo, "mache");
        assert!(bead.is_ready());
    }

    #[test]
    fn state_from_string() {
        assert_eq!(BeadState::from("backlog"), BeadState::Backlog);
        assert_eq!(BeadState::from("open"), BeadState::Open);
        assert_eq!(BeadState::from("queued"), BeadState::Queued);
        assert_eq!(BeadState::from("dispatched"), BeadState::Dispatched);
        assert_eq!(BeadState::from("verifying"), BeadState::Verifying);
        assert_eq!(BeadState::from("done"), BeadState::Done);
        assert_eq!(BeadState::from("closed"), BeadState::Done);
        assert_eq!(BeadState::from("rejected"), BeadState::Rejected);
        assert_eq!(BeadState::from("blocked"), BeadState::Blocked);
        assert_eq!(BeadState::from("stale"), BeadState::Stale);
        assert_eq!(BeadState::from("in_progress"), BeadState::Dispatched);
        assert_eq!(BeadState::from("garbage"), BeadState::Open);
    }

    #[test]
    fn state_display_roundtrip() {
        let states = [
            BeadState::Backlog,
            BeadState::Open,
            BeadState::Queued,
            BeadState::Dispatched,
            BeadState::Verifying,
            BeadState::Done,
            BeadState::Rejected,
            BeadState::Blocked,
            BeadState::Stale,
        ];
        for state in states {
            let s = state.to_string();
            assert_eq!(BeadState::from(s.as_str()), state);
        }
    }

    #[test]
    fn valid_transitions() {
        assert!(BeadState::Backlog.can_transition_to(BeadState::Open));
        assert!(!BeadState::Backlog.can_transition_to(BeadState::Done));

        assert!(BeadState::Open.can_transition_to(BeadState::Queued));
        assert!(!BeadState::Open.can_transition_to(BeadState::Done));

        assert!(BeadState::Queued.can_transition_to(BeadState::Dispatched));
        assert!(!BeadState::Queued.can_transition_to(BeadState::Open));

        assert!(BeadState::Dispatched.can_transition_to(BeadState::Verifying));
        assert!(!BeadState::Dispatched.can_transition_to(BeadState::Done));

        assert!(BeadState::Verifying.can_transition_to(BeadState::Done));
        assert!(BeadState::Verifying.can_transition_to(BeadState::Rejected));
        assert!(BeadState::Verifying.can_transition_to(BeadState::Blocked));

        assert!(BeadState::Rejected.can_transition_to(BeadState::Open));
        assert!(!BeadState::Rejected.can_transition_to(BeadState::Done));

        assert!(BeadState::Done.is_terminal());
    }

    #[test]
    fn to_linear_type_mapping() {
        assert_eq!(BeadState::Backlog.to_linear_type(), ("backlog", "Backlog"));
        assert_eq!(BeadState::Open.to_linear_type(), ("unstarted", "Todo"));
        assert_eq!(BeadState::Queued.to_linear_type(), ("unstarted", "Todo"));
        assert_eq!(
            BeadState::Dispatched.to_linear_type(),
            ("started", "In Progress")
        );
        assert_eq!(
            BeadState::Verifying.to_linear_type(),
            ("started", "In Review")
        );
        assert_eq!(BeadState::Done.to_linear_type(), ("completed", "Done"));
        assert_eq!(BeadState::Blocked.to_linear_type(), ("backlog", "Backlog"));
        assert_eq!(BeadState::Rejected.to_linear_type(), ("unstarted", "Todo"));
        assert_eq!(BeadState::Stale.to_linear_type(), ("unstarted", "Todo"));
    }

    #[test]
    fn from_linear_type_mapping() {
        assert_eq!(
            BeadState::from_linear_type("completed", "Done"),
            BeadState::Done
        );
        assert_eq!(
            BeadState::from_linear_type("canceled", "Canceled"),
            BeadState::Done
        );
        assert_eq!(
            BeadState::from_linear_type("started", "In Progress"),
            BeadState::Dispatched
        );
        assert_eq!(
            BeadState::from_linear_type("started", "In Review"),
            BeadState::Verifying
        );
        // Custom name with "review" in it still maps to Verifying
        assert_eq!(
            BeadState::from_linear_type("started", "Code Review"),
            BeadState::Verifying
        );
        assert_eq!(
            BeadState::from_linear_type("unstarted", "Todo"),
            BeadState::Open
        );
        assert_eq!(
            BeadState::from_linear_type("backlog", "Icebox"),
            BeadState::Backlog
        );
        assert_eq!(
            BeadState::from_linear_type("backlog", "Backlog"),
            BeadState::Backlog
        );
    }

    #[test]
    fn generation_changes_with_content() {
        let bead1 = Bead::from_bd_json(
            &json!({
                "id": "x-1", "title": "fix bug", "description": "desc",
                "status": "open", "priority": 1,
                "created_at": "2026-03-12T00:00:00Z",
                "updated_at": "2026-03-12T00:00:00Z"
            }),
            "repo",
        )
        .unwrap();

        let bead2 = Bead::from_bd_json(
            &json!({
                "id": "x-1", "title": "fix bug UPDATED", "description": "desc",
                "status": "open", "priority": 1,
                "created_at": "2026-03-12T00:00:00Z",
                "updated_at": "2026-03-12T00:00:00Z"
            }),
            "repo",
        )
        .unwrap();

        // Same content → same generation
        assert_eq!(bead1.generation(), bead1.generation());
        // Different title → different generation
        assert_ne!(bead1.generation(), bead2.generation());
    }

    #[test]
    fn generation_ignores_status_and_timestamps() {
        let bead1 = Bead::from_bd_json(
            &json!({
                "id": "x-1", "title": "t", "description": "d",
                "status": "open", "priority": 1,
                "created_at": "2026-03-12T00:00:00Z",
                "updated_at": "2026-03-12T00:00:00Z"
            }),
            "repo",
        )
        .unwrap();

        let bead2 = Bead::from_bd_json(
            &json!({
                "id": "x-1", "title": "t", "description": "d",
                "status": "in_progress", "priority": 1,
                "created_at": "2026-03-11T00:00:00Z",
                "updated_at": "2026-03-13T00:00:00Z"
            }),
            "repo",
        )
        .unwrap();

        assert_eq!(bead1.generation(), bead2.generation());
    }

    #[test]
    fn bead_state_accessor() {
        let bead = Bead::from_bd_json(
            &json!({
                "id": "x-1", "title": "t",
                "status": "in_progress", "priority": 1,
                "created_at": "2026-03-12T00:00:00Z",
                "updated_at": "2026-03-12T00:00:00Z"
            }),
            "repo",
        )
        .unwrap();
        assert_eq!(bead.state(), BeadState::Dispatched);
    }

    #[test]
    fn blocked_bead_not_ready() {
        let val = json!({
            "id": "mache-abc",
            "title": "blocked task",
            "status": "open",
            "priority": 1,
            "dependency_count": 2,
            "dependent_count": 0,
            "comment_count": 0,
            "created_at": "2026-03-12T00:00:00Z",
            "updated_at": "2026-03-12T00:00:00Z"
        });

        let bead = Bead::from_bd_json(&val, "mache").unwrap();
        assert!(!bead.is_ready());
    }

    #[test]
    fn bead_pr_fields_default_none() {
        let val = json!({
            "id": "x-1",
            "title": "some task",
            "status": "open",
            "priority": 1,
            "created_at": "2026-03-12T00:00:00Z",
            "updated_at": "2026-03-12T00:00:00Z"
        });

        let bead = Bead::from_bd_json(&val, "repo").unwrap();
        assert!(bead.branch.is_none());
        assert!(bead.pr_url.is_none());
        assert!(bead.jj_change_id.is_none());
    }

    #[test]
    fn requires_files_for_all_types() {
        // All types require scopes (files or directories) for overlap detection
        assert!(requires_files("bug"));
        assert!(requires_files("task"));
        assert!(requires_files("feature"));
        assert!(requires_files("chore"));
        assert!(requires_files("epic"));
        assert!(requires_files("design"));
        assert!(requires_files("research"));
        assert!(requires_files("review"));
    }

    #[test]
    fn bead_pr_fields_display() {
        let val = json!({
            "id": "x-2",
            "title": "with PR",
            "status": "open",
            "priority": 1,
            "pr_url": "https://github.com/org/repo/pull/42",
            "created_at": "2026-03-12T00:00:00Z",
            "updated_at": "2026-03-12T00:00:00Z"
        });

        let bead = Bead::from_bd_json(&val, "repo").unwrap();
        let display = format!("{bead}");
        assert!(
            display.contains("https://github.com/org/repo/pull/42"),
            "display should include pr_url: {display}"
        );
    }
}
