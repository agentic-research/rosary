//! Codex app-server **runtime** — the event loop that turns a *started* turn
//! into an *observed* completion.
//!
//! The wire contract (request envelopes, params) lives in `codex_native.rs`.
//! This is the runtime fable flagged as missing: `turn/start` returns only an
//! *ack*; the turn then runs asynchronously and the app-server streams progress
//! notifications and **server→client approval requests** on the same
//! connection until the turn ends. The old one-shot `request()` dropped the
//! connection after the ack, so completion was never observed and
//! `AgentSession::wait` was hardcoded to `false`. This module reads past the
//! ack: a persistent [`CodexConnection`] driven by [`run_turn_loop`], which
//! answers approvals (so the turn doesn't stall) and resolves the turn's real
//! success/failure.
//!
//! **Protocol strings are matched tolerantly on purpose.** Codex's exact event
//! method names are matched by namespace + keyword (see [`classify_turn_signal`])
//! rather than exact equality, so a single renamed method can't silently break
//! completion detection. The *machinery* here is protocol-shape-agnostic and is
//! what the tests pin; the concrete strings are a small surface to verify
//! against the live app-server protocol.
//!
//! Dormant seam: the loop is exercised by the tests below and wired into the
//! runtime in the follow-up transport slice, so the public surface is
//! `allow(dead_code)` until then — matching `codex_native.rs`.
#![allow(dead_code)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::PermissionProfile;
use super::codex_native::{
    CodexAppServerRequest, CodexNativeSession, CodexRuntime, CodexThreadStart,
};

/// A live, ordered message channel to the Codex app-server. Unlike the one-shot
/// request/response boundary, a connection is read repeatedly: after
/// `turn/start` the server streams notifications and server→client approval
/// requests until the turn ends. Production is a persistent WebSocket; tests
/// inject a scripted connection.
pub trait CodexConnection: Send {
    /// Send one JSON-RPC value (request or response) to the app-server.
    fn send(&mut self, value: &Value) -> Result<()>;
    /// Block for the next inbound message, bounded by `idle_timeout` (the max
    /// silence between events before we treat the server as hung). Returns the
    /// parsed JSON-RPC value.
    fn recv(&mut self, idle_timeout: Duration) -> Result<Value>;
}

/// What a single inbound app-server message means to the turn loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnSignal {
    /// The turn reached a terminal state. `success` is false on failure/abort.
    Completed { success: bool },
    /// The server is asking permission for an action; it carries an `id` (to
    /// answer) and the request `method` (which selects the decision vocabulary).
    ApprovalRequest { id: Value, method: String },
    /// Progress / telemetry / an unrelated message — nothing the loop must act
    /// on.
    Progress,
}

/// Classify one inbound message against Codex's app-server protocol — verified
/// against `codex app-server generate-json-schema` (v0.142.5):
///
/// - a server→client **approval request** carries an `id` and an approval-shaped
///   method (`execCommandApproval`, `applyPatchApproval`,
///   `commandExecutionRequestApproval`, `fileChangeRequestApproval`,
///   `permissionsRequestApproval` — all contain "approval");
/// - **`turn/completed`** is the *only* terminal turn notification — there is no
///   `turn/failed`; success is read from `params.turn.status`;
/// - an **`error`** notification with `willRetry:false` is a terminal failure
///   (with `willRetry:true` it is a retry — progress);
/// - everything else — `item/*`, `turn/started`, `thread/*` — is progress.
pub fn classify_turn_signal(msg: &Value) -> TurnSignal {
    let method = msg
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let has_id = msg.get("id").map(|v| !v.is_null()).unwrap_or(false);

    if has_id && method.to_ascii_lowercase().contains("approval") {
        return TurnSignal::ApprovalRequest {
            id: msg.get("id").cloned().unwrap_or(Value::Null),
            method: method.to_string(),
        };
    }

    match method {
        "turn/completed" => TurnSignal::Completed {
            success: turn_reported_success(msg),
        },
        // A non-retrying error notification ends the turn in failure.
        "error"
            if !msg
                .pointer("/params/willRetry")
                .and_then(Value::as_bool)
                .unwrap_or(false) =>
        {
            TurnSignal::Completed { success: false }
        }
        _ => TurnSignal::Progress,
    }
}

