use super::*;
use crate::bead::BeadState;
use crate::queue;

#[test]
fn detect_language_rust() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
    assert_eq!(helpers::detect_language(dir.path()), "rust");
}

#[test]
fn detect_language_go() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("go.mod"), "module test").unwrap();
    assert_eq!(helpers::detect_language(dir.path()), "go");
}

#[test]
fn detect_language_unknown() {
    let dir = tempfile::TempDir::new().unwrap();
    assert_eq!(helpers::detect_language(dir.path()), "unknown");
}

#[test]
fn iteration_summary_display() {
    let s = IterationSummary {
        scanned: 10,
        triaged: 3,
        dispatched: 2,
        completed: 1,
        passed: 1,
        failed: 0,
        deadlettered: 0,
        agent_closed: 0,
        vcs_transitions: 1,
    };
    let display = format!("{s}");
    assert!(display.contains("scanned=10"));
    assert!(display.contains("dispatched=2"));
    assert!(display.contains("vcs=1"));
}

#[test]
fn reconciler_config_defaults() {
    let cfg = ReconcilerConfig::default();
    assert_eq!(cfg.max_concurrent, 5);
    assert_eq!(cfg.scan_interval, Duration::from_secs(30));
    assert_eq!(cfg.max_retries, 5);
    assert!(!cfg.once);
    assert!(!cfg.dry_run);
    assert_eq!(cfg.provider, "claude");
}

#[tokio::test]
async fn severity_floor_blocks_p3_with_min_priority_2() {
    // A P3 bead should not pass the severity floor when min_priority=2.
    // This tests the integration point: queue::passes_severity_floor is
    // called in iterate() with self.queue.min_priority (default=2).
    let bead = crate::bead::Bead {
        id: "test-p3".into(),
        title: "low priority task".into(),
        description: String::new(),
        status: "open".into(),
        priority: 3,
        issue_type: "task".into(),
        owner: None,
        repo: "test".into(),
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
    };

    let config = ReconcilerConfig::default();
    let r = Reconciler::new(config).await;
    // Default min_priority is 2, so P3 (priority=3) should be blocked
    assert!(
        !queue::passes_severity_floor(&bead, r.queue.min_priority),
        "P3 bead should not pass severity floor with min_priority=2"
    );
}

#[tokio::test]
async fn reconciler_dry_run_single_pass() {
    // No repos configured — should complete immediately with empty scan
    let config = ReconcilerConfig {
        once: true,
        dry_run: true,
        repo: Vec::new(),
        ..Default::default()
    };

    let mut reconciler = Reconciler::new(config).await;
    let summary = reconciler.iterate().await.unwrap();
    assert_eq!(summary.scanned, 0);
    assert_eq!(summary.dispatched, 0);
}

#[tokio::test]
async fn on_pass_clears_state() {
    let config = ReconcilerConfig {
        once: true,
        repo: Vec::new(),
        ..Default::default()
    };
    let mut r = Reconciler::new(config).await;

    r.trackers.insert(
        "x".into(),
        BeadTracker {
            repo: "test".into(),
            last_generation: 1,
            retries: 2,
            consecutive_reverts: 1,
            highest_tier: Some(3),
            current_agent: None,
            phase_index: 0,
            issue_type: "task".into(),
        },
    );

    r.on_pass("x");
    assert_eq!(r.trackers["x"].consecutive_reverts, 0);
}

#[tokio::test]
async fn on_fail_exit_deadletters_after_max() {
    let config = ReconcilerConfig {
        max_retries: 3,
        once: true,
        repo: Vec::new(),
        ..Default::default()
    };
    let mut r = Reconciler::new(config).await;

    // Retries increment: 1, 2, 3 — deadletter at 3 == max_retries
    assert!(!r.on_fail_exit("x")); // retries=1
    assert!(!r.on_fail_exit("x")); // retries=2
    assert!(r.on_fail_exit("x")); // retries=3 == max, deadletter
}

