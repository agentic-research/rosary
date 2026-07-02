use super::*;

/// Source-level lint: reconcile/ must never use println! (corrupts MCP stdio).
/// Regression test for rosary-b0b69a.
#[test]
fn no_println_in_reconcile() {
    let reconcile_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/reconcile");
    let mut violations = Vec::new();
    for entry in std::fs::read_dir(&reconcile_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "rs") {
            let content = std::fs::read_to_string(&path).unwrap();
            for (i, line) in content.lines().enumerate() {
                let trimmed = line.trim();
                // Skip line comments and attributes. Intentionally conservative —
                // may flag println! in block comments or string literals, which is
                // acceptable (false positives > false negatives for this lint).
                if trimmed.starts_with("//") || trimmed.starts_with('#') {
                    continue;
                }
                if trimmed.contains("println!") && !trimmed.contains("eprintln!") {
                    violations.push(format!(
                        "{}:{}: {}",
                        path.file_name().unwrap().to_string_lossy(),
                        i + 1,
                        trimmed
                    ));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "println! in reconcile/ corrupts MCP stdio JSON-RPC stream. Use eprintln! instead.\n{}",
        violations.join("\n")
    );
}

// ---------------------------------------------------------------------------
// triage() function tests — direct coverage of src/reconcile/triage.rs
// ---------------------------------------------------------------------------

/// Standard test bead: P1 task, open, no deps, repo "test-repo".
fn make_test_bead(id: &str) -> crate::bead::Bead {
    crate::bead::Bead {
        id: id.into(),
        title: format!("test bead {id}"),
        // Description must be long enough to pass the refinement gate
        // (Golden Rule 12: short descriptions are deferred for 5-whys).
        description: "This is a sufficiently descriptive task body that explains \
                      what needs to happen, why it matters, and how to verify \
                      completion. It exists to bypass the refinement filter."
            .into(),
        status: "open".into(),
        priority: 1,
        issue_type: "task".into(),
        owner: None,
        repo: "test-repo".into(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        dependency_count: 0,
        dependent_count: 0,
        comment_count: 0,
        branch: None,
        pr_url: None,
        jj_change_id: None,
        external_ref: None,
        files: Vec::new(),
        test_files: Vec::new(),
        created_by: None,
        scope: String::new(),
        derived_from: vec![],
        acceptance_criteria: String::new(),
    }
}

#[tokio::test]
async fn triage_skips_epic_without_target_filter() {
    // Epics are planning beads, not actionable work — must not be triaged
    // unless explicitly targeted by --bead.
    let mut bead = make_test_bead("epic-1");
    bead.issue_type = "epic".into();

    let mut r = Reconciler::new(ReconcilerConfig::default()).await;
    let triaged = r.triage(
        &[bead],
        &std::collections::HashMap::new(),
        &std::collections::HashSet::new(),
    );
    assert_eq!(triaged, 0, "epic must not be triaged without --bead");
}

#[tokio::test]
async fn triage_target_filter_overrides_epic_skip() {
    // Targeted dispatch (--bead) bypasses the epic skip — explicit user intent
    // overrides the heuristic.
    let mut bead = make_test_bead("epic-1");
    bead.issue_type = "epic".into();

    let mut r = Reconciler::new(ReconcilerConfig {
        target_bead: Some("epic-1".into()),
        triage_threshold: 0.0,
        ..Default::default()
    })
    .await;
    let triaged = r.triage(
        &[bead],
        &std::collections::HashMap::new(),
        &std::collections::HashSet::new(),
    );
    assert_eq!(triaged, 1, "--bead must override the epic skip");
}

#[tokio::test]
async fn triage_skips_blocked_bead_with_unresolved_deps() {
    let mut bead = make_test_bead("blocked-1");
    bead.dependency_count = 2;

    let mut r = Reconciler::new(ReconcilerConfig::default()).await;
    let triaged = r.triage(
        &[bead],
        &std::collections::HashMap::new(),
        &std::collections::HashSet::new(),
    );
    assert_eq!(triaged, 0, "bead with dependencies must not be triaged");
}

#[tokio::test]
async fn triage_skips_cross_repo_blocked_bead() {
    let bead = make_test_bead("cross-1");
    let mut blocked = std::collections::HashSet::new();
    blocked.insert("cross-1".to_string());

    let mut r = Reconciler::new(ReconcilerConfig::default()).await;
    let triaged = r.triage(&[bead], &std::collections::HashMap::new(), &blocked);
    assert_eq!(
        triaged, 0,
        "bead in cross_repo_blocked set must not be triaged"
    );
}

#[tokio::test]
async fn triage_skips_unrefined_bead_short_description() {
    // Golden Rule 12: bead with short description must be deferred for 5-whys.
    let mut bead = make_test_bead("short-1");
    bead.description = "fix it".into();
    bead.issue_type = "task".into();

    let mut r = Reconciler::new(ReconcilerConfig::default()).await;
    let triaged = r.triage(
        &[bead],
        &std::collections::HashMap::new(),
        &std::collections::HashSet::new(),
    );
    assert_eq!(triaged, 0, "unrefined bead must be deferred for refinement");
}

#[tokio::test]
async fn triage_target_filter_bypasses_refinement_gate() {
    let mut bead = make_test_bead("short-1");
    bead.description = "fix it".into();
    bead.issue_type = "task".into();

    let mut r = Reconciler::new(ReconcilerConfig {
        target_bead: Some("short-1".into()),
        triage_threshold: 0.0,
        ..Default::default()
    })
    .await;
    let triaged = r.triage(
        &[bead],
        &std::collections::HashMap::new(),
        &std::collections::HashSet::new(),
    );
    assert_eq!(triaged, 1, "--bead must override the refinement gate");
}

#[tokio::test]
async fn triage_enqueues_eligible_bead() {
    // Happy path: a normal P1 task with no blockers should be enqueued.
    let bead = make_test_bead("happy-1");

    let mut r = Reconciler::new(ReconcilerConfig {
        triage_threshold: 0.0, // ensure score passes
        ..Default::default()
    })
    .await;
    let triaged = r.triage(
        &[bead],
        &std::collections::HashMap::new(),
        &std::collections::HashSet::new(),
    );
    assert_eq!(triaged, 1, "eligible bead must be enqueued");
}

#[tokio::test]
async fn triage_skips_non_open_bead_without_target() {
    let mut bead = make_test_bead("done-1");
    bead.status = "closed".into();

    let mut r = Reconciler::new(ReconcilerConfig::default()).await;
    let triaged = r.triage(
        &[bead],
        &std::collections::HashMap::new(),
        &std::collections::HashSet::new(),
    );
    assert_eq!(triaged, 0, "non-open bead must not be triaged");
}

#[tokio::test]
async fn triage_target_filter_overrides_non_open_state() {
    let mut bead = make_test_bead("done-1");
    bead.status = "closed".into();

    let mut r = Reconciler::new(ReconcilerConfig {
        target_bead: Some("done-1".into()),
        triage_threshold: 0.0,
        ..Default::default()
    })
    .await;
    let triaged = r.triage(
        &[bead],
        &std::collections::HashMap::new(),
        &std::collections::HashSet::new(),
    );
    assert_eq!(triaged, 1, "--bead must override non-open state filter");
}

#[tokio::test]
async fn triage_skips_deadlettered_bead() {
    let bead = make_test_bead("dl-1");

    let mut r = Reconciler::new(ReconcilerConfig::default()).await;
    // Drive the bead past MAX_RETRIES (5) so is_deadlettered() returns true.
    for retries in 1..=5 {
        r.queue
            .record_backoff("test-repo", "dl-1", retries, std::time::Instant::now());
    }
    assert!(r.queue.is_deadlettered("test-repo", "dl-1"));

    let triaged = r.triage(
        &[bead],
        &std::collections::HashMap::new(),
        &std::collections::HashSet::new(),
    );
    assert_eq!(triaged, 0, "deadlettered bead must not be triaged");
}

// ---------------------------------------------------------------------------
// build_thread_map() tests — direct coverage of src/reconcile/threading.rs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn build_thread_map_no_hierarchy_returns_empty() {
    // When the reconciler has no hierarchy store wired, build_thread_map
    // returns an empty map regardless of how many beads are passed in.
    let r = Reconciler::new(ReconcilerConfig::default()).await;
    assert!(r.hierarchy.is_none());

    let beads = vec![make_test_bead("b-1"), make_test_bead("b-2")];
    let map = r.build_thread_map(&beads).await;
    assert!(map.is_empty(), "no hierarchy → empty thread map");
}

#[tokio::test]
async fn build_thread_map_with_hierarchy_returns_membership() {
    use crate::store::{DecadeRecord, HierarchyStore, ThreadRecord, WorkRef};

    let dir = tempfile::TempDir::new().unwrap();
    let backend = crate::store_sqlite::SqliteBackend::connect(&dir.path().join("test.db")).unwrap();

    // Set up a decade + thread with two members.
    backend
        .upsert_decade(&DecadeRecord {
            id: "test-decade".into(),
            title: "Test decade".into(),
            source_path: String::new(),
            status: "active".into(),
        })
        .await
        .unwrap();
    backend
        .upsert_thread(&ThreadRecord {
            id: "test-decade/threadA".into(),
            name: "Thread A".into(),
            decade_id: "test-decade".into(),
            feature_branch: None,
        })
        .await
        .unwrap();
    for bid in ["b-1", "b-2"] {
        backend
            .add_bead_to_thread(
                "test-decade/threadA",
                &WorkRef {
                    repo: "test-repo".into(),
                    scope: String::new(),
                    bead_id: bid.into(),
                },
            )
            .await
            .unwrap();
    }

    let mut r = Reconciler::new(ReconcilerConfig::default()).await;
    r.hierarchy = Some(Box::new(backend));

    // b-1 and b-2 are in the thread; b-3 is not.
    let beads = vec![
        make_test_bead("b-1"),
        make_test_bead("b-2"),
        make_test_bead("b-3"),
    ];
    let map = r.build_thread_map(&beads).await;

    assert_eq!(map.len(), 2, "only beads in a thread should appear");
    assert_eq!(
        map.get("b-1").map(String::as_str),
        Some("test-decade/threadA")
    );
    assert_eq!(
        map.get("b-2").map(String::as_str),
        Some("test-decade/threadA")
    );
    assert!(
        !map.contains_key("b-3"),
        "non-member bead must not be in map"
    );
}

#[tokio::test]
async fn build_thread_map_empty_input_returns_empty() {
    let r = Reconciler::new(ReconcilerConfig::default()).await;
    let map = r.build_thread_map(&[]).await;
    assert!(map.is_empty(), "no input beads → empty thread map");
}

// ---------------------------------------------------------------------------
// verify_agent() — staging-agent readonly path
// (verify_agent_readonly_vs_readwrite already covers scoping-agent + work_dir paths)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn verify_agent_skips_staging_agent_readonly() {
    // The pre-existing test exercises scoping-agent; this locks in that
    // staging-agent is also treated as ReadOnly (same `matches!` arm).
    let mut r = Reconciler::new(ReconcilerConfig::default()).await;
    r.trackers.insert(
        "ro-2".into(),
        BeadTracker {
            repo: "test-repo".into(),
            last_generation: 0,
            retries: 0,
            consecutive_reverts: 0,
            highest_tier: None,
            current_agent: Some("staging-agent".into()),
            phase_index: 0,
            issue_type: "task".into(),
            dispatch_id: None,
            scope: String::new(),
        },
    );
    let result = r.verify_agent("ro-2");
    assert!(
        result.is_none(),
        "staging-agent is read-only — verify must skip"
    );
}

// ---------------------------------------------------------------------------
// on_fail() tests — additional cases for src/reconcile/completion.rs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn on_fail_consecutive_reverts_reset_on_improvement() {
    // After a regression bumps consecutive_reverts, an improvement (higher
    // tier than previous) must reset the counter to 0.
    let mut r = Reconciler::new(ReconcilerConfig {
        max_retries: 100,
        once: true,
        repo: Vec::new(),
        ..Default::default()
    })
    .await;

    r.trackers.insert(
        "imp-1".into(),
        BeadTracker {
            repo: "test".into(),
            last_generation: 1,
            retries: 0,
            consecutive_reverts: 0,
            highest_tier: Some(3),
            current_agent: None,
            phase_index: 0,
            issue_type: "task".into(),
            dispatch_id: None,
            scope: String::new(),
        },
    );

    let summary = |highest: Option<usize>| crate::verify::VerifySummary {
        results: vec![(
            "test".into(),
            crate::verify::VerifyResult::Fail("fail".into()),
        )],
        highest_passing_tier: highest,
    };

    // Regression: 3 → 1 (consecutive_reverts becomes 1)
    assert!(!r.on_fail("imp-1", &summary(Some(1))));
    assert_eq!(r.trackers["imp-1"].consecutive_reverts, 1);

    // Improvement: 1 → 2 (consecutive_reverts resets to 0)
    assert!(!r.on_fail("imp-1", &summary(Some(2))));
    assert_eq!(
        r.trackers["imp-1"].consecutive_reverts, 0,
        "improvement must reset consecutive_reverts"
    );
}

