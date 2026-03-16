//! Agent session registry — tracks active dispatches.
//!
//! Persisted to `~/.rsry/sessions.json`. This is ephemeral state
//! (not beads, not Dolt) — rebuilt on startup by checking PIDs.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A tracked agent session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntry {
    pub bead_id: String,
    pub repo: String,
    pub provider: String,
    pub pid: Option<u32>,
    pub work_dir: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub agent: String,
    /// VCS kind used for workspace isolation ("jj", "git", or empty).
    #[serde(default)]
    pub workspace_vcs: String,
    /// Original repo path (needed for workspace cleanup when session dies).
    #[serde(default)]
    pub repo_path: String,
    /// Last activity timestamp (updated on bead_comment).
    #[serde(default)]
    pub last_activity: Option<chrono::DateTime<chrono::Utc>>,
    /// Latest bead comment body (truncated).
    #[serde(default)]
    pub last_comment: Option<String>,
}

/// File-based session registry at `~/.rsry/sessions.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SessionRegistry {
    pub sessions: Vec<SessionEntry>,
}

impl SessionRegistry {
    fn path() -> Result<PathBuf> {
        let home = dirs_next::home_dir().context("cannot determine home directory")?;
        Ok(home.join(".rsry").join("sessions.json"))
    }

    /// Load the registry, returning empty if file doesn't exist.
    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let mut registry: Self =
            serde_json::from_str(&content).with_context(|| "parsing sessions.json")?;

        // Partition into alive and dead sessions
        let (alive, dead): (Vec<_>, Vec<_>) = registry
            .sessions
            .into_iter()
            .partition(|s| s.pid.map(is_pid_alive).unwrap_or(false));

        // Clean up workspaces for dead sessions
        for session in &dead {
            cleanup_session_workspace(session);
        }

        registry.sessions = alive;
        Ok(registry)
    }

    /// Save the registry.
    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let content = serde_json::to_string_pretty(self).context("serializing sessions")?;
        std::fs::write(&path, content).with_context(|| format!("writing {}", path.display()))
    }

    /// Register a new session.
    pub fn register(&mut self, entry: SessionEntry) -> Result<()> {
        // Remove any stale entry for the same bead
        self.sessions.retain(|s| s.bead_id != entry.bead_id);
        self.sessions.push(entry);
        self.save()
    }

    /// Remove a session by bead ID.
    #[allow(dead_code)] // Used by reconciler on completion
    pub fn unregister(&mut self, bead_id: &str) -> Result<()> {
        self.sessions.retain(|s| s.bead_id != bead_id);
        self.save()
    }

    /// List active sessions (pruned of dead PIDs).
    pub fn active(&self) -> &[SessionEntry] {
        &self.sessions
    }

    /// Record activity for a session (called on bead_comment).
    pub fn touch(&mut self, bead_id: &str, comment: &str) -> Result<()> {
        if let Some(session) = self.sessions.iter_mut().find(|s| s.bead_id == bead_id) {
            session.last_activity = Some(chrono::Utc::now());
            session.last_comment = Some(comment.chars().take(200).collect());
            self.save()?;
        }
        Ok(())
    }
}

/// Check if a PID is alive via kill(pid, 0).
fn is_pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// Clean up workspace for a dead session (best-effort).
fn cleanup_session_workspace(session: &SessionEntry) {
    if session.repo_path.is_empty() || session.workspace_vcs.is_empty() {
        return;
    }
    let repo_path = std::path::Path::new(&session.repo_path);
    eprintln!(
        "[session-cleanup] {} workspace for {} (vcs={})",
        session.bead_id, session.repo, session.workspace_vcs
    );
    match session.workspace_vcs.as_str() {
        "jj" => crate::workspace::cleanup_jj_workspace(repo_path, &session.bead_id),
        "git" => crate::workspace::cleanup_git_worktree(repo_path, &session.bead_id),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_registry() {
        let reg = SessionRegistry::default();
        assert!(reg.active().is_empty());
    }

    #[test]
    fn register_and_list() {
        let mut reg = SessionRegistry::default();
        reg.sessions.push(SessionEntry {
            bead_id: "rsry-abc".into(),
            repo: "rosary".into(),
            provider: "claude".into(),
            pid: Some(std::process::id()), // current process — alive
            work_dir: "/tmp/test".into(),
            started_at: chrono::Utc::now(),
            title: String::new(),
            agent: String::new(),
            workspace_vcs: String::new(),
            repo_path: String::new(),
            last_activity: None,
            last_comment: None,
        });
        assert_eq!(reg.active().len(), 1);
        assert_eq!(reg.active()[0].bead_id, "rsry-abc");
    }

    #[test]
    fn unregister_removes() {
        let mut reg = SessionRegistry::default();
        reg.sessions.push(SessionEntry {
            bead_id: "rsry-abc".into(),
            repo: "rosary".into(),
            provider: "claude".into(),
            pid: Some(1),
            work_dir: "/tmp/test".into(),
            started_at: chrono::Utc::now(),
            title: String::new(),
            agent: String::new(),
            workspace_vcs: String::new(),
            repo_path: String::new(),
            last_activity: None,
            last_comment: None,
        });
        reg.sessions.retain(|s| s.bead_id != "rsry-abc");
        assert!(reg.active().is_empty());
    }

    #[test]
    fn is_pid_alive_self() {
        assert!(is_pid_alive(std::process::id()));
    }

    #[test]
    fn is_pid_alive_dead() {
        // PID 99999999 almost certainly doesn't exist
        assert!(!is_pid_alive(99_999_999));
    }

    #[test]
    fn serialization_roundtrip() {
        let reg = SessionRegistry {
            sessions: vec![SessionEntry {
                bead_id: "rsry-abc".into(),
                repo: "rosary".into(),
                provider: "claude".into(),
                pid: Some(42),
                work_dir: "/tmp/test".into(),
                started_at: chrono::Utc::now(),
                title: "Test bead".into(),
                agent: "dev-agent".into(),
                workspace_vcs: "jj".into(),
                repo_path: "/tmp/repo".into(),
                last_activity: None,
                last_comment: None,
            }],
        };
        let json = serde_json::to_string(&reg).unwrap();
        let parsed: SessionRegistry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.sessions.len(), 1);
        assert_eq!(parsed.sessions[0].bead_id, "rsry-abc");
    }
}
