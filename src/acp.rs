//! ACP (Agent Client Protocol) integration for rosary.
//!
//! Rosary acts as an ACP **client** — it spawns agent subprocesses and
//! communicates via JSON-RPC over stdio. The `Client` trait implementation
//! handles permission requests by auto-approving based on `PermissionProfile`.
//!
//! This module replaces the CLI shell-out pattern in dispatch.rs with
//! protocol-native agent communication.

use crate::dispatch::PermissionProfile;

#[allow(dead_code)] // Wired when dispatch.rs migrates from CLI to ACP
/// An ACP-connected agent handle. Wraps the subprocess + connection.
pub struct AcpHandle {
    /// The agent subprocess.
    pub child: tokio::process::Child,
    /// Permission profile for auto-approving tool calls.
    pub permissions: PermissionProfile,
    /// Working directory the agent operates in.
    pub work_dir: std::path::PathBuf,
}

#[allow(dead_code)] // Wired when dispatch.rs migrates from CLI to ACP
/// Spawn an agent subprocess and establish an ACP connection.
///
/// The agent binary (e.g. `claude-agent-acp`) is started as a subprocess
/// with stdio piped. Rosary connects as the ACP client.
pub async fn spawn_acp_agent(
    agent_binary: &str,
    work_dir: &std::path::Path,
    permissions: PermissionProfile,
) -> anyhow::Result<AcpHandle> {
    let child = tokio::process::Command::new(agent_binary)
        .current_dir(work_dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning ACP agent: {agent_binary}"))?;

    Ok(AcpHandle {
        child,
        permissions,
        work_dir: work_dir.to_path_buf(),
    })
}

use agent_client_protocol::{
    Client, PermissionOptionKind, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionNotification,
};
use anyhow::Context as _;

// ---------------------------------------------------------------------------
// RosaryClient — implements ACP Client trait for autonomous permission handling
// ---------------------------------------------------------------------------

/// Rosary's ACP client implementation.
///
/// Auto-approves tool calls based on the `PermissionProfile` without user
/// interaction. This is what makes dispatched agents autonomous — rosary
/// decides what tools they can use based on the bead's issue type.
#[allow(dead_code)]
pub struct RosaryClient {
    pub permissions: PermissionProfile,
}

#[async_trait::async_trait(?Send)]
impl Client for RosaryClient {
    async fn request_permission(
        &self,
        args: RequestPermissionRequest,
    ) -> agent_client_protocol::Result<RequestPermissionResponse> {
        // Tool name is in ToolCallUpdate.fields.title
        let tool_name = args.tool_call.fields.title.as_deref().unwrap_or("");

        if should_approve(tool_name, &self.permissions)
            && let Some(allow_opt) = args
                .options
                .iter()
                .find(|o| {
                    matches!(
                        o.kind,
                        PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways
                    )
                })
                .map(|o| o.option_id.clone())
        {
            return Ok(RequestPermissionResponse::new(
                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(allow_opt)),
            ));
        }

        // Reject: find a reject option
        if let Some(reject_opt) = args
            .options
            .iter()
            .find(|o| matches!(o.kind, PermissionOptionKind::RejectOnce))
            .map(|o| o.option_id.clone())
        {
            return Ok(RequestPermissionResponse::new(
                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(reject_opt)),
            ));
        }

        Ok(RequestPermissionResponse::new(
            RequestPermissionOutcome::Cancelled,
        ))
    }

    async fn session_notification(
        &self,
        _args: SessionNotification,
    ) -> agent_client_protocol::Result<()> {
        Ok(())
    }
}

