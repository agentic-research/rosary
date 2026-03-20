//! MCP tool definitions — JSON schema for every `rsry_*` tool.

use serde_json::{Value, json};

pub(crate) fn tool_definitions() -> Value {
    json!({
        "tools": [
            {
                "name": "rsry_scan",
                "description": "Scan all configured repos for beads (work items). Returns a JSON array of beads with their status, priority, and metadata.",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            },
            {
                "name": "rsry_status",
                "description": "Return aggregated status counts across all repos: open, ready, in_progress, and blocked bead counts.",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            },
            {
                "name": "rsry_list_beads",
                "description": "List all beads with optional status filter. Returns a JSON array of matching beads.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "status": {
                            "type": "string",
                            "description": "Filter by status (open, in_progress, blocked, done, etc.). If omitted, returns all beads."
                        }
                    },
                    "required": []
                }
            },
            {
                "name": "rsry_run_once",
                "description": "Run a single reconciliation pass (scan, triage, dispatch, verify). Use dry_run=true to preview without spawning agents.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "dry_run": {
                            "type": "boolean",
                            "description": "If true, print what would be dispatched without actually spawning agents. Defaults to true.",
                            "default": true
                        }
                    },
                    "required": []
                }
            },
            {
                "name": "rsry_bead_create",
                "description": "Create a new bead (work item) in a repo's Dolt database. Use when you've identified a discrete, actionable issue. Set file scopes accurately — they determine parallel dispatch safety via has_file_overlap().",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "repo_path": { "type": "string", "description": "Path to repo with .beads/ directory" },
                        "title": { "type": "string", "description": "Bead title" },
                        "description": { "type": "string", "description": "Bead description", "default": "" },
                        "priority": { "type": "integer", "description": "Priority 0-3 (0=P0 highest)", "default": 2 },
                        "issue_type": { "type": "string", "description": "Issue type (bug, task, feature, review, epic)", "default": "task" },
                        "owner": { "type": "string", "description": "Agent owner (dev-agent, staging-agent, etc.). Auto-assigned from issue_type if omitted." },
                        "files": { "type": "array", "items": { "type": "string" }, "description": "Source files this bead touches. CRITICAL: these scope parallel dispatch — has_file_overlap() (epic.rs:386-393) blocks concurrent beads sharing files, and reconcile.rs:372-380 enforces it at dispatch time. Set scopes ONLY after reading the code; guessed scopes cause false-negative overlap and agent collisions. Include both files being modified AND files needing wiring changes (imports, call sites). New files are safe — no overlap possible." },
                        "test_files": { "type": "array", "items": { "type": "string" }, "description": "Test files to validate the change. Also checked for overlap — two beads sharing a test file will be serialized, not parallelized." },
                        "depends_on": { "type": "array", "items": { "type": "string" }, "description": "Bead IDs this bead depends on (blocked until they complete). Creates entries in the dependencies table." },
                        "owner_type": { "type": "string", "enum": ["agent", "human"], "description": "Whether this bead is agent-dispatchable ('agent', default) or requires human action ('human'). Human beads are skipped during autonomous triage.", "default": "agent" }
                    },
                    "required": ["repo_path", "title"]
                }
            },
            {
                "name": "rsry_bead_update",
                "description": "Update a bead's fields (PATCH semantics). Only provided fields are changed; omitted fields are left unchanged.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "repo_path": { "type": "string", "description": "Path to repo with .beads/ directory" },
                        "id": { "type": "string", "description": "Bead ID to update" },
                        "title": { "type": "string", "description": "New title" },
                        "description": { "type": "string", "description": "New description" },
                        "priority": { "type": "integer", "description": "New priority 0-3" },
                        "issue_type": { "type": "string", "description": "New issue type" },
                        "owner": { "type": "string", "description": "New owner/assignee" },
                        "files": { "type": "array", "items": { "type": "string" }, "description": "Updated source files list. These scope parallel dispatch — see has_file_overlap() (epic.rs:386-393). Verify against actual code before setting; inaccurate scopes cause agent collisions or missed overlap detection." },
                        "test_files": { "type": "array", "items": { "type": "string" }, "description": "Updated test files list. Also checked for overlap at dispatch time (reconcile.rs:372-380)." },
                        "owner_type": { "type": "string", "enum": ["agent", "human"], "description": "Whether this bead is agent-dispatchable ('agent') or requires human action ('human')." }
                    },
                    "required": ["repo_path", "id"]
                }
            },
            {
                "name": "rsry_bead_close",
                "description": "Close a bead by ID, marking it as done. Use after your changes are committed and tests pass. Do not close if the fix is incomplete or tests are failing — comment explaining the state instead.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "repo_path": { "type": "string", "description": "Path to repo with .beads/ directory" },
                        "id": { "type": "string", "description": "Bead ID to close" }
                    },
                    "required": ["repo_path", "id"]
                }
            },
            {
                "name": "rsry_bead_comment",
                "description": "Add a progress comment to a bead. Use throughout your work to log what you've tried, found, and what remains. Other agents in the pipeline and human reviewers read these comments for context.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "repo_path": { "type": "string", "description": "Path to repo with .beads/ directory" },
                        "id": { "type": "string", "description": "Bead ID" },
                        "body": { "type": "string", "description": "Comment text" }
                    },
                    "required": ["repo_path", "id", "body"]
                }
            },
            {
                "name": "rsry_bead_link",
                "description": "Add or remove a dependency between beads. Use to express 'A depends on B' (A is blocked until B completes).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "repo_path": { "type": "string", "description": "Path to repo with .beads/ directory" },
                        "id": { "type": "string", "description": "Bead ID that depends on another" },
                        "depends_on": { "type": "string", "description": "Bead ID that must complete first" },
                        "remove": { "type": "boolean", "description": "If true, removes the dependency instead of adding", "default": false }
                    },
                    "required": ["repo_path", "id", "depends_on"]
                }
            },
            {
                "name": "rsry_bead_search",
                "description": "Search beads in a specific repo by title/description substring. Returns matching beads with their status and metadata. Use to check for existing beads before creating duplicates.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "repo_path": { "type": "string", "description": "Path to repo with .beads/ directory" },
                        "query": { "type": "string", "description": "Search query" }
                    },
                    "required": ["repo_path", "query"]
                }
            },
            {
                "name": "rsry_dispatch",
                "description": "Dispatch an agent to work on a specific bead. Spawns a Claude/Gemini agent in the bead's repo with the appropriate agent perspective (dev-agent, staging-agent, etc.) and permissions.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "bead_id": { "type": "string", "description": "Bead ID to dispatch" },
                        "repo_path": { "type": "string", "description": "Path to repo containing the bead" },
                        "provider": { "type": "string", "description": "Agent provider (claude, gemini, acp)", "default": "claude" },
                        "agent": { "type": "string", "description": "Agent persona override (dev-agent, staging-agent, prod-agent, feature-agent, pm-agent). If omitted, uses bead owner." },
                        "isolate": { "type": "boolean", "description": "Create an isolated workspace (git worktree / jj workspace) before dispatch. Defaults to true. Set to false only for single-concurrency in-place execution.", "default": true }
                    },
                    "required": ["bead_id", "repo_path"]
                }
            },
            {
                "name": "rsry_active",
                "description": "Show currently running agent sessions with bead ID, repo, provider, elapsed time.",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            },
            {
                "name": "rsry_workspace_create",
                "description": "Create an isolated workspace (jj or git worktree) for a bead. Returns the workspace work_dir and vcs type. The conductor should call this before dispatching an agent.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "bead_id": { "type": "string", "description": "Bead ID for the workspace" },
                        "repo_path": { "type": "string", "description": "Path to the repo root" }
                    },
                    "required": ["bead_id", "repo_path"]
                }
            },
            {
                "name": "rsry_workspace_checkpoint",
                "description": "Checkpoint a workspace: jj commit + bookmark. Returns the jj change ID. Call after agent completes, before cleanup.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "bead_id": { "type": "string", "description": "Bead ID" },
                        "repo_path": { "type": "string", "description": "Path to the repo root" },
                        "message": { "type": "string", "description": "Commit message (default: agent work)" }
                    },
                    "required": ["bead_id", "repo_path"]
                }
            },
            {
                "name": "rsry_workspace_cleanup",
                "description": "Clean up a workspace (jj workspace forget + delete directory). Call after checkpoint.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "bead_id": { "type": "string", "description": "Bead ID" },
                        "repo_path": { "type": "string", "description": "Path to the repo root" }
                    },
                    "required": ["bead_id", "repo_path"]
                }
            },
            {
                "name": "rsry_workspace_merge",
                "description": "Merge a completed agent's worktree branch back to main (ff-merge for tasks/bugs, push branch for features/epics).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "bead_id": { "type": "string", "description": "Bead ID (branch is fix/{bead_id})" },
                        "repo_path": { "type": "string", "description": "Path to the repo root" },
                        "issue_type": { "type": "string", "description": "Issue type (task/bug = ff-merge, feature/epic = push branch)", "default": "task" }
                    },
                    "required": ["bead_id", "repo_path"]
                }
            },
            {
                "name": "rsry_decompose",
                "description": "Decompose a markdown document (ADR, README, etc.) into a decade of threaded beads. Skips non-actionable sections (consequences, alternatives, references). Returns structure without creating beads — review before committing.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Absolute path to the markdown file" },
                        "title": { "type": "string", "description": "Title for the decade (defaults to first heading)" }
                    },
                    "required": ["path"]
                }
            },
            {
                "name": "rsry_pipeline_upsert",
                "description": "Write pipeline state for a bead to the backend store. Creates or updates the pipeline record tracking which agent phase a bead is in.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "repo": { "type": "string", "description": "Repository name (e.g. 'rosary')" },
                        "bead_id": { "type": "string", "description": "Bead ID (e.g. 'rsry-abc123')" },
                        "pipeline_phase": { "type": "integer", "description": "Phase index: dev=0, staging=1, prod=2, feature=3" },
                        "pipeline_agent": { "type": "string", "description": "Agent name (e.g. 'dev-agent')" },
                        "phase_status": { "type": "string", "description": "Sub-state: pending, executing, completed, failed", "default": "pending" },
                        "retries": { "type": "integer", "description": "Retry count", "default": 0 },
                        "consecutive_reverts": { "type": "integer", "description": "Consecutive revert count", "default": 0 },
                        "highest_verify_tier": { "type": "integer", "description": "Highest verification tier reached (optional)" },
                        "last_generation": { "type": "integer", "description": "Content hash generation", "default": 0 },
                        "backoff_until": { "type": "string", "description": "ISO 8601 datetime for retry eligibility (optional)" }
                    },
                    "required": ["repo", "bead_id", "pipeline_phase", "pipeline_agent"]
                }
            },
            {
                "name": "rsry_pipeline_query",
                "description": "Query pipeline state. Get a single pipeline by repo + bead_id, or list all active pipelines with no args.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "repo": { "type": "string", "description": "Repository name" },
                        "bead_id": { "type": "string", "description": "Bead ID" }
                    },
                    "required": []
                }
            },
            {
                "name": "rsry_dispatch_record",
                "description": "Record a dispatch event in the backend store. Called by the conductor/orchestrator when spawning an agent — not typically called by agents directly.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "Dispatch UUID" },
                        "repo": { "type": "string", "description": "Repo name" },
                        "bead_id": { "type": "string", "description": "Bead ID" },
                        "agent": { "type": "string", "description": "Agent name" },
                        "provider": { "type": "string", "description": "Provider (claude, gemini, acp)" },
                        "work_dir": { "type": "string", "description": "Working directory" }
                    },
                    "required": ["id", "repo", "bead_id", "agent", "provider", "work_dir"]
                }
            },
            {
                "name": "rsry_dispatch_history",
                "description": "Query dispatch history. Filter by bead_id to see all dispatches for a specific bead, or use active_only to see currently running agents. Useful for checking if an agent is already working on a bead before dispatching another.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "bead_id": { "type": "string", "description": "Filter by bead ID" },
                        "active_only": { "type": "boolean", "description": "Only active dispatches", "default": true }
                    },
                    "required": []
                }
            },
            {
                "name": "rsry_decade_list",
                "description": "List decades (ADR-level organizing primitives). Optionally filter by status (proposed, active, completed, superseded).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "status": { "type": "string", "description": "Filter by status (optional)" }
                    },
                    "required": []
                }
            },
            {
                "name": "rsry_thread_list",
                "description": "List threads within a decade, or find the thread a bead belongs to.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "decade_id": { "type": "string", "description": "Decade ID to list threads for" },
                        "bead_id": { "type": "string", "description": "Find thread for this bead (alternative to decade_id)" },
                        "repo": { "type": "string", "description": "Repo name for bead lookup (required with bead_id)" }
                    },
                    "required": []
                }
            },
            {
                "name": "rsry_thread_assign",
                "description": "Assign a bead to a thread. Creates the thread if it doesn't exist. Use to build ordered progressions of related beads.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "thread_id": { "type": "string", "description": "Thread ID (e.g. 'ADR-003/pipeline-quality')" },
                        "thread_name": { "type": "string", "description": "Thread display name (for new threads)" },
                        "decade_id": { "type": "string", "description": "Decade this thread belongs to (for new threads)" },
                        "bead_id": { "type": "string", "description": "Bead ID to assign to the thread" },
                        "repo": { "type": "string", "description": "Repo name for the bead" }
                    },
                    "required": ["thread_id", "bead_id", "repo"]
                }
            }
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_definitions_has_expected_tools() {
        let defs = tool_definitions();
        let tools = defs["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();

        // Verify all tools have rsry_ prefix (no typos or missing names)
        for name in &names {
            assert!(
                name.starts_with("rsry_"),
                "tool '{name}' missing rsry_ prefix"
            );
        }

        // Verify critical tools exist (not exhaustive — adding a tool shouldn't break this test)
        for required in [
            "rsry_scan",
            "rsry_bead_create",
            "rsry_bead_close",
            "rsry_dispatch",
            "rsry_active",
            "rsry_workspace_create",
        ] {
            assert!(
                names.contains(&required),
                "required tool '{required}' missing"
            );
        }

        // Sanity: at least 15 tools (grows over time, never shrinks)
        assert!(
            tools.len() >= 15,
            "expected at least 15 tools, got {}",
            tools.len()
        );
    }

    #[test]
    fn tool_definitions_have_input_schemas() {
        let defs = tool_definitions();
        let tools = defs["tools"].as_array().unwrap();
        for tool in tools {
            assert!(
                tool.get("inputSchema").is_some(),
                "tool {} missing inputSchema",
                tool["name"]
            );
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn bead_crud_tools_have_repo_path() {
        let defs = tool_definitions();
        let tools = defs["tools"].as_array().unwrap();
        for tool in tools {
            let name = tool["name"].as_str().unwrap();
            if name.starts_with("rsry_bead_") {
                let props = &tool["inputSchema"]["properties"];
                assert!(
                    props.get("repo_path").is_some(),
                    "{name} missing repo_path parameter"
                );
            }
        }
    }
}