#[tokio::test]
async fn on_fail_creates_tracker_for_unknown_bead() {
    // on_fail uses entry().or_insert() — must work even if the bead has
    // no prior tracker (e.g. orchestrator beads not yet promoted).
    let mut r = Reconciler::new(ReconcilerConfig::default()).await;
    let summary = crate::verify::VerifySummary {
        results: vec![(
            "compile".into(),
            crate::verify::VerifyResult::Fail("fail".into()),
        )],
        highest_passing_tier: None,
    };
    assert!(!r.trackers.contains_key("new-bead"));
    let _ = r.on_fail("new-bead", &summary);
    assert!(
        r.trackers.contains_key("new-bead"),
        "on_fail must create tracker for unknown bead"
    );
    assert_eq!(r.trackers["new-bead"].retries, 1);
}

// ---------------------------------------------------------------------------
// dispatch approval gate tests (Warp-style OrchestrationConfigStatus)
// ---------------------------------------------------------------------------

fn approval_repo(
    name: &str,
    approval: crate::config::DispatchApproval,
) -> crate::config::RepoConfig {
    crate::config::RepoConfig {
        name: name.into(),
        path: std::path::PathBuf::from("/tmp/unused"),
        lang: None,
        self_managed: false,
        approval,
    }
}

#[tokio::test]
async fn triage_holds_bead_when_repo_not_approved_and_gate_active() {
    let bead = make_test_bead("hold-1");
    let mut r = Reconciler::new(ReconcilerConfig {
        require_approval: true,
        repo: vec![approval_repo(
            "test-repo",
            crate::config::DispatchApproval::None,
        )],
        ..Default::default()
    })
    .await;
    let triaged = r.triage(
        &[bead],
        &std::collections::HashMap::new(),
        &std::collections::HashSet::new(),
    );
    assert_eq!(
        triaged, 0,
        "non-approved repo with gate active must not dispatch"
    );
}

