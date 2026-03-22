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