/// A `turn/completed` notification carries the final `Turn` in `params.turn`;
/// the turn failed iff its `status` is `failed`/`interrupted` or its `error` is
/// populated (`error` is only set when the status is `failed`).
fn turn_reported_success(msg: &Value) -> bool {
    let Some(turn) = msg.pointer("/params/turn") else {
        return true;
    };
    if turn.get("error").map(|e| !e.is_null()).unwrap_or(false) {
        return false;
    }
    !matches!(
        turn.get("status").and_then(Value::as_str),
        Some("failed") | Some("interrupted")
    )
}

/// Build the JSON-RPC response to a server approval request. Codex uses two
/// decision vocabularies (verified against the protocol schema): the
/// `ReviewDecision` set (`approved`/`denied`) for `execCommandApproval` and
/// `applyPatchApproval`, and the v2 set (`accept`/`decline`) for the
/// `*RequestApproval` methods. The request `method` selects which.
pub fn approval_response(id: &Value, method: &str, approve: bool) -> Value {
    let m = method.to_ascii_lowercase();
    let review_vocab = m.contains("execcommand") || m.contains("applypatch");
    let decision = match (review_vocab, approve) {
        (true, true) => "approved",
        (true, false) => "denied",
        (false, true) => "accept",
        (false, false) => "decline",
    };
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": { "decision": decision },
    })
}

/// Drive an already-started turn to completion over `conn`, answering approval
/// requests with `approve`. Returns the turn's success.
///
/// Blocks (each `recv` bounded by `idle_timeout`) until a terminal signal. A
/// `recv` error — a closed socket, a hung server past the idle bound — is
/// propagated; the caller maps it to a failed session. This is the read-past-
/// the-ack loop the one-shot client never had.
pub fn run_turn_loop(
    conn: &mut dyn CodexConnection,
    approve: bool,
    idle_timeout: Duration,
) -> Result<bool> {
    loop {
        let msg = conn.recv(idle_timeout)?;
        match classify_turn_signal(&msg) {
            TurnSignal::Completed { success } => return Ok(success),
            TurnSignal::ApprovalRequest { id, method } => {
                conn.send(&approval_response(&id, &method, approve))?;
            }
            TurnSignal::Progress => continue,
        }
    }
}

/// Deadline for one startup JSON-RPC round-trip (initialize / thread-start /
/// turn-start) — a hung server can't block startup forever (rosary-72fc26).
pub(crate) const CODEX_RPC_TIMEOUT: Duration = Duration::from_secs(30);

/// Max silence between turn events before a running turn is treated as hung.
/// A turn can be quiet for a while while the model works, so this is generous.
const CODEX_TURN_IDLE_TIMEOUT: Duration = Duration::from_secs(600);

/// Opens persistent Codex app-server connections. Production dials the local
/// control socket and performs the initialize handshake; tests inject a
/// scripted connector.
pub trait CodexConnector: Send + Sync {
    fn connect(&self) -> Result<Box<dyn CodexConnection>>;
}

/// Native Codex runtime backed by a [`CodexConnector`]. `start_thread` opens one
/// connection, starts the thread and the turn synchronously (their acks), then
/// hands the connection to an event-loop thread that runs the turn to
/// completion and resolves the session's oneshot.
pub struct CodexAppServerRuntime {
    connector: Arc<dyn CodexConnector>,
}

impl CodexAppServerRuntime {
    pub fn new(connector: Arc<dyn CodexConnector>) -> Self {
        Self { connector }
    }
}