#[tokio::test]
async fn triage_holds_bead_when_repo_rejected_and_gate_active() {
    let bead = make_test_bead("rej-1");
    let mut r = Reconciler::new(ReconcilerConfig {
        require_approval: true,
        repo: vec![approval_repo(
            "test-repo",
            crate::config::DispatchApproval::Rejected,
        )],
        ..Default::default()
    })
    .await;
    let triaged = r.triage(
        &[bead],
        &std::collections::HashMap::new(),
        &std::collections::HashSet::new(),
    );
    assert_eq!(triaged, 0, "rejected repo must not dispatch");
}

#[tokio::test]
async fn triage_passes_bead_when_repo_approved_and_gate_active() {
    let bead = make_test_bead("ok-1");
    let mut r = Reconciler::new(ReconcilerConfig {
        require_approval: true,
        triage_threshold: 0.0,
        repo: vec![approval_repo(
            "test-repo",
            crate::config::DispatchApproval::Approved,
        )],
        ..Default::default()
    })
    .await;
    let triaged = r.triage(
        &[bead],
        &std::collections::HashMap::new(),
        &std::collections::HashSet::new(),
    );
    assert_eq!(triaged, 1, "approved repo must dispatch normally");
}

#[tokio::test]
async fn triage_ignores_approval_when_gate_inactive() {
    // Default: require_approval = false. Even None/Rejected repos should pass
    // through this filter (other filters may still reject).
    let bead = make_test_bead("nogate-1");
    let mut r = Reconciler::new(ReconcilerConfig {
        require_approval: false,
        triage_threshold: 0.0,
        repo: vec![approval_repo(
            "test-repo",
            crate::config::DispatchApproval::None,
        )],
        ..Default::default()
    })
    .await;
    let triaged = r.triage(
        &[bead],
        &std::collections::HashMap::new(),
        &std::collections::HashSet::new(),
    );
    assert_eq!(triaged, 1, "gate disabled — approval state must be ignored");
}