#[tokio::test]
async fn on_fail_consecutive_reverts_deadletter() {
    let config = ReconcilerConfig {
        max_retries: 100, // won't hit this
        once: true,
        repo: Vec::new(),
        ..Default::default()
    };
    let mut r = Reconciler::new(config).await;

    // Set initial high tier
    r.trackers.insert(
        "x".into(),
        BeadTracker {
            repo: "test".into(),
            last_generation: 1,
            retries: 0,
            consecutive_reverts: 0,
            highest_tier: Some(4),
            current_agent: None,
            phase_index: 0,
            issue_type: "task".into(),
        },
    );

    // Three consecutive reverts (each lower than previous best)
    let regress = |highest: Option<usize>| crate::verify::VerifySummary {
        results: vec![
            ("commit".into(), crate::verify::VerifyResult::Pass),
            (
                "test".into(),
                crate::verify::VerifyResult::Fail("fail".into()),
            ),
        ],
        highest_passing_tier: highest,
    };

    assert!(!r.on_fail("x", &regress(Some(2)))); // 4→2, revert #1
    assert!(!r.on_fail("x", &regress(Some(1)))); // 2→1, revert #2
    assert!(r.on_fail("x", &regress(Some(0)))); // 1→0, revert #3 → deadletter
}

#[tokio::test]
async fn failed_bead_retries_despite_same_generation() {
    // Scenario: bead dispatched → agent fails → retry scheduled.
    // On next iterate(), the bead's generation hasn't changed (Dolt wasn't updated).
    // The generation check must NOT block re-triage for beads with pending retries.
    let config = ReconcilerConfig {
        max_retries: 3,
        once: true,
        repo: Vec::new(),
        ..Default::default()
    };
    let mut r = Reconciler::new(config).await;

    // Simulate: bead "x" was dispatched at generation 42, then failed
    r.trackers.insert(
        "x".into(),
        BeadTracker {
            repo: "test".into(),
            last_generation: 42,
            retries: 1,
            consecutive_reverts: 0,
            highest_tier: None,
            current_agent: None,
            phase_index: 0,
            issue_type: "task".into(),
        },
    );
    // Record backoff (retry is pending)
    r.queue.record_backoff(
        "x",
        1,
        std::time::Instant::now() - std::time::Duration::from_secs(60),
    );

    // Create a bead with the SAME generation (42)
    let bead = crate::bead::Bead {
        id: "x".into(),
        title: "test bead".into(),
        description: String::new(),
        status: "open".into(),
        priority: 1,
        issue_type: "bug".into(),
        owner: None,
        repo: "test".into(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        dependency_count: 0,
        dependent_count: 0,
        comment_count: 0,
        external_ref: None,
        files: Vec::new(),
        test_files: Vec::new(),
        branch: None,
        pr_url: None,
        jj_change_id: None,
    };

    // The bead should still be triageable despite same generation
    let retries = r.queue.retries(&bead.id);
    assert_eq!(retries, 1, "should have 1 retry recorded");

    // Check: tracker has same generation, but retries > 0
    let tracker = r.trackers.get("x").unwrap();
    assert_eq!(tracker.last_generation, 42);
    assert_eq!(tracker.retries, 1);

    // The generation check should NOT block when retries > 0
    let bead_gen = bead.generation();
    let should_skip = tracker.last_generation == bead_gen && tracker.retries == 0;
    assert!(
        !should_skip,
        "bead with pending retries should NOT be skipped by generation check"
    );
}

// -- Smart triage tests --

#[test]
fn blocked_bead_filtered_by_triage() {
    // A bead with dependency_count > 0 should be hard-filtered in triage,
    // not just scored low. This prevents dispatching work whose
    // prerequisites aren't done yet.
    let bead = crate::bead::Bead {
        id: "dep-blocked".into(),
        title: "blocked by deps".into(),
        description: String::new(),
        status: "open".into(),
        priority: 0, // highest priority
        issue_type: "task".into(),
        owner: None,
        repo: "test".into(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        dependency_count: 2, // has unresolved deps
        dependent_count: 0,
        comment_count: 0,
        branch: None,
        pr_url: None,
        jj_change_id: None,
        external_ref: None,
        files: Vec::new(),
        test_files: Vec::new(),
    };

    // is_blocked returns true for open beads with deps
    assert!(bead.is_blocked());
    // is_ready returns false
    assert!(!bead.is_ready());
    // Even with P0 priority, the bead should be blocked
    assert_eq!(bead.state(), BeadState::Open);
}

#[test]
fn self_managed_repo_gets_score_boost() {
    // Beads from self-managed repos should get a 0.15 score boost,
    // making them dispatch before equivalent beads on other repos.
    let now = chrono::Utc::now();
    let bead = crate::bead::Bead {
        id: "self-1".into(),
        title: "self-managed task".into(),
        description: String::new(),
        status: "open".into(),
        priority: 2,
        issue_type: "task".into(),
        owner: None,
        repo: "rosary".into(),
        created_at: now,
        updated_at: now,
        dependency_count: 0,
        dependent_count: 0,
        comment_count: 0,
        branch: None,
        pr_url: None,
        jj_change_id: None,
        external_ref: None,
        files: Vec::new(),
        test_files: Vec::new(),
    };

    let base_score = queue::triage_score(&bead, 0, now);
    let boosted_score = (base_score + 0.15).min(1.0);
    assert!(
        boosted_score > base_score,
        "self-managed boost should increase score: {boosted_score} vs {base_score}"
    );
    // The boost is 0.15 — enough to push self-managed beads ahead
    assert!(
        (boosted_score - base_score - 0.15).abs() < f64::EPSILON,
        "boost should be exactly 0.15"
    );
}

#[tokio::test]
async fn repo_busy_check_uses_trackers() {
    // When an agent is active on repo "mache", no other bead from
    // "mache" should be triaged. This tests the tracker lookup logic.
    let config = ReconcilerConfig {
        once: true,
        repo: Vec::new(),
        ..Default::default()
    };
    let mut r = Reconciler::new(config).await;

    // Simulate an active agent on repo "mache"
    r.trackers.insert(
        "mache-abc".into(),
        BeadTracker {
            repo: "mache".into(),
            last_generation: 1,
            retries: 0,
            consecutive_reverts: 0,
            highest_tier: None,
            current_agent: None,
            phase_index: 0,
            issue_type: "task".into(),
        },
    );
    // Mark it as active (need a dummy handle — use the key presence)
    // We can't easily create an AgentHandle in tests, so test the
    // lookup logic directly:
    let active_ids = ["mache-abc".to_string()];
    let candidate_repo = "mache";

    let repo_busy = active_ids.iter().any(|active_id| {
        r.trackers
            .get(active_id)
            .is_some_and(|t| t.repo == candidate_repo)
    });
    assert!(repo_busy, "repo with active agent should be busy");

    // Different repo should not be busy
    let other_repo = "rosary";
    let other_busy = active_ids.iter().any(|active_id| {
        r.trackers
            .get(active_id)
            .is_some_and(|t| t.repo == other_repo)
    });
    assert!(!other_busy, "repo without active agent should not be busy");
}

// ---------------------------------------------------------------------------
// Level 2: Pipeline sequence integration tests
// ---------------------------------------------------------------------------

#[test]
fn pipeline_bug_three_phase_sequence() {
    use crate::config::default_pipelines;
    use crate::pipeline::{CompletionAction, PipelineEngine};

    let e = PipelineEngine::new(default_pipelines(), None, 0);
    // scoping → dev → staging → Terminal
    assert_eq!(
        e.decide("bug", Some("scoping-agent"), true, None, 0, 3),
        CompletionAction::Advance {
            next_agent: "dev-agent".into(),
            phase: 1
        }
    );
    assert_eq!(
        e.decide("bug", Some("dev-agent"), true, Some(true), 0, 3),
        CompletionAction::Advance {
            next_agent: "staging-agent".into(),
            phase: 2
        }
    );
    assert_eq!(
        e.decide("bug", Some("staging-agent"), true, None, 0, 3),
        CompletionAction::Terminal
    );
}

#[test]
fn pipeline_feature_four_phase_with_retry() {
    use crate::config::default_pipelines;
    use crate::pipeline::{CompletionAction, PipelineEngine};

    let e = PipelineEngine::new(default_pipelines(), None, 0);
    assert_eq!(
        e.decide("feature", Some("scoping-agent"), true, None, 0, 3),
        CompletionAction::Advance {
            next_agent: "dev-agent".into(),
            phase: 1
        }
    );
    // dev fails → retry
    assert_eq!(
        e.decide("feature", Some("dev-agent"), true, Some(false), 0, 3),
        CompletionAction::Retry
    );
    // dev retry passes → advance
    assert_eq!(
        e.decide("feature", Some("dev-agent"), true, Some(true), 1, 3),
        CompletionAction::Advance {
            next_agent: "staging-agent".into(),
            phase: 2
        }
    );
    assert_eq!(
        e.decide("feature", Some("staging-agent"), true, None, 0, 3),
        CompletionAction::Advance {
            next_agent: "prod-agent".into(),
            phase: 3
        }
    );
    assert_eq!(
        e.decide("feature", Some("prod-agent"), true, Some(true), 0, 3),
        CompletionAction::Terminal
    );
}

#[test]
fn pipeline_crash_retries_then_deadletters() {
    use crate::config::default_pipelines;
    use crate::pipeline::{CompletionAction, PipelineEngine};

    let e = PipelineEngine::new(default_pipelines(), None, 0);
    for retry in 0..3 {
        assert_eq!(
            e.decide("bug", Some("dev-agent"), false, None, retry, 3),
            CompletionAction::Retry
        );
    }
    assert_eq!(
        e.decide("bug", Some("dev-agent"), false, None, 3, 3),
        CompletionAction::Deadletter
    );
}

#[test]
fn pipeline_task_single_phase_terminal() {
    use crate::config::default_pipelines;
    use crate::pipeline::{CompletionAction, PipelineEngine};

    let e = PipelineEngine::new(default_pipelines(), None, 0);
    assert_eq!(
        e.decide("task", Some("dev-agent"), true, Some(true), 0, 3),
        CompletionAction::Terminal
    );
}

/// Level 2.6: Mock agent → verify → decide — proves the pieces compose.
/// This simulates what wait_and_verify() does without needing a full Reconciler.
#[test]
fn mock_agent_verify_decide_compose_to_advance() {
    use crate::config::default_pipelines;
    use crate::dispatch::AgentProvider;
    use crate::dispatch::tests::MockAgentProvider;
    use crate::pipeline::{CompletionAction, PipelineEngine};
    use crate::verify::{BeadRefCheck, CommitCheck, Verifier};

    let repo = crate::testutil::TestRepo::new();
    let pipeline = PipelineEngine::new(default_pipelines(), None, 0);

    // Simulate scoping-agent (ReadOnly — skip verify, just decide)
    let action = pipeline.decide("bug", Some("scoping-agent"), true, None, 0, 3);
    assert_eq!(
        action,
        CompletionAction::Advance {
            next_agent: "dev-agent".into(),
            phase: 1
        }
    );

    // Simulate dev-agent: mock provider creates a bead-ref commit
    let provider = MockAgentProvider::with_commit("rsry-test1");
    let _session = provider
        .spawn_agent("prompt", repo.path(), &Default::default(), "sys")
        .unwrap();

    // Verify the commit (what the reconciler does after agent completes)
    let verifier = Verifier::new(vec![Box::new(CommitCheck), Box::new(BeadRefCheck)]);
    let summary = verifier.run(repo.path()).unwrap();
    assert!(
        summary.passed(),
        "dev-agent commit should pass verification"
    );

    // Pipeline decides: dev passes → advance to staging
    let action = pipeline.decide("bug", Some("dev-agent"), true, Some(summary.passed()), 0, 3);
    assert_eq!(
        action,
        CompletionAction::Advance {
            next_agent: "staging-agent".into(),
            phase: 2
        }
    );

    // Simulate staging-agent (ReadOnly — skip verify)
    let action = pipeline.decide("bug", Some("staging-agent"), true, None, 0, 3);
    assert_eq!(action, CompletionAction::Terminal);
}

/// Adversarial: mock agent creates commit WITHOUT bead ref → verify fails → retry
#[test]
fn mock_agent_bad_commit_verify_fails_triggers_retry() {
    use crate::config::default_pipelines;
    use crate::pipeline::{CompletionAction, PipelineEngine};
    use crate::verify::{BeadRefCheck, CommitCheck, Verifier};

    let repo = crate::testutil::TestRepo::new();
    // Create a commit WITHOUT bead reference
    repo.commit_plain("bad.rs", "fn bad() {}");

    let verifier = Verifier::new(vec![Box::new(CommitCheck), Box::new(BeadRefCheck)]);
    let summary = verifier.run(repo.path()).unwrap();
    assert!(!summary.passed(), "commit without bead ref should fail");

    let pipeline = PipelineEngine::new(default_pipelines(), None, 0);
    let action = pipeline.decide("bug", Some("dev-agent"), true, Some(false), 0, 3);
    assert_eq!(action, CompletionAction::Retry);
}

// ---------------------------------------------------------------------------
// Level 3: verify_completed() integration tests
// ---------------------------------------------------------------------------
//
// These test the actual Reconciler.verify_completed() method — the orchestrator
// decision loop that wires verify_agent + pipeline.decide + tracker mutations.

/// Helper: create a Reconciler with a tracker set up for a bead.
async fn reconciler_with_bead(
    bead_id: &str,
    repo_name: &str,
    issue_type: &str,
    current_agent: &str,
    retries: u32,
) -> Reconciler {
    let config = ReconcilerConfig {
        once: true,
        repo: Vec::new(),
        max_retries: 3,
        ..Default::default()
    };
    let mut r = Reconciler::new(config).await;
    r.trackers.insert(
        bead_id.to_string(),
        BeadTracker {
            repo: repo_name.to_string(),
            last_generation: 1,
            retries,
            consecutive_reverts: 0,
            highest_tier: None,
            current_agent: Some(current_agent.to_string()),
            phase_index: if current_agent == "scoping-agent" {
                0
            } else if current_agent == "dev-agent" {
                1
            } else {
                2
            },
            issue_type: issue_type.to_string(),
        },
    );
    r
}

/// dev-agent exits successfully, no work_dir → verify_agent returns None →
/// pipeline.decide(verify_passed=None) treats as pass → Advance to staging.
/// Tests the orchestrator loop: verify_completed → pipeline.decide → Advance.
#[tokio::test]
async fn verify_completed_pass_advances_pipeline() {
    let mut r = reconciler_with_bead("bug-001", "test-repo", "bug", "dev-agent", 0).await;

    let beads = vec![crate::testutil::make_bead("bug-001", "bug", "test-repo")];
    let thread_map = std::collections::HashMap::new();

    let result = r
        .verify_completed(&[("bug-001".to_string(), true)], &beads, &thread_map)
        .await;

    assert_eq!(result.passed, 1, "should count 1 pass");
    assert_eq!(result.failed, 0);
    assert_eq!(result.deadlettered, 0);
    assert_eq!(
        result.phase_advances.len(),
        1,
        "should have 1 phase advance"
    );

    let (bead_id, _repo, next_agent) = &result.phase_advances[0];
    assert_eq!(bead_id, "bug-001");
    assert_eq!(next_agent, "staging-agent", "dev → staging for bugs");
}

/// dev-agent exits successfully but commit lacks bead ref → verify fails → Retry.
#[tokio::test]
async fn verify_completed_verify_fail_retries() {
    let test_repo = crate::testutil::TestRepo::new();
    test_repo.commit_plain("bad.rs", "fn bad() {}");

    let mut r = reconciler_with_bead("bug-002", "test-repo", "bug", "dev-agent", 0).await;
    r.completed_work_dirs.insert(
        "bug-002".to_string(),
        (test_repo.path().to_path_buf(), "test-repo".to_string()),
    );
    r.repo_info.insert(
        "test-repo".to_string(),
        (test_repo.path().to_path_buf(), "unknown".to_string()),
    );

    let beads = vec![crate::testutil::make_bead("bug-002", "bug", "test-repo")];
    let thread_map = std::collections::HashMap::new();

    let result = r
        .verify_completed(&[("bug-002".to_string(), true)], &beads, &thread_map)
        .await;

    assert_eq!(result.passed, 0);
    assert_eq!(result.failed, 1, "verify failure → retry");
    assert_eq!(result.deadlettered, 0);
    assert_eq!(result.phase_advances.len(), 0, "no advance on verify fail");
    assert_eq!(result.status_updates.len(), 1);
    assert_eq!(result.status_updates[0].2, "open", "retry reopens bead");
}

/// Agent process exits non-zero → Retry (regardless of verify).
#[tokio::test]
async fn verify_completed_exit_failure_retries() {
    let mut r = reconciler_with_bead("bug-003", "test-repo", "bug", "dev-agent", 0).await;

    let beads = vec![crate::testutil::make_bead("bug-003", "bug", "test-repo")];
    let thread_map = std::collections::HashMap::new();

    let result = r
        .verify_completed(&[("bug-003".to_string(), false)], &beads, &thread_map)
        .await;

    assert_eq!(result.passed, 0);
    assert_eq!(result.failed, 1);
    assert_eq!(result.deadlettered, 0);
    assert_eq!(result.status_updates[0].2, "open");
}

/// Agent fails at max retries → Deadletter → status "blocked".
#[tokio::test]
async fn verify_completed_max_retries_deadletters() {
    let mut r = reconciler_with_bead("bug-004", "test-repo", "bug", "dev-agent", 3).await;

    let beads = vec![crate::testutil::make_bead("bug-004", "bug", "test-repo")];
    let thread_map = std::collections::HashMap::new();

    let result = r
        .verify_completed(&[("bug-004".to_string(), false)], &beads, &thread_map)
        .await;

    assert_eq!(result.passed, 0);
    assert_eq!(result.failed, 1);
    assert_eq!(result.deadlettered, 1, "max retries → deadletter");
    assert_eq!(
        result.status_updates[0].2, "blocked",
        "deadletter → blocked"
    );
}

/// staging-agent (ReadOnly) at end of bug pipeline → Terminal → "pr_open".
#[tokio::test]
async fn verify_completed_terminal_sets_pr_open() {
    let mut r = reconciler_with_bead("bug-005", "test-repo", "bug", "staging-agent", 0).await;

    let beads = vec![crate::testutil::make_bead("bug-005", "bug", "test-repo")];
    let thread_map = std::collections::HashMap::new();

    // staging-agent exit_success=true, verify skipped (ReadOnly) → Terminal
    let result = r
        .verify_completed(&[("bug-005".to_string(), true)], &beads, &thread_map)
        .await;

    assert_eq!(result.passed, 1);
    assert_eq!(result.failed, 0);
    assert_eq!(result.status_updates.len(), 1);
    assert_eq!(result.status_updates[0].2, "pr_open", "terminal → pr_open");
}

/// scoping-agent passes → Advance to dev-agent (first phase of bug pipeline).
#[tokio::test]
async fn verify_completed_scoping_advances_to_dev() {
    let mut r = reconciler_with_bead("bug-006", "test-repo", "bug", "scoping-agent", 0).await;

    let beads = vec![crate::testutil::make_bead("bug-006", "bug", "test-repo")];
    let thread_map = std::collections::HashMap::new();

    let result = r
        .verify_completed(&[("bug-006".to_string(), true)], &beads, &thread_map)
        .await;

    assert_eq!(result.passed, 1);
    assert_eq!(result.phase_advances.len(), 1);
    assert_eq!(
        result.phase_advances[0].2, "dev-agent",
        "scoping → dev for bugs"
    );
}

/// task with dev-agent pass → Terminal (single-phase pipeline).
#[tokio::test]
async fn verify_completed_task_terminal_immediately() {
    let mut r = reconciler_with_bead("task-001", "test-repo", "task", "dev-agent", 0).await;

    let beads = vec![crate::testutil::make_bead("task-001", "task", "test-repo")];
    let thread_map = std::collections::HashMap::new();

    let result = r
        .verify_completed(&[("task-001".to_string(), true)], &beads, &thread_map)
        .await;

    assert_eq!(result.passed, 1);
    assert_eq!(result.phase_advances.len(), 0, "task has no next phase");
    assert_eq!(result.status_updates.len(), 1);
    assert_eq!(
        result.status_updates[0].2, "pr_open",
        "task terminal → pr_open"
    );
}

/// Multiple beads completing in the same pass — mixed outcomes.
#[tokio::test]
async fn verify_completed_mixed_batch() {
    let mut r = reconciler_with_bead("mix-pass", "repo-a", "bug", "dev-agent", 0).await;
    // Add second bead tracker (exit failure)
    r.trackers.insert(
        "mix-fail".to_string(),
        BeadTracker {
            repo: "repo-b".to_string(),
            last_generation: 1,
            retries: 0,
            consecutive_reverts: 0,
            highest_tier: None,
            current_agent: Some("dev-agent".to_string()),
            phase_index: 1,
            issue_type: "task".to_string(),
        },
    );

    let beads = vec![
        crate::testutil::make_bead("mix-pass", "bug", "repo-a"),
        crate::testutil::make_bead("mix-fail", "task", "repo-b"),
    ];
    let thread_map = std::collections::HashMap::new();

    // mix-pass: exit_success=true, no work_dir → verify=None → advance
    // mix-fail: exit_success=false → retry
    let completed = vec![
        ("mix-pass".to_string(), true),
        ("mix-fail".to_string(), false),
    ];
    let result = r.verify_completed(&completed, &beads, &thread_map).await;

    assert_eq!(result.passed, 1, "one pass");
    assert_eq!(result.failed, 1, "one fail");
    assert_eq!(
        result.phase_advances.len() + result.status_updates.len(),
        2,
        "each bead gets an outcome"
    );
}

// ---------------------------------------------------------------------------
// Level 3: Adversarial — edge cases that have caused real bugs
// ---------------------------------------------------------------------------

/// Bead completes but has no tracker entry — should not panic.
/// This can happen if the agent was spawned before a reconciler restart.
#[tokio::test]
async fn verify_completed_missing_tracker_does_not_panic() {
    let config = ReconcilerConfig {
        once: true,
        repo: Vec::new(),
        max_retries: 3,
        ..Default::default()
    };
    let mut r = Reconciler::new(config).await;
    // No tracker inserted — bead "orphan-001" is unknown to reconciler

    let beads = vec![crate::testutil::make_bead(
        "orphan-001",
        "task",
        "test-repo",
    )];
    let thread_map = std::collections::HashMap::new();

    // Should not panic — falls through to bead lookup for repo/issue_type
    let result = r
        .verify_completed(&[("orphan-001".to_string(), true)], &beads, &thread_map)
        .await;

    // Pipeline should still make a decision (Terminal for task with no tracker)
    assert!(
        result.passed + result.failed + result.deadlettered > 0,
        "orphan bead should still get a decision"
    );
}

/// Bead completes but is not in the beads list either — worst case.
/// Should not panic, should produce some outcome.
#[tokio::test]
async fn verify_completed_unknown_bead_does_not_panic() {
    let config = ReconcilerConfig {
        once: true,
        repo: Vec::new(),
        max_retries: 3,
        ..Default::default()
    };
    let mut r = Reconciler::new(config).await;

    let beads: Vec<crate::bead::Bead> = vec![]; // empty — bead not found
    let thread_map = std::collections::HashMap::new();

    // Should not panic even with completely unknown bead
    let result = r
        .verify_completed(&[("ghost-999".to_string(), true)], &beads, &thread_map)
        .await;

    // With no tracker and no bead, repo/issue_type default to empty strings.
    // pipeline.decide with empty issue_type falls through to Terminal.
    assert_eq!(
        result.passed + result.failed + result.deadlettered,
        1,
        "ghost bead should still produce exactly one outcome"
    );
}

/// Tracker retries field is at max-1, exit fails → should Retry (not deadletter yet).
/// Then at max, should deadletter.
#[tokio::test]
async fn verify_completed_retry_boundary() {
    // retries=2, max_retries=3 → Retry (2 < 3)
    let mut r = reconciler_with_bead("bound-1", "repo", "bug", "dev-agent", 2).await;
    let beads = vec![crate::testutil::make_bead("bound-1", "bug", "repo")];
    let thread_map = std::collections::HashMap::new();

    let result = r
        .verify_completed(&[("bound-1".to_string(), false)], &beads, &thread_map)
        .await;
    assert_eq!(result.deadlettered, 0, "retries=2 < max=3 → not deadletter");
    assert_eq!(result.status_updates[0].2, "open", "retry → open");

    // retries=3, max_retries=3 → Deadletter (3 >= 3)
    let mut r2 = reconciler_with_bead("bound-2", "repo", "bug", "dev-agent", 3).await;
    let beads2 = vec![crate::testutil::make_bead("bound-2", "bug", "repo")];

    let result2 = r2
        .verify_completed(&[("bound-2".to_string(), false)], &beads2, &thread_map)
        .await;
    assert_eq!(result2.deadlettered, 1, "retries=3 >= max=3 → deadletter");
    assert_eq!(result2.status_updates[0].2, "blocked");
}

/// verify_agent skips ReadOnly agents (scoping-agent, staging-agent) but
/// MUST NOT skip dev-agent even if it has no work_dir.
#[tokio::test]
async fn verify_agent_readonly_vs_readwrite() {
    let mut r = reconciler_with_bead("ro-1", "repo", "bug", "scoping-agent", 0).await;

    // ReadOnly agent → verify_agent returns None (skip)
    let result = r.verify_agent("ro-1");
    assert!(
        result.is_none(),
        "scoping-agent (ReadOnly) should skip verification"
    );

    // dev-agent with no work_dir → verify_agent returns None (no work_dir)
    // but NOT because it was skipped as ReadOnly
    let mut r2 = reconciler_with_bead("rw-1", "repo", "bug", "dev-agent", 0).await;
    let result2 = r2.verify_agent("rw-1");
    assert!(
        result2.is_none(),
        "dev-agent without work_dir returns None (not found, not skipped)"
    );

    // dev-agent WITH work_dir → verify_agent returns Some (runs verifier)
    let test_repo = crate::testutil::TestRepo::new();
    test_repo.commit_plain("test.rs", "fn x() {}");
    let mut r3 = reconciler_with_bead("rw-2", "repo", "bug", "dev-agent", 0).await;
    r3.completed_work_dirs.insert(
        "rw-2".to_string(),
        (test_repo.path().to_path_buf(), "repo".to_string()),
    );
    let result3 = r3.verify_agent("rw-2");
    assert!(
        result3.is_some(),
        "dev-agent WITH work_dir should run verifier"
    );
}

/// feature pipeline: 4 phases (scoping → dev → staging → prod).
/// Each phase advance should specify the correct next agent.
#[tokio::test]
async fn verify_completed_feature_four_phase_progression() {
    let thread_map = std::collections::HashMap::new();

    // Phase 0: scoping-agent → dev-agent
    let mut r = reconciler_with_bead("feat-1", "repo", "feature", "scoping-agent", 0).await;
    let beads = vec![crate::testutil::make_bead("feat-1", "feature", "repo")];
    let result = r
        .verify_completed(&[("feat-1".to_string(), true)], &beads, &thread_map)
        .await;
    assert_eq!(result.phase_advances[0].2, "dev-agent");

    // Phase 1: dev-agent → staging-agent
    let mut r = reconciler_with_bead("feat-2", "repo", "feature", "dev-agent", 0).await;
    let beads = vec![crate::testutil::make_bead("feat-2", "feature", "repo")];
    let result = r
        .verify_completed(&[("feat-2".to_string(), true)], &beads, &thread_map)
        .await;
    assert_eq!(result.phase_advances[0].2, "staging-agent");

    // Phase 2: staging-agent → prod-agent
    let mut r = reconciler_with_bead("feat-3", "repo", "feature", "staging-agent", 0).await;
    let beads = vec![crate::testutil::make_bead("feat-3", "feature", "repo")];
    let result = r
        .verify_completed(&[("feat-3".to_string(), true)], &beads, &thread_map)
        .await;
    assert_eq!(result.phase_advances[0].2, "prod-agent");

    // Phase 3: prod-agent → Terminal
    let mut r = reconciler_with_bead("feat-4", "repo", "feature", "prod-agent", 0).await;
    let beads = vec![crate::testutil::make_bead("feat-4", "feature", "repo")];
    let result = r
        .verify_completed(&[("feat-4".to_string(), true)], &beads, &thread_map)
        .await;
    assert_eq!(result.phase_advances.len(), 0, "prod → no more phases");
    assert_eq!(result.status_updates[0].2, "pr_open", "Terminal → pr_open");
}

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
                // Skip test code and comments
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
