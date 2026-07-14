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

use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};

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
    /// The server is asking permission for an action; it carries an `id` that
    /// must be answered or the turn stalls.
    ApprovalRequest { id: Value },
    /// Progress / telemetry / an unrelated message — nothing the loop must act
    /// on.
    Progress,
}

/// Classify one inbound message.
///
/// Tolerant by design (see module docs): a server→client **approval request**
/// is any message that carries an `id` *and* an approval-shaped method; a
/// **terminal** turn event is a `turn`-namespaced method whose name reads as
/// completed/failed/aborted. Everything else — including `item/*` progress and
/// `turn/started` — is [`TurnSignal::Progress`]. `item/completed` is *not*
/// terminal: the namespace guard keeps an item finishing from ending the turn.
pub fn classify_turn_signal(msg: &Value) -> TurnSignal {
    let method = msg
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let has_id = msg.get("id").map(|v| !v.is_null()).unwrap_or(false);

    // Server→client approval request: needs a response addressed to its id.
    if has_id && (method.contains("approval") || method.contains("requestpermission")) {
        return TurnSignal::ApprovalRequest {
            id: msg.get("id").cloned().unwrap_or(Value::Null),
        };
    }

    // Terminal turn event — restricted to the `turn` namespace so an item
    // completing doesn't end the turn.
    if method.starts_with("turn/") || method.starts_with("turn.") {
        if method.contains("failed") || method.contains("aborted") || method.contains("error") {
            return TurnSignal::Completed { success: false };
        }
        if method.contains("completed") || method.contains("finished") {
            return TurnSignal::Completed {
                success: turn_reported_success(msg),
            };
        }
    }

    TurnSignal::Progress
}

/// A `turn/completed`-shaped event still reports success or failure in its
/// params (a non-zero exit, an `error`, or `status: "failed"`). Absent any such
/// marker, a completed turn is a success.
fn turn_reported_success(msg: &Value) -> bool {
    let Some(params) = msg.get("params") else {
        return true;
    };
    if params.get("error").map(|e| !e.is_null()).unwrap_or(false) {
        return false;
    }
    if let Some(status) = params
        .get("status")
        .or_else(|| params.get("turn").and_then(|t| t.get("status")))
        .and_then(Value::as_str)
    {
        let s = status.to_ascii_lowercase();
        return !(s.contains("fail") || s.contains("error") || s.contains("abort"));
    }
    true
}

/// Build the JSON-RPC response to a server approval request. The `decision`
/// field shape is the surface to verify against Codex's approval schema; the
/// loop machinery does not depend on its exact value.
pub fn approval_response(id: &Value, approve: bool) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": { "decision": if approve { "approved" } else { "denied" } },
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
            TurnSignal::ApprovalRequest { id } => {
                conn.send(&approval_response(&id, approve))?;
            }
            TurnSignal::Progress => continue,
        }
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
    fn turn_failed_is_terminal_failure() {
        let msg = json!({"jsonrpc": "2.0", "method": "turn/failed", "params": {}});
        assert_eq!(
            classify_turn_signal(&msg),
            TurnSignal::Completed { success: false }
        );
    }

    #[test]
    fn completed_with_error_status_is_failure() {
        let msg = json!({
            "jsonrpc": "2.0", "method": "turn/completed",
            "params": {"status": "failed"}
        });
        assert_eq!(
            classify_turn_signal(&msg),
            TurnSignal::Completed { success: false }
        );
    }

    #[test]
    fn item_completed_is_not_terminal() {
        // An item finishing must not end the turn — namespace guard.
        let msg = json!({"jsonrpc": "2.0", "method": "item/completed", "params": {}});
        assert_eq!(classify_turn_signal(&msg), TurnSignal::Progress);
    }

    #[test]
    fn approval_request_is_detected_by_id_and_method() {
        let msg = json!({
            "jsonrpc": "2.0", "id": "req-7",
            "method": "turn/requestApproval",
            "params": {"call": "exec"}
        });
        assert_eq!(
            classify_turn_signal(&msg),
            TurnSignal::ApprovalRequest { id: json!("req-7") }
        );
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
            json!({"id": "a1", "method": "turn/requestApproval", "params": {}}),
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
            json!({"id": "a1", "method": "turn/requestApproval", "params": {}}),
            json!({"method": "turn/completed", "params": {}}),
        ]);
        assert!(run_turn_loop(&mut conn, false, idle()).unwrap());
        assert_eq!(conn.sent[0]["result"]["decision"], "denied");
    }

    #[test]
    fn loop_propagates_failure() {
        let mut conn = ScriptedConnection::new(vec![
            json!({"method": "turn/started"}),
            json!({"method": "turn/failed", "params": {}}),
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