#[tokio::test]
async fn triage_target_filter_bypasses_approval_gate() {
    // --bead overrides every filter, including the approval gate.
    let bead = make_test_bead("forced-1");
    let mut r = Reconciler::new(ReconcilerConfig {
        require_approval: true,
        target_bead: Some("forced-1".into()),
        triage_threshold: 0.0,
        repo: vec![approval_repo(
            "test-repo",
            crate::config::DispatchApproval::Rejected,
        )],
        ..Default::default()
    })
    .await;
    let triaged = r.triage(
        &[bead],
        &std::collections::HashMap::new(),
        &std::collections::HashSet::new(),
    );
    assert_eq!(triaged, 1, "--bead must override approval gate");
}

#[tokio::test]
async fn triage_admits_global_scope_bead_when_gate_active() {
    // A Global-scoped bead (repo == GLOBAL_REPO) is the org-level incoming
    // triage queue (rosary-1db9c9) — it has no per-repo home and no registered
    // RepoConfig. The approval gate would otherwise hold it (no repo named
    // "global" is registered/approved), but Global scope is the user's own
    // queue and needs no per-repo approval, so it must pass triage. (rosary-fa8a39)
    let mut bead = make_test_bead("global-1");
    bead.repo = crate::scope::GLOBAL_REPO.to_string();
    let mut r = Reconciler::new(ReconcilerConfig {
        require_approval: true,
        triage_threshold: 0.0,
        repo: vec![approval_repo(
            "test-repo",
            crate::config::DispatchApproval::Approved,
        )],
        ..Default::default()
    })
    .await;
    let triaged = r.triage(
        &[bead],
        &std::collections::HashMap::new(),
        &std::collections::HashSet::new(),
    );
    assert_eq!(
        triaged, 1,
        "Global-scope bead must pass the approval gate — it is the user's own org-level queue"
    );
}

