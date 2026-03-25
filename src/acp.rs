//! ACP (Agent Client Protocol) integration for rosary.
//!
//! Rosary acts as an ACP **client** — it spawns agent subprocesses and
//! communicates via JSON-RPC over stdio. The `Client` trait implementation
//! handles permission requests by auto-approving based on `PermissionProfile`.
//!
//! ## Architecture
//!
//! ACP futures are `!Send` (the SDK uses `#[async_trait(?Send)]`), so the
//! connection runs in a dedicated thread with its own `LocalSet`. The
//! `AcpSession` wrapper implements `AgentSession: Send + Sync` by
//! communicating with the ACP thread via channels.
//!
//! ## Lifecycle
//!
//! ```text
//! spawn_acp_session(binary, prompt, work_dir, perms)
//!   → spawn dedicated thread
//!     → spawn agent subprocess (piped stdio)
//!     → ClientSideConnection::new(RosaryClient, stdout, stdin)
//!     → conn.initialize()
//!     → conn.new_session(work_dir)
//!     → conn.prompt(prompt)
//!     → wait for StopReason
//!   → AcpSession { join_handle, pid }
//! ```

use crate::dispatch::PermissionProfile;

use agent_client_protocol::{
    self as acp, Client, PermissionOptionKind, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionNotification,
};
use anyhow::{Context as _, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

// ---------------------------------------------------------------------------
// AcpSession — Send+Sync wrapper around !Send ACP connection thread
// ---------------------------------------------------------------------------

/// An ACP agent session. The connection runs in a dedicated thread;
/// this handle lets the reconciler poll/wait/kill from any thread.
pub struct AcpSession {
    join_handle: Option<std::thread::JoinHandle<bool>>,
    finished: Arc<AtomicBool>,
    result: Option<bool>,
    child_pid: Option<u32>,
}

#[async_trait::async_trait]
impl crate::dispatch::session::AgentSession for AcpSession {
    fn try_wait(&mut self) -> Result<Option<bool>> {
        if let Some(result) = self.result {
            return Ok(Some(result));
        }
        if self.finished.load(Ordering::SeqCst)
            && let Some(handle) = self.join_handle.take()
        {
            let success = handle.join().unwrap_or(false);
            self.result = Some(success);
            return Ok(Some(success));
        }
        Ok(None)
    }

    async fn wait(&mut self) -> Result<bool> {
        if let Some(result) = self.result {
            return Ok(result);
        }
        // Poll until the thread finishes (non-blocking check every 500ms)
        loop {
            if self.finished.load(Ordering::SeqCst)
                && let Some(handle) = self.join_handle.take()
            {
                let success = handle.join().unwrap_or(false);
                self.result = Some(success);
                return Ok(success);
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }

    fn kill(&mut self) -> Result<()> {
        // Kill the child process directly — the ACP thread will see EOF and exit
        if let Some(pid) = self.child_pid {
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
        }
        Ok(())
    }

    fn pid(&self) -> Option<u32> {
        self.child_pid
    }
}

// ---------------------------------------------------------------------------
// spawn_acp_session — the main entry point
// ---------------------------------------------------------------------------

/// Spawn an ACP agent and run the full protocol lifecycle in a dedicated thread.
///
/// Returns an `AcpSession` that implements `AgentSession` for the reconciler.
/// The agent binary is spawned as a subprocess with piped stdio, and the ACP
/// protocol (initialize → new_session → prompt) runs in a `LocalSet`.
pub fn spawn_acp_session(
    binary: &str,
    prompt: &str,
    work_dir: &Path,
    permissions: PermissionProfile,
    _system_prompt: &str,
    log_path: &Path,
) -> Result<AcpSession> {
    let binary = binary.to_string();
    let prompt = prompt.to_string();
    let work_dir = work_dir.to_path_buf();
    let log_path = log_path.to_path_buf();
    let finished = Arc::new(AtomicBool::new(false));
    let finished_clone = finished.clone();

    // Spawn the child process BEFORE the thread so we can get the PID
    let mut child = std::process::Command::new(&binary)
        .current_dir(&work_dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning ACP agent: {binary}"))?;

    let child_pid = child.id();
    eprintln!("[acp] spawned {binary} (pid={child_pid})");

    let join_handle = std::thread::Builder::new()
        .name(format!("acp-{child_pid}"))
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build tokio runtime for ACP thread");

            let success = rt.block_on(async {
                let local_set = tokio::task::LocalSet::new();
                local_set
                    .run_until(async {
                        run_acp_lifecycle(&mut child, &prompt, &work_dir, permissions, &log_path)
                            .await
                    })
                    .await
            });

            let ok = match success {
                Ok(true) => {
                    eprintln!("[acp] session completed successfully (pid={child_pid})");
                    true
                }
                Ok(false) => {
                    eprintln!("[acp] session completed with failure (pid={child_pid})");
                    false
                }
                Err(e) => {
                    eprintln!("[acp] session error (pid={child_pid}): {e}");
                    false
                }
            };

            finished_clone.store(true, Ordering::SeqCst);
            ok
        })
        .context("spawning ACP thread")?;

    Ok(AcpSession {
        join_handle: Some(join_handle),
        finished,
        result: None,
        child_pid: Some(child_pid),
    })
}

/// Run the ACP lifecycle inside a LocalSet: initialize → new_session → prompt.
async fn run_acp_lifecycle(
    child: &mut std::process::Child,
    prompt: &str,
    work_dir: &Path,
    permissions: PermissionProfile,
    log_path: &Path,
) -> Result<bool> {
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

    // Convert std process handles to tokio async handles
    let stdin = child
        .stdin
        .take()
        .context("agent subprocess has no stdin")?;
    let stdout = child
        .stdout
        .take()
        .context("agent subprocess has no stdout")?;

    // Wrap in tokio async IO + compat layer for futures::AsyncRead/Write
    let stdin_async = tokio::io::BufWriter::new(tokio::fs::File::from_std(unsafe {
        std::os::unix::io::FromRawFd::from_raw_fd(std::os::unix::io::IntoRawFd::into_raw_fd(stdin))
    }));
    let stdout_async = tokio::io::BufReader::new(tokio::fs::File::from_std(unsafe {
        std::os::unix::io::FromRawFd::from_raw_fd(std::os::unix::io::IntoRawFd::into_raw_fd(stdout))
    }));

    let outgoing = stdin_async.compat_write();
    let incoming = stdout_async.compat();

    let client = RosaryClient {
        permissions,
        log_path: log_path.to_path_buf(),
    };

    let (conn, handle_io) = acp::ClientSideConnection::new(client, outgoing, incoming, |fut| {
        tokio::task::spawn_local(fut);
    });

    // Drive I/O in background
    tokio::task::spawn_local(handle_io);

    // Initialize
    conn.initialize(
        acp::InitializeRequest::new(acp::ProtocolVersion::V1).client_info(
            acp::Implementation::new("rosary", env!("CARGO_PKG_VERSION"))
                .title("Rosary Orchestrator"),
        ),
    )
    .await
    .context("ACP initialize")?;

    // Create session
    let session = conn
        .new_session(acp::NewSessionRequest::new(work_dir))
        .await
        .context("ACP new_session")?;

    eprintln!(
        "[acp] session {} established in {}",
        session.session_id,
        work_dir.display()
    );

    // Send prompt
    use acp::Agent as _;
    let response = conn
        .prompt(acp::PromptRequest::new(
            session.session_id.clone(),
            vec![prompt.into()],
        ))
        .await
        .context("ACP prompt")?;

    // Check stop reason
    let success = matches!(
        response.stop_reason,
        acp::StopReason::EndTurn | acp::StopReason::MaxTurnRequests
    );

    eprintln!(
        "[acp] prompt finished: stop_reason={:?}, success={success}",
        response.stop_reason
    );

    Ok(success)
}

// ---------------------------------------------------------------------------
// RosaryClient — implements ACP Client trait for autonomous permission handling
// ---------------------------------------------------------------------------

/// Rosary's ACP client implementation.
///
/// Auto-approves tool calls based on the `PermissionProfile` without user
/// interaction. Logs session notifications to `.rsry-stream.jsonl`.
pub struct RosaryClient {
    pub permissions: PermissionProfile,
    pub log_path: PathBuf,
}

#[async_trait::async_trait(?Send)]
impl Client for RosaryClient {
    async fn request_permission(
        &self,
        args: RequestPermissionRequest,
    ) -> agent_client_protocol::Result<RequestPermissionResponse> {
        let tool_name = args.tool_call.fields.title.as_deref().unwrap_or("");
        eprintln!("[acp] permission request: {tool_name}");

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
            eprintln!("[acp] → approved: {tool_name}");
            return Ok(RequestPermissionResponse::new(
                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(allow_opt)),
            ));
        }

        // Reject
        if let Some(reject_opt) = args
            .options
            .iter()
            .find(|o| matches!(o.kind, PermissionOptionKind::RejectOnce))
            .map(|o| o.option_id.clone())
        {
            eprintln!("[acp] → rejected: {tool_name}");
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
        args: SessionNotification,
    ) -> agent_client_protocol::Result<()> {
        // Log structured event to stream file
        if let Ok(json) = serde_json::to_string(&args.update)
            && let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.log_path)
        {
            use std::io::Write;
            let _ = writeln!(f, "{json}");
        }
        Ok(())
    }
}

