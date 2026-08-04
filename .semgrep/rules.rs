// Test fixtures for .semgrep/rules.yml (same stem → `semgrep --test` pairs them).
// `ruleid:` marks a line that MUST match; `ok:` a line that must NOT.
// Excluded from the main `task lint` scan via .semgrepignore.

async fn blocks_with_args() {
    // ruleid: blocking-subprocess-in-async
    std::process::Command::new("git").args(["status"]).output().unwrap();
}

async fn blocks_direct_status() {
    // ruleid: blocking-subprocess-in-async
    std::process::Command::new("git").status().unwrap();
}

async fn uses_tokio_ok() {
    // ok: blocking-subprocess-in-async
    tokio::process::Command::new("git").args(["status"]).output().await.unwrap();
}

fn sync_context_ok() {
    // ok: blocking-subprocess-in-async
    std::process::Command::new("git").output().unwrap();
}

async fn non_blocking_spawn_ok() {
    // ok: blocking-subprocess-in-async
    let _ = std::process::Command::new("git").spawn();
}

async fn transition_before_audit_log() {
    // ruleid: audit-log-before-status-change
    self.persist_status(id, state, evidence).await;
    client.log_event(id, "state", "done").await;
}

async fn transition_before_audit_comment() {
    // ruleid: audit-log-before-status-change
    self.persist_status(id, state, evidence).await;
    client.add_comment(id, "note", "rosary").await;
}

async fn audit_first_ok() {
    // ok: audit-log-before-status-change
    client.log_event(id, "state", "done").await;
    self.persist_status(id, state, evidence).await;
}

async fn raw_close_reaches_past_the_gate() {
    // ruleid: bead-close-bypasses-gate
    store.close_bead(&id).await.ok();
}

async fn gated_close_via_bead_ops_ok() {
    // ok: bead-close-bypasses-gate
    bead_ops::close_bead(store.as_ref(), &id, &repo_name, force).await?;
}
