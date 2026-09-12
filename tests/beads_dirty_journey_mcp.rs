//! The `.beads/beads.jsonl` dirty-plague journey (rosary-3d455a) from the
//! surface that caused most of it: an agent commenting over MCP stdio from
//! Claude Code (rosary-e5fcd2). Same five observations as the CLI journey in
//! `beads_dirty_journey.rs`; the store write is `rsry serve --transport
//! stdio` driven over line-delimited JSON-RPC (`initialize`, then
//! `tools/call rsry_bead_comment`), the exact path `rsry_bead_comment` takes
//! from a live Claude session.
//!
//! RED by design until rosary-3d455a P1–P4 land (F1 removes the `#[ignore]`).
//! Run with `cargo test --test beads_dirty_journey_mcp -- --ignored`.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

#[path = "common/journey.rs"]
mod mcp_journey;
use mcp_journey::Journey;

/// A live `rsry serve --transport stdio` child driven the way Claude Code
/// drives it: one JSON-RPC request per line on stdin, one response per line
/// on stdout.
struct McpStdio {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr: std::thread::JoinHandle<String>,
    next_id: u64,
}

impl McpStdio {
    fn spawn(j: &mut Journey) -> Self {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_rsry"));
        cmd.args(["serve", "--transport", "stdio"])
            .current_dir(&j.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        j.env(&mut cmd);
        let mut child = cmd.spawn().expect("spawn rsry serve --transport stdio");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let stderr = drain_stderr(child.stderr.take().unwrap());
        j.transcript
            .push("$ rsry serve --transport stdio  (spawned, stdin piped)".into());
        Self {
            child,
            stdin,
            stdout,
            stderr,
            next_id: 0,
        }
    }

    /// The MCP handshake Claude Code performs before any tool call.
    fn handshake(&mut self, j: &mut Journey) {
        let init = self.request(
            j,
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "beads_dirty_journey_mcp", "version": "0"}
            }),
        );
        assert!(
            init.get("result").is_some(),
            "initialize must succeed (fixture): {init}"
        );
        self.notify("notifications/initialized");
    }

    fn notify(&mut self, method: &str) {
        let line = json!({"jsonrpc": "2.0", "method": method}).to_string();
        writeln!(self.stdin, "{line}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn request(&mut self, j: &mut Journey, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let req = json!({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params});
        writeln!(self.stdin, "{req}").unwrap();
        self.stdin.flush().unwrap();
        let mut line = String::new();
        let n = self.stdout.read_line(&mut line).expect("read MCP response");
        assert!(n > 0, "MCP server closed stdout before answering {method}");
        let resp: Value = serde_json::from_str(line.trim()).expect("MCP response is JSON");
        j.transcript.push(format!(
            "> {method} {}\n< {}",
            params,
            line.trim().chars().take(200).collect::<String>()
        ));
        resp
    }

    /// `tools/call`, asserting the tool itself did not report an error — a
    /// refused write would make every observation vacuously green.
    fn call(&mut self, j: &mut Journey, tool: &str, args: Value) -> Value {
        let resp = self.request(j, "tools/call", json!({"name": tool, "arguments": args}));
        assert!(
            resp.get("error").is_none() && resp["result"]["isError"] != json!(true),
            "{tool} failed (fixture, not the journey): {resp}\n\ntranscript:\n{}",
            j.transcript.join("\n")
        );
        resp
    }

    fn shutdown(mut self) -> String {
        drop(self.stdin);
        self.child.wait().ok();
        self.stderr.join().unwrap_or_default()
    }
}

/// Drain the server's stderr on a thread so a chatty startup can never block
/// the pipe; the log is appended to the transcript at shutdown.
fn drain_stderr(mut err: std::process::ChildStderr) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut err, &mut buf).ok();
        buf
    })
}

#[test]
#[ignore = "rosary-3d455a: RED until P1–P4 land; run with -- --ignored"]
fn a_bead_commented_over_mcp_stdio_on_a_feature_branch_does_not_dirty_or_block_the_checkout() {
    let mut j = Journey::new();
    // Both beads exist before the export is tracked: the journey's only store
    // writes are the MCP comments below.
    let ids = j.seed(&["feature work", "unrelated note"]);
    let (id, other) = (ids[0].clone(), ids[1].clone());
    j.start_feature();
    let repo_path = j.root.to_string_lossy().into_owned();
    let mut mcp = McpStdio::spawn(&mut j);
    mcp.handshake(&mut j);

    // (1) one store write over MCP stdio — `rsry_bead_comment`, as an agent
    //     in Claude Code does it.
    mcp.call(
        &mut j,
        "rsry_bead_comment",
        json!({"repo_path": repo_path, "id": id, "body": "agent: starting on this"}),
    );
    j.observe_write_clean();

    // Unrelated churn from the same session: a comment on another bead.
    mcp.call(
        &mut j,
        "rsry_bead_comment",
        json!({"repo_path": repo_path, "id": other, "body": "agent: noted"}),
    );

    // (2) explicit-path commit through the real pre-commit hook.
    j.observe_explicit_commit(&id, &other);

    // (3) an MCP write AFTER the commit, then push through the real pre-push hook.
    mcp.call(
        &mut j,
        "rsry_bead_comment",
        json!({"repo_path": repo_path, "id": id, "body": "opened PR #1"}),
    );
    j.observe_push_feature("opened PR #1");

    // (4) switch back to main with no stash; (5) push main is not refused.
    j.observe_checkout_main_and_push();
    let server_log = mcp.shutdown();
    j.transcript.push(format!(
        "[rsry serve stderr]\n    {}",
        server_log.trim().replace('\n', "\n    ")
    ));
    j.finish("MCP stdio (rsry_bead_comment)");
}
