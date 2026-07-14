//! Codex app-server **transport** — the concrete persistent WebSocket
//! connection over the local Unix control socket, and the connector that dials
//! and initializes it.
//!
//! The runtime ([`super::codex_runtime`]) is transport-agnostic: it drives a
//! [`CodexConnection`] via a [`CodexConnector`]. This file is the production
//! wiring of those two traits onto a real socket. Dormant seam until config
//! selects the native provider — `allow(dead_code)` matching the rest.
#![allow(dead_code)]

use std::io::ErrorKind;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{Value, json};
use tungstenite::{Message, WebSocket};

use super::codex_runtime::{CODEX_RPC_TIMEOUT, CodexConnection, CodexConnector};

/// Persistent WebSocket connection to the Codex app-server over its Unix control
/// socket. Initialized once at connect; then `send`/`recv` drive the whole
/// thread → turn → event lifecycle on the SAME connection — the one-shot client
/// reconnected per RPC and so never saw the turn's event stream.
pub struct CodexWebSocketConnection {
    ws: WebSocket<UnixStream>,
}

impl CodexConnection for CodexWebSocketConnection {
    fn send(&mut self, value: &Value) -> Result<()> {
        send_jsonrpc_value(&mut self.ws, value)
    }
    fn recv(&mut self, idle_timeout: Duration) -> Result<Value> {
        read_next_jsonrpc_message(&mut self.ws, idle_timeout)
    }
}

/// Connector that dials the Codex app-server's local Unix control socket.
pub struct CodexUnixSocketConnector {
    socket_path: PathBuf,
}

impl Default for CodexUnixSocketConnector {
    fn default() -> Self {
        Self {
            socket_path: default_codex_app_server_socket_path(),
        }
    }
}

impl CodexUnixSocketConnector {
    #[cfg(test)]
    pub(crate) fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }
}

impl CodexConnector for CodexUnixSocketConnector {
    fn connect(&self) -> Result<Box<dyn CodexConnection>> {
        let stream = UnixStream::connect(&self.socket_path).with_context(|| {
            format!(
                "connecting Codex app-server socket {}",
                self.socket_path.display()
            )
        })?;
        let (mut ws, _response) = tungstenite::client::client("ws://localhost/", stream)
            .with_context(|| {
                format!(
                    "upgrading Codex app-server socket {}",
                    self.socket_path.display()
                )
            })?;
        initialize_codex_app_server(&mut ws)?;
        Ok(Box::new(CodexWebSocketConnection { ws }))
    }
}

fn send_jsonrpc_value(ws: &mut WebSocket<UnixStream>, value: &Value) -> Result<()> {
    ws.send(Message::Text(serde_json::to_string(value)?.into()))
        .context("sending Codex app-server JSON-RPC message")
}

/// Read the next JSON-RPC value from the socket, bounded by `timeout` (the max
/// wait for one message). Skips ping/pong/keepalive frames. The deadline is
/// keyed off the clock, not the errno, so a hung peer bails deterministically
/// however the OS wraps the timeout (rosary-72fc26).
pub(crate) fn read_next_jsonrpc_message(
    ws: &mut WebSocket<UnixStream>,
    timeout: Duration,
) -> Result<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| anyhow::anyhow!("Codex app-server read timed out after {timeout:?}"))?;
        ws.get_ref()
            .set_read_timeout(Some(remaining))
            .context("setting Codex app-server read timeout")?;
        let message = match ws.read() {
            Ok(m) => m,
            Err(_) if Instant::now() >= deadline => {
                anyhow::bail!("Codex app-server read timed out after {timeout:?}")
            }
            Err(tungstenite::Error::Io(io))
                if matches!(
                    io.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(e) => return Err(e).context("reading Codex app-server JSON-RPC message"),
        };
        let payload = match message {
            Message::Text(text) => text.to_string(),
            Message::Binary(bytes) => String::from_utf8(bytes.to_vec())
                .context("Codex app-server returned non-UTF8 binary JSON-RPC payload")?,
            Message::Close(_) => anyhow::bail!("Codex app-server closed the connection"),
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => continue,
        };
        return serde_json::from_str(&payload).context("parsing Codex app-server JSON-RPC payload");
    }
}