impl CodexRuntime for CodexAppServerRuntime {
    fn start_thread(&self, start: CodexThreadStart) -> Result<CodexNativeSession> {
        let mut conn = self
            .connector
            .connect()
            .context("connecting codex app-server")?;

        conn.send(&serde_json::to_value(CodexAppServerRequest::thread_start(
            "rsry-thread-start",
            &start,
        ))?)?;
        let thread_response = recv_result(conn.as_mut(), "rsry-thread-start", CODEX_RPC_TIMEOUT)
            .context("starting codex app-server thread")?;
        let thread_id = thread_id_from_thread_start_response(&thread_response)?;

        conn.send(&serde_json::to_value(CodexAppServerRequest::turn_start(
            "rsry-turn-start",
            thread_id.clone(),
            start.prompt.clone(),
            &start.work_dir,
            start.permissions,
            start.model.clone(),
        ))?)?;
        let turn_ack = recv_result(conn.as_mut(), "rsry-turn-start", CODEX_RPC_TIMEOUT)
            .context("starting codex app-server turn")?;
        let turn_id = turn_ack
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .map(str::to_owned);

        // Within the sandbox, Implement may act; ReadOnly/Plan must not — so the
        // loop denies any approval the server requests for them.
        let approve = matches!(start.permissions, PermissionProfile::Implement);
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let result =
                run_turn_loop(conn.as_mut(), approve, CODEX_TURN_IDLE_TIMEOUT).unwrap_or(false);
            let _ = tx.send(result);
        });

        // kill() cooperatively interrupts the running turn via turn/interrupt on
        // a *fresh* connection (the loop owns the original) — the app-server
        // addresses turns by {threadId, turnId}, not by connection. Only wired
        // when the ack gave us a turn id.
        let interrupt = turn_id.map(|tid| {
            let connector = Arc::clone(&self.connector);
            let thread = thread_id.clone();
            Box::new(move || {
                super::codex_transport::send_turn_interrupt(connector.as_ref(), &thread, &tid)
            }) as Box<dyn FnOnce() -> Result<()> + Send + Sync>
        });

        Ok(CodexNativeSession::pending(thread_id, rx, interrupt))
    }
}