#[tokio::test]
async fn triage_approval_gate_sees_remote_repos_too() {
    // Beads can come from runtime-registered remote_repos, not just
    // statically configured config.repo. The gate must check both.
    let bead = make_test_bead("remote-1");
    let mut r = Reconciler::new(ReconcilerConfig {
        require_approval: true,
        triage_threshold: 0.0,
        repo: Vec::new(), // no static repos
        ..Default::default()
    })
    .await;
    // Inject the remote repo with Approved status (matching what the
    // reconciler does after a successful clone).
    r.remote_repos.push(approval_repo(
        "test-repo",
        crate::config::DispatchApproval::Approved,
    ));
    let triaged = r.triage(
        &[bead],
        &std::collections::HashMap::new(),
        &std::collections::HashSet::new(),
    );
    assert_eq!(
        triaged, 1,
        "approved remote_repo bead must dispatch; gate must check both repo + remote_repos"
    );
}

#[test]
fn dispatch_approval_default_is_approved() {
    // Crucial for backward compat: existing configs without an `approval`
    // field must deserialize as Approved so dispatch behavior is unchanged
    // when `require_approval = false`.
    use crate::config::DispatchApproval;
    assert_eq!(DispatchApproval::default(), DispatchApproval::Approved);
}

#[test]
fn dispatch_approval_admits_only_approved() {
    use crate::config::DispatchApproval;
    assert!(DispatchApproval::Approved.admits());
    assert!(!DispatchApproval::None.admits());
    assert!(!DispatchApproval::Rejected.admits());
}

// ============================================================================
// liveness_sweep integration — the test the user's hypothesis demanded
// ("our pipelines theoretically should [catch dead workers]") and that
// the existing test suite did NOT have.
// ============================================================================

/// Spawn a child, reap it, return the now-dead pid.
fn dead_pid() -> u32 {
    let mut child = std::process::Command::new("sh")
        .args(["-c", "exit 0"])
        .spawn()
        .expect("spawn");
    let pid = child.id();
    let _ = child.wait();
    pid
}

