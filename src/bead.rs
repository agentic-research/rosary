use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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
}

impl Bead {
    /// Parse from `bd list --json` output
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
            priority: value
                .get("priority")
                .and_then(|v| v.as_u64())
                .unwrap_or(2) as u8,
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
        })
    }

    pub fn is_ready(&self) -> bool {
        self.status == "open" && self.dependency_count == 0
    }
}

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
}