fn thread_id_from_thread_start_response(response: &Value) -> Result<String> {
    response
        .get("thread")
        .and_then(|thread| thread.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .context("codex thread/start response missing thread.id")
}

/// Send a request already issued, then read until the JSON-RPC reply whose `id`
/// matches, skipping unrelated notifications. Errors on a JSON-RPC error reply.
fn recv_result(conn: &mut dyn CodexConnection, id: &str, timeout: Duration) -> Result<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| {
                anyhow::anyhow!("Codex app-server request {id} timed out after {timeout:?}")
            })?;
        let value = conn.recv(remaining)?;
        if value.get("id").and_then(Value::as_str) != Some(id) {
            continue;
        }
        if let Some(error) = value.get("error") {
            anyhow::bail!("Codex app-server request {id} failed: {error}");
        }
        return value
            .get("result")
            .cloned()
            .with_context(|| format!("Codex app-server response {id} missing result"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// A scripted connection: pops queued inbound messages, captures outbound.
    struct ScriptedConnection {
        inbound: VecDeque<Value>,
        sent: Vec<Value>,
    }

    impl ScriptedConnection {
        fn new(inbound: Vec<Value>) -> Self {
            Self {
                inbound: inbound.into(),
                sent: Vec::new(),
            }
        }
    }

    impl CodexConnection for ScriptedConnection {
        fn send(&mut self, value: &Value) -> Result<()> {
            self.sent.push(value.clone());
            Ok(())
        }
        fn recv(&mut self, _idle: Duration) -> Result<Value> {
            self.inbound
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("connection closed"))
        }
    }

    fn idle() -> Duration {
        Duration::from_millis(10)
    }

    // --- classifier ---

    #[test]
    fn turn_completed_is_terminal_success() {
        let msg = json!({"jsonrpc": "2.0", "method": "turn/completed", "params": {}});
        assert_eq!(
            classify_turn_signal(&msg),
            TurnSignal::Completed { success: true }
        );
    }

    #[test]
    fn completed_with_failed_turn_status_is_failure() {
        // There is no turn/failed — failure rides in turn/completed's turn.status.
        let msg = json!({
            "jsonrpc": "2.0", "method": "turn/completed",
            "params": {"threadId": "t", "turn": {"status": "failed", "error": {"message": "boom"}}}
        });
        assert_eq!(
            classify_turn_signal(&msg),
            TurnSignal::Completed { success: false }
        );
    }

    #[test]
    fn interrupted_turn_status_is_failure() {
        let msg = json!({
            "jsonrpc": "2.0", "method": "turn/completed",
            "params": {"threadId": "t", "turn": {"status": "interrupted"}}
        });
        assert_eq!(
            classify_turn_signal(&msg),
            TurnSignal::Completed { success: false }
        );
    }

    #[test]
    fn non_retrying_error_notification_is_terminal_failure() {
        let msg = json!({
            "jsonrpc": "2.0", "method": "error",
            "params": {"threadId": "t", "turnId": "u", "willRetry": false, "error": {"message": "x"}}
        });
        assert_eq!(
            classify_turn_signal(&msg),
            TurnSignal::Completed { success: false }
        );
    }

    #[test]
    fn retrying_error_notification_is_progress() {
        let msg = json!({
            "jsonrpc": "2.0", "method": "error",
            "params": {"threadId": "t", "turnId": "u", "willRetry": true, "error": {"message": "x"}}
        });
        assert_eq!(classify_turn_signal(&msg), TurnSignal::Progress);
    }

    #[test]
    fn item_completed_is_not_terminal() {
        // An item finishing must not end the turn — namespace guard.
        let msg = json!({"jsonrpc": "2.0", "method": "item/completed", "params": {}});
        assert_eq!(classify_turn_signal(&msg), TurnSignal::Progress);
    }

    #[test]
    fn approval_request_is_detected_and_carries_method() {
        let msg = json!({
            "jsonrpc": "2.0", "id": "req-7",
            "method": "execCommandApproval",
            "params": {"command": ["ls"]}
        });
        assert_eq!(
            classify_turn_signal(&msg),
            TurnSignal::ApprovalRequest {
                id: json!("req-7"),
                method: "execCommandApproval".into()
            }
        );
    }

    #[test]
    fn approval_decision_vocab_matches_request_type() {
        // ReviewDecision methods → approved/denied; v2 *RequestApproval → accept/decline.
        let d = |m, a| approval_response(&json!("i"), m, a)["result"]["decision"].clone();
        assert_eq!(d("execCommandApproval", true), "approved");
        assert_eq!(d("applyPatchApproval", false), "denied");
        assert_eq!(d("fileChangeRequestApproval", true), "accept");
        assert_eq!(d("commandExecutionRequestApproval", false), "decline");
    }

    #[test]
    fn notification_without_id_is_not_an_approval() {
        // Same keyword, but a notification (no id) is not a request to answer.
        let msg = json!({"jsonrpc": "2.0", "method": "turn/approvalResolved"});
        assert_eq!(classify_turn_signal(&msg), TurnSignal::Progress);
    }

    // --- loop ---

    #[test]
    fn loop_returns_success_on_completed() {
        let mut conn = ScriptedConnection::new(vec![
            json!({"method": "turn/started"}),
            json!({"method": "item/updated"}),
            json!({"method": "turn/completed", "params": {}}),
        ]);
        assert!(run_turn_loop(&mut conn, true, idle()).unwrap());
    }

    #[test]
    fn loop_answers_approval_then_completes() {
        let mut conn = ScriptedConnection::new(vec![
            json!({"id": "a1", "method": "execCommandApproval", "params": {}}),
            json!({"method": "turn/completed", "params": {}}),
        ]);
        assert!(run_turn_loop(&mut conn, true, idle()).unwrap());
        // The loop must have answered the approval addressed to its id, or the
        // real server would stall.
        assert_eq!(conn.sent.len(), 1);
        assert_eq!(conn.sent[0]["id"], "a1");
        assert_eq!(conn.sent[0]["result"]["decision"], "approved");
    }

    #[test]
    fn loop_denies_when_not_approving() {
        let mut conn = ScriptedConnection::new(vec![
            json!({"id": "a1", "method": "execCommandApproval", "params": {}}),
            json!({"method": "turn/completed", "params": {}}),
        ]);
        assert!(run_turn_loop(&mut conn, false, idle()).unwrap());
        assert_eq!(conn.sent[0]["result"]["decision"], "denied");
    }

    #[test]
    fn loop_propagates_failure() {
        let mut conn = ScriptedConnection::new(vec![
            json!({"method": "turn/started"}),
            json!({"method": "turn/completed", "params": {"turn": {"status": "failed"}}}),
        ]);
        assert!(!run_turn_loop(&mut conn, true, idle()).unwrap());
    }

    #[test]
    fn loop_errors_on_closed_connection_before_terminal() {
        // A closed socket before any terminal event is an error (caller → false),
        // never a silent success.
        let mut conn = ScriptedConnection::new(vec![json!({"method": "turn/started"})]);
        assert!(run_turn_loop(&mut conn, true, idle()).is_err());
    }
}