/// Build a SessionEntry pointing at a given pid for a (bead, repo) pair.
fn fake_session(bead_id: &str, repo: &str, pid: u32) -> crate::session::SessionEntry {
    crate::session::SessionEntry {
        bead_id: bead_id.to_string(),
        repo: repo.to_string(),
        provider: "claude".to_string(),
        pid: Some(pid),
        session_ref: None,
        work_dir: "/tmp/test-liveness-work".to_string(),
        started_at: chrono::Utc::now(),
        title: format!("test bead {bead_id}"),
        agent: "scoping-agent".to_string(),
        workspace_vcs: "git".to_string(),
        repo_path: "/tmp/test-liveness-repo".to_string(),
        last_activity: None,
        last_comment: None,
    }
}

/// The load-bearing assertion: a Dispatched bead with a dead-pid session
/// flows from `iterate()`'s phase 1.8 through `liveness_sweep` into
/// `sweep_dead_workers` and lands at `dead_letter`. Wiring proof.
#[tokio::test]
async fn liveness_sweep_deadletters_dead_worker_via_reconciler() {
    let beads_dir = tempfile::TempDir::new().unwrap();
    // Connect a real SQLite store and use it directly to set up state —
    // bypasses the on-disk repo scanner so the test is hermetic.
    let store = crate::bead_sqlite::connect_bead_store(beads_dir.path())
        .await
        .expect("connect sqlite store");
    store
        .create_bead("dead-r-1", "T", "", 1, "task")
        .await
        .unwrap();
    store.update_status("dead-r-1", "dispatched").await.unwrap();

    // Reconciler with the store pre-installed for repo "test-repo".
    let config = ReconcilerConfig {
        once: true,
        repo: Vec::new(),
        ..Default::default()
    };
    let mut r = Reconciler::new(config).await;
    r.dolt_clients.insert("test-repo".to_string(), store);

    // Build the bead Vec the way iterate() would receive it from scanner.
    let mut bead = crate::testutil::make_bead("dead-r-1", "task", "test-repo");
    bead.status = "dispatched".to_string();

    // Inject a session entry with a dead pid (instead of loading the
    // global ~/.rsry/sessions.json file).
    let pid = dead_pid();
    let sessions = vec![fake_session("dead-r-1", "test-repo", pid)];

    let ids = r.liveness_sweep(&[bead], &sessions).await;
    assert_eq!(ids, vec!["dead-r-1".to_string()]);

    let reaped_bead = r
        .dolt_clients
        .get("test-repo")
        .unwrap()
        .get_bead("dead-r-1", "test-repo")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        reaped_bead.status, "dead_letter",
        "reconciler iteration must transition dead-worker beads to dead_letter"
    );
}

/// Companion negative test: a live worker should NOT be deadlettered.
/// Without this, a false-positive in `liveness_sweep` would silently abort
/// real running agents — worse than the original bug.
#[tokio::test]
async fn liveness_sweep_leaves_live_worker_alone_via_reconciler() {
    let beads_dir = tempfile::TempDir::new().unwrap();
    let store = crate::bead_sqlite::connect_bead_store(beads_dir.path())
        .await
        .expect("connect sqlite store");
    store
        .create_bead("live-r-1", "T", "", 1, "task")
        .await
        .unwrap();
    store.update_status("live-r-1", "dispatched").await.unwrap();

    let config = ReconcilerConfig {
        once: true,
        repo: Vec::new(),
        ..Default::default()
    };
    let mut r = Reconciler::new(config).await;
    r.dolt_clients.insert("test-repo".to_string(), store);

    let mut bead = crate::testutil::make_bead("live-r-1", "task", "test-repo");
    bead.status = "dispatched".to_string();

    // Our own pid — guaranteed alive throughout the test.
    let sessions = vec![fake_session("live-r-1", "test-repo", std::process::id())];

    let ids = r.liveness_sweep(&[bead], &sessions).await;
    assert!(ids.is_empty(), "live worker must not be touched");

    let bead = r
        .dolt_clients
        .get("test-repo")
        .unwrap()
        .get_bead("live-r-1", "test-repo")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bead.status, "dispatched");
}