fn initialize_codex_app_server(ws: &mut WebSocket<UnixStream>) -> Result<()> {
    send_jsonrpc_value(
        ws,
        &json!({
            "jsonrpc": "2.0",
            "id": "rsry-initialize",
            "method": "initialize",
            "params": {
                "clientInfo": {
                    "name": "rosary",
                    "title": "Rosary",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "capabilities": {
                    "experimentalApi": true,
                    "requestAttestation": false,
                    "mcpServerOpenaiFormElicitation": false,
                }
            }
        }),
    )?;
    // Drain to the initialize reply, then confirm.
    loop {
        let value = read_next_jsonrpc_message(ws, CODEX_RPC_TIMEOUT)?;
        if value.get("id").and_then(Value::as_str) == Some("rsry-initialize") {
            break;
        }
    }
    send_jsonrpc_value(ws, &json!({"jsonrpc": "2.0", "method": "initialized"}))
}

fn default_codex_app_server_socket_path() -> PathBuf {
    if let Ok(path) = std::env::var("RSRY_CODEX_APP_SERVER_SOCKET") {
        return PathBuf::from(path);
    }
    if let Ok(path) = std::env::var("CODEX_APP_SERVER_SOCKET") {
        return PathBuf::from(path);
    }
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(dirs_next::home_dir)
        .map(|home| {
            if home.ends_with(".codex") {
                home
            } else {
                home.join(".codex")
            }
        })
        .unwrap_or_else(|| PathBuf::from(".codex"));
    codex_home
        .join("app-server-control")
        .join("app-server-control.sock")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::PermissionProfile;
    use crate::dispatch::codex_native::{CodexRuntime, CodexThreadStart};
    use crate::dispatch::codex_runtime::CodexAppServerRuntime;
    use crate::dispatch::session::{AgentSession, AgentSessionRef};
    use std::sync::Arc;

    fn read_ws(ws: &mut WebSocket<UnixStream>) -> Value {
        loop {
            match ws.read().unwrap() {
                Message::Text(t) => return serde_json::from_str(&t).unwrap(),
                Message::Ping(_) | Message::Pong(_) => continue,
                other => panic!("unexpected frame {other:?}"),
            }
        }
    }
    fn write_ws(ws: &mut WebSocket<UnixStream>, v: Value) {
        ws.send(Message::Text(v.to_string().into())).unwrap();
    }

    #[test]
    fn read_next_times_out_on_hung_server() {
        // A connected-but-silent peer: the only way out is the read deadline
        // (rosary-72fc26). `from_raw_socket` skips the upgrade to avoid the
        // accept/connect race that once flaked CI.
        let (client_end, peer_end) = UnixStream::pair().unwrap();
        let mut ws = tungstenite::WebSocket::from_raw_socket(
            client_end,
            tungstenite::protocol::Role::Client,
            None,
        );
        let started = Instant::now();
        let err = read_next_jsonrpc_message(&mut ws, Duration::from_millis(300))
            .expect_err("a hung app-server must not block forever");
        assert!(
            err.to_string().contains("timed out"),
            "expected a deterministic timeout, got: {err:#}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "must bail near the 300ms deadline"
        );
        drop(peer_end);
    }

    #[tokio::test]
    async fn runtime_completes_turn_over_real_socket() {
        // End-to-end over a real socket: connect+initialize, thread/start,
        // turn/start, then a streamed turn/completed the loop must observe so
        // wait() resolves true (not the old hardcoded false).
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("codex.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();

            let init = read_ws(&mut ws);
            assert_eq!(init["method"], "initialize");
            write_ws(
                &mut ws,
                json!({"jsonrpc": "2.0", "id": init["id"], "result": {}}),
            );
            assert_eq!(read_ws(&mut ws)["method"], "initialized");

            let thread = read_ws(&mut ws);
            assert_eq!(thread["method"], "thread/start");
            write_ws(
                &mut ws,
                json!({"jsonrpc": "2.0", "id": thread["id"],
                       "result": {"thread": {"id": "thread-sock"}}}),
            );

            let turn = read_ws(&mut ws);
            assert_eq!(turn["method"], "turn/start");
            assert_eq!(turn["params"]["threadId"], "thread-sock");
            write_ws(
                &mut ws,
                json!({"jsonrpc": "2.0", "id": turn["id"], "result": {"turn": {"id": "t1"}}}),
            );

            // Stream the terminal event the loop is waiting for.
            write_ws(
                &mut ws,
                json!({"jsonrpc": "2.0", "method": "turn/completed", "params": {}}),
            );
            // Keep the connection open until the client disconnects, so the
            // terminal event isn't lost to a close-race — the client closes once
            // its event-loop thread reads turn/completed and resolves wait().
            let _ = ws.read();
        });

        let runtime =
            CodexAppServerRuntime::new(Arc::new(CodexUnixSocketConnector::new(socket_path)));
        let start = CodexThreadStart {
            bead_id: Some("rosary-codex-e2e".into()),
            agent_name: Some("dev-agent".into()),
            prompt: "work over the socket".into(),
            work_dir: PathBuf::from("/tmp/rsry-codex-work"),
            permissions: PermissionProfile::Implement,
            system_prompt: "developer rules".into(),
            mcp_servers: std::collections::BTreeMap::new(),
            expected_mcp_tools: vec!["rsry".into()],
            model: None,
        };

        let mut session = runtime.start_thread(start).expect("runtime starts");
        assert_eq!(
            session.session_ref(),
            Some(AgentSessionRef::new("codex", "thread-sock"))
        );
        assert!(
            session.wait().await.expect("wait resolves"),
            "wait() must observe the streamed turn/completed"
        );
        server.join().expect("fake app-server exits");
    }
}