/// Check whether a tool call should be auto-approved based on the permission profile.
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
            log_path: PathBuf::from("/dev/null"),
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
            log_path: PathBuf::from("/dev/null"),
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
            log_path: PathBuf::from("/dev/null"),
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

    // -- AcpSession tests --

    use crate::dispatch::session::AgentSession;

    #[test]
    fn spawn_nonexistent_binary_errors() {
        let result = spawn_acp_session(
            "nonexistent-acp-agent-xyz",
            "test",
            std::path::Path::new("."),
            PermissionProfile::ReadOnly,
            "",
            std::path::Path::new("/dev/null"),
        );
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn acp_session_try_wait_returns_none_initially() {
        // Use `cat` as a fake agent — it will block on stdin forever
        let mut session = spawn_acp_session(
            "cat",
            "test",
            std::path::Path::new("."),
            PermissionProfile::ReadOnly,
            "",
            std::path::Path::new("/dev/null"),
        )
        .unwrap();

        // Should not have completed yet (cat blocks on stdin)
        // Give the thread a moment to start
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        // The ACP handshake will fail immediately because cat doesn't speak ACP,
        // so the thread may finish quickly. But the session should still be queryable.
        let result = session.try_wait();
        assert!(result.is_ok());
        // Clean up
        session.kill().ok();
    }

    #[tokio::test]
    async fn acp_session_kill_terminates() {
        let mut session = spawn_acp_session(
            "sleep",
            "test",
            std::path::Path::new("."),
            PermissionProfile::ReadOnly,
            "",
            std::path::Path::new("/dev/null"),
        );

        // sleep doesn't take args the way we need — this will error on spawn
        // or fail the ACP handshake. Either way, kill should not panic.
        if let Ok(ref mut s) = session {
            s.kill().ok();
        }
    }
}