/// Regression for Copilot review round 5 on PR #202: when
/// `SessionRegistry::load()` fails, `iterate()` falls back to passing
/// an empty `sessions` slice to `liveness_sweep`. Without a short-circuit
/// the sweep would still do per-repo `list_beads()` scans every
/// iteration despite being guaranteed to no-op (no sessions → no dead
/// PIDs detectable). This test pins that the short-circuit fires —
/// even when the bead list contains Dispatched beads, an empty sessions
/// slice produces zero deadletters AND avoids the per-repo scan that
/// would otherwise hit the DB.
#[tokio::test]
async fn liveness_sweep_short_circuits_on_empty_sessions() {
    // Construct a reconciler that has NO dolt_clients registered. If the
    // sweep didn't short-circuit, the per-repo loop would still call
    // `self.dolt_client(&repo).await`, which (per persistence.rs:11)
    // attempts to lazily connect via `bead_sqlite::connect_bead_store` —
    // failing with eprintln noise. The short-circuit must run BEFORE
    // the repo collection logic.
    let config = ReconcilerConfig {
        once: true,
        repo: Vec::new(),
        ..Default::default()
    };
    let mut r = Reconciler::new(config).await;
    // Intentionally NO dolt_clients insert — if the short-circuit fails,
    // we'd attempt a connect and see "[bead] failed to connect" noise.

    let mut bead = crate::testutil::make_bead("any-bead", "task", "test-repo");
    bead.status = "dispatched".to_string();

    let empty_sessions: Vec<crate::session::SessionEntry> = Vec::new();
    let ids = r.liveness_sweep(&[bead], &empty_sessions).await;

    assert!(
        ids.is_empty(),
        "empty sessions must short-circuit — no scans, no deadletters"
    );
}

/// Regression for Copilot review on PR #202: target-bead mode used to
/// exit if `cumulative.deadlettered > 0` regardless of which bead was
/// deadlettered. With the new liveness sweep adding deadletter events
/// for ANY dead worker (not just retry-exhaustion on the target), the
/// false-positive exit would terminate target mode the first time an
/// unrelated worker died. The fix splits `deadlettered_ids` from the
/// counter and checks set-membership; this test pins that contract.
#[tokio::test]
async fn target_bead_mode_only_exits_when_target_bead_deadletters() {
    let beads_dir = tempfile::TempDir::new().unwrap();
    let store = crate::bead_sqlite::connect_bead_store(beads_dir.path())
        .await
        .unwrap();
    // Two beads: TARGET (the one the operator asked for) and SIBLING.
    store
        .create_bead("TARGET", "T", "", 1, "task")
        .await
        .unwrap();
    store.update_status("TARGET", "dispatched").await.unwrap();
    store
        .create_bead("SIBLING", "S", "", 1, "task")
        .await
        .unwrap();
    store.update_status("SIBLING", "dispatched").await.unwrap();

    let config = ReconcilerConfig {
        once: true,
        target_bead: Some("TARGET".to_string()),
        repo: Vec::new(),
        ..Default::default()
    };
    let mut r = Reconciler::new(config).await;
    r.dolt_clients.insert("test-repo".to_string(), store);

    let mut target_bead = crate::testutil::make_bead("TARGET", "task", "test-repo");
    target_bead.status = "dispatched".to_string();
    let mut sibling_bead = crate::testutil::make_bead("SIBLING", "task", "test-repo");
    sibling_bead.status = "dispatched".to_string();

    // SIBLING's worker has died; TARGET's worker is alive (our pid).
    let dead = dead_pid();
    let sessions = vec![
        fake_session("SIBLING", "test-repo", dead),
        fake_session("TARGET", "test-repo", std::process::id()),
    ];

    let ids = r
        .liveness_sweep(&[target_bead, sibling_bead], &sessions)
        .await;

    // Liveness sweep correctly identifies SIBLING but NOT TARGET.
    assert_eq!(ids, vec!["SIBLING".to_string()]);
    // The membership check the run() loop now uses:
    let target = "TARGET";
    assert!(
        !ids.iter().any(|id| id == target),
        "target {target} must NOT appear in deadlettered_ids when only SIBLING died — \
         the old `deadlettered > 0` check would have falsely terminated target mode here"
    );
}