#[allow(dead_code)] // Wired when Client::request_permission is implemented
/// Check whether a tool call should be auto-approved based on the permission profile.
///
/// This is the core permission logic — called from the `Client::request_permission`
/// implementation to decide without user interaction.
pub fn should_approve(tool_name: &str, permissions: &PermissionProfile) -> bool {
    let is_mcp = tool_name.starts_with("mcp__mache__") || tool_name.starts_with("mcp__rsry__");
    let is_read = matches!(tool_name, "Read" | "Glob" | "Grep");

    match permissions {
        PermissionProfile::ReadOnly => is_read || is_mcp,
        PermissionProfile::Implement => {
            is_read
                || is_mcp
                || matches!(tool_name, "Edit" | "Write")
                || tool_name.starts_with("Bash(")
        }
        PermissionProfile::Plan => is_read || is_mcp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::PermissionProfile;

    // -- should_approve tests --

    #[test]
    fn read_only_approves_read_tools() {
        let p = PermissionProfile::ReadOnly;
        assert!(should_approve("Read", &p));
        assert!(should_approve("Glob", &p));
        assert!(should_approve("Grep", &p));
    }

    #[test]
    fn read_only_blocks_write_tools() {
        let p = PermissionProfile::ReadOnly;
        assert!(!should_approve("Edit", &p));
        assert!(!should_approve("Write", &p));
        assert!(!should_approve("Bash(cargo test)", &p));
    }

    #[test]
    fn read_only_approves_mcp_tools() {
        let p = PermissionProfile::ReadOnly;
        assert!(should_approve("mcp__mache__get_overview", &p));
        assert!(should_approve("mcp__rsry__bead_list", &p));
    }

    #[test]
    fn implement_approves_edit_tools() {
        let p = PermissionProfile::Implement;
        assert!(should_approve("Read", &p));
        assert!(should_approve("Edit", &p));
        assert!(should_approve("Write", &p));
        assert!(should_approve("Glob", &p));
        assert!(should_approve("Grep", &p));
    }

    #[test]
    fn implement_approves_bash_commands() {
        let p = PermissionProfile::Implement;
        assert!(should_approve("Bash(cargo test)", &p));
        assert!(should_approve("Bash(git commit -m 'fix')", &p));
    }

    #[test]
    fn implement_approves_mcp_tools() {
        let p = PermissionProfile::Implement;
        assert!(should_approve("mcp__mache__search", &p));
        assert!(should_approve("mcp__rsry__bead_create", &p));
    }

    #[test]
    fn plan_blocks_edit_tools() {
        let p = PermissionProfile::Plan;
        assert!(!should_approve("Edit", &p));
        assert!(!should_approve("Write", &p));
        assert!(!should_approve("Bash(cargo test)", &p));
    }

    #[test]
    fn plan_approves_read_and_mcp() {
        let p = PermissionProfile::Plan;
        assert!(should_approve("Read", &p));
        assert!(should_approve("mcp__rsry__bead_create", &p));
        assert!(should_approve("mcp__mache__find_definition", &p));
    }

    #[test]
    fn implement_blocks_unknown_tools() {
        let p = PermissionProfile::Implement;
        assert!(!should_approve("DeleteDatabase", &p));
        assert!(!should_approve("SendEmail", &p));
    }

    // -- RosaryClient tests --

    use agent_client_protocol::{
        PermissionOption, PermissionOptionKind, ToolCallUpdate, ToolCallUpdateFields,
    };

    fn make_permission_request(tool_name: &str) -> (RequestPermissionRequest, String, String) {
        let allow_id = "allow-once";
        let reject_id = "reject-once";
        let fields = ToolCallUpdateFields::new().title(tool_name);
        let tool_call = ToolCallUpdate::new("call-1", fields);
        let req = RequestPermissionRequest::new(
            "test-session",
            tool_call,
            vec![
                PermissionOption::new(allow_id, "Allow", PermissionOptionKind::AllowOnce),
                PermissionOption::new(reject_id, "Reject", PermissionOptionKind::RejectOnce),
            ],
        );
        (req, allow_id.to_string(), reject_id.to_string())
    }

    #[tokio::test]
    async fn rosary_client_approves_allowed_tool() {
        let client = RosaryClient {
            permissions: PermissionProfile::Implement,
        };
        let (req, allow_id, _) = make_permission_request("Edit");
        let resp = client.request_permission(req).await.unwrap();
        match resp.outcome {
            RequestPermissionOutcome::Selected(sel) => {
                assert_eq!(sel.option_id.to_string(), allow_id);
            }
            other => panic!("expected Selected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rosary_client_rejects_disallowed_tool() {
        let client = RosaryClient {
            permissions: PermissionProfile::ReadOnly,
        };
        let (req, _, reject_id) = make_permission_request("Edit");
        let resp = client.request_permission(req).await.unwrap();
        match resp.outcome {
            RequestPermissionOutcome::Selected(sel) => {
                assert_eq!(sel.option_id.to_string(), reject_id);
            }
            other => panic!("expected Selected(reject), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rosary_client_approves_mcp_tool() {
        let client = RosaryClient {
            permissions: PermissionProfile::Plan,
        };
        let (req, allow_id, _) = make_permission_request("mcp__rsry__bead_create");
        let resp = client.request_permission(req).await.unwrap();
        match resp.outcome {
            RequestPermissionOutcome::Selected(sel) => {
                assert_eq!(sel.option_id.to_string(), allow_id);
            }
            other => panic!("expected Selected(allow), got {other:?}"),
        }
    }

    // -- spawn_acp_agent tests (require real binary, so gated) --

    #[tokio::test]
    async fn spawn_nonexistent_binary_errors() {
        let result = spawn_acp_agent(
            "nonexistent-acp-agent-xyz",
            std::path::Path::new("."),
            PermissionProfile::ReadOnly,
        )
        .await;
        assert!(result.is_err());
    }
}
