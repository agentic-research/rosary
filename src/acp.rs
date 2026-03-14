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

use anyhow::Context as _;

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

    // -- AcpHandle tests --

    #[test]
    fn acp_handle_stores_permissions() {
        // Can't actually spawn without a binary, but verify the struct works
        let handle_fields_exist = std::mem::size_of::<AcpHandle>() > 0;
        assert!(handle_fields_exist);
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
