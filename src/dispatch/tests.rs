use super::*;
use std::path::PathBuf;
use tempfile::TempDir;

#[tokio::test]
async fn dispatch_missing_beads_dir_errors() {
    let dir = TempDir::new().unwrap();
    let result = run("fake-id", dir.path(), false).await;
    assert!(result.is_err());
}

#[test]
fn claude_provider_name() {
    let provider = ClaudeProvider;
    assert_eq!(provider.name(), "claude");
}

#[test]
fn gemini_provider_name() {
    let provider = GeminiProvider::default();
    assert_eq!(provider.name(), "gemini");
}

#[test]
fn gemini_provider_extra_args() {
    let provider = GeminiProvider {
        extra_args: vec!["--approval-mode".into(), "yolo".into()],
    };
    assert_eq!(provider.extra_args.len(), 2);
    assert_eq!(provider.name(), "gemini");
}

#[test]
fn provider_by_name_claude() {
    let p = provider_by_name("claude").unwrap();
    assert_eq!(p.name(), "claude");
}

#[test]
fn provider_by_name_gemini() {
    let p = provider_by_name("gemini").unwrap();
    assert_eq!(p.name(), "gemini");
}

#[test]
fn provider_by_name_unknown() {
    assert!(provider_by_name("copilot").is_err());
}

#[test]
fn permission_profile_from_issue_type() {
    // bug/task/feature → Implement
    assert_eq!(
        PermissionProfile::Implement,
        match "bug" {
            "review" | "survey" | "audit" => PermissionProfile::ReadOnly,
            "epic" | "plan" | "triage" => PermissionProfile::Plan,
            _ => PermissionProfile::Implement,
        }
    );
    // review → ReadOnly
    assert_eq!(
        PermissionProfile::ReadOnly,
        match "review" {
            "review" | "survey" | "audit" => PermissionProfile::ReadOnly,
            "epic" | "plan" | "triage" => PermissionProfile::Plan,
            _ => PermissionProfile::Implement,
        }
    );
    // epic → Plan
    assert_eq!(
        PermissionProfile::Plan,
        match "epic" {
            "review" | "survey" | "audit" => PermissionProfile::ReadOnly,
            "epic" | "plan" | "triage" => PermissionProfile::Plan,
            _ => PermissionProfile::Implement,
        }
    );
}

#[test]
fn permission_profile_claude_tools() {
    assert!(
        PermissionProfile::Implement
            .claude_allowed_tools()
            .contains("Edit")
    );
    assert!(
        !PermissionProfile::ReadOnly
            .claude_allowed_tools()
            .contains("Edit")
    );
    assert!(
        PermissionProfile::Plan
            .claude_allowed_tools()
            .contains("mcp__rsry__")
    );
}

#[test]
fn permission_profile_gemini_mode() {
    assert_eq!(
        PermissionProfile::Implement.gemini_approval_mode(),
        "auto_edit"
    );
    assert_eq!(PermissionProfile::ReadOnly.gemini_approval_mode(), "plan");
}

#[test]
fn build_prompt_includes_title_and_description() {
    let bead = crate::bead::Bead {
        id: "test-1".into(),
        title: "Fix the widget".into(),
        description: "The widget is broken".into(),
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
        branch: None,
        pr_url: None,
        jj_change_id: None,
        external_ref: None,
        files: Vec::new(),
        test_files: Vec::new(),
    };

    let prompt = build_prompt(&bead, "/tmp/test-repo", None, None);
    assert!(prompt.contains("Fix the widget"));
    assert!(prompt.contains("The widget is broken"));
    assert!(prompt.contains("task test"));
    assert!(prompt.contains("test-1"), "prompt should include bead ID");
    assert!(
        prompt.contains("/tmp/test-repo"),
        "prompt should include repo path"
    );
    assert!(
        prompt.contains("rsry_bead_close"),
        "prompt should instruct agent to close bead"
    );
    // XML structure
    assert!(prompt.contains("<task>"), "prompt should use XML tags");
    assert!(prompt.contains("<bead>"), "prompt should wrap bead in XML");
    assert!(
        prompt.contains("<instructions>"),
        "prompt should wrap instructions in XML"
    );
}

/// Regression: when a workspace is provided, the Repo: line must point
/// to the workspace (where the agent works), NOT the main repo.
/// The MCP bead_close instruction must still use the main repo path
/// (where .beads/ lives). This prevents agents from writing changes
/// to the main working tree instead of their isolated worktree.
#[test]
fn build_prompt_uses_workspace_for_repo_line() {
    let bead = crate::bead::Bead {
        id: "iso-1".into(),
        title: "Test isolation".into(),
        description: "Ensure workspace isolation".into(),
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
        branch: None,
        pr_url: None,
        jj_change_id: None,
        external_ref: None,
        files: Vec::new(),
        test_files: Vec::new(),
    };

    let ws = PathBuf::from("/home/user/.rsry/worktrees/myrepo/iso-1");
    let prompt = build_prompt(&bead, "/home/user/repos/myrepo", Some(&ws), None);

    // Repo: line must show the WORKSPACE path (agent's working directory)
    assert!(
        prompt.contains("/home/user/.rsry/worktrees/myrepo/iso-1"),
        "Repo line must point to workspace, not main repo. Got:\n{prompt}"
    );
    // MCP bead_close must still use the MAIN repo path (where .beads/ lives)
    assert!(
        prompt.contains("repo_path=\"/home/user/repos/myrepo\""),
        "bead_close repo_path must point to main repo. Got:\n{prompt}"
    );
    // Repo: line must NOT contain the main repo path as the workspace repo
    assert!(
        !prompt.contains("Repo: /home/user/repos/myrepo\n"),
        "Repo line must NOT show main repo path when workspace exists. Got:\n{prompt}"
    );
}

#[test]
fn build_prompt_varies_framing_by_agent() {
    let bead = crate::bead::Bead {
        id: "framing-1".into(),
        title: "Test framing".into(),
        description: "Agent framing varies".into(),
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
        branch: None,
        pr_url: None,
        jj_change_id: None,
        external_ref: None,
        files: Vec::new(),
        test_files: Vec::new(),
    };

    // Default (dev-agent) framing
    let dev = build_prompt(&bead, "/tmp/repo", None, None);
    assert!(dev.contains("Fix this issue"), "dev framing: {dev}");

    // Staging-agent framing
    let staging = build_prompt(&bead, "/tmp/repo", None, Some("staging-agent"));
    assert!(
        staging.contains("Review this change"),
        "staging framing: {staging}"
    );

    // Architect-agent framing
    let arch = build_prompt(&bead, "/tmp/repo", None, Some("architect-agent"));
    assert!(
        arch.contains("Analyze this problem"),
        "architect framing: {arch}"
    );
}

#[test]
fn prompt_version_is_set() {
    assert!(
        PROMPT_VERSION.starts_with('v'),
        "PROMPT_VERSION should start with 'v'"
    );
    let assembled = build_system_prompt(None, None);
    assert!(
        assembled.contains(PROMPT_VERSION),
        "assembled system prompt should contain version"
    );
}

// -- AgentSession tests --

#[tokio::test]
async fn cli_session_success() {
    let child = tokio::process::Command::new("true").spawn().unwrap();
    let mut session = CliSession::new(child);
    let success = session.wait().await.unwrap();
    assert!(success);
}

#[tokio::test]
async fn cli_session_failure() {
    let child = tokio::process::Command::new("false").spawn().unwrap();
    let mut session = CliSession::new(child);
    let success = session.wait().await.unwrap();
    assert!(!success);
}

#[tokio::test]
async fn cli_session_try_wait_completed() {
    let child = tokio::process::Command::new("true").spawn().unwrap();
    let mut session = CliSession::new(child);
    // Wait for it to finish
    session.wait().await.unwrap();
    // try_wait should return Some now
    // (already waited, so this is a no-op — just verifying the API)
}

#[tokio::test]
async fn cli_session_kill() {
    let child = tokio::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let mut session = CliSession::new(child);
    assert!(session.pid().is_some());
    session.kill().unwrap();
    // After kill, wait should return (not hang)
    let _success = session.wait().await.unwrap();
}

#[tokio::test]
async fn cli_session_pid() {
    let child = tokio::process::Command::new("sleep")
        .arg("0.1")
        .spawn()
        .unwrap();
    let session = CliSession::new(child);
    assert!(session.pid().is_some());
}

#[test]
fn agent_session_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<CliSession>();
}

#[tokio::test]
async fn agent_handle_session_id() {
    let child = tokio::process::Command::new("true").spawn().unwrap();
    let mut handle = AgentHandle {
        bead_id: "test-1".into(),
        generation: 1,
        session: Box::new(CliSession::new(child)),
        work_dir: PathBuf::from("/tmp"),
        started_at: chrono::Utc::now(),
        workspace: None,
        session_id: None,
        workspace_path: Some("/tmp/.rsry-workspaces/test-1".into()),
        log_path: Some(PathBuf::from("/tmp/.rsry-stream.jsonl")),
    };

    assert!(handle.session_id.is_none());
    handle.set_session_id("sess-abc-123".into());
    assert_eq!(handle.session_id.as_deref(), Some("sess-abc-123"));
    assert_eq!(
        handle.workspace_path.as_deref(),
        Some("/tmp/.rsry-workspaces/test-1")
    );
}

// -- Agent definition loading tests --

#[test]
fn strip_frontmatter_basic() {
    let content = "---\nname: dev-agent\ndescription: test\n---\n\n# Dev Agent\n\nBody here.";
    let stripped = strip_frontmatter(content);
    assert!(stripped.starts_with("# Dev Agent"));
    assert!(!stripped.contains("name: dev-agent"));
}

#[test]
fn strip_frontmatter_no_frontmatter() {
    let content = "# Just Markdown\n\nNo frontmatter here.";
    assert_eq!(strip_frontmatter(content), content);
}

#[test]
fn strip_frontmatter_empty() {
    assert_eq!(strip_frontmatter(""), "");
}

#[test]
fn strip_frontmatter_only_opening() {
    let content = "---\nno closing delimiter";
    assert_eq!(strip_frontmatter(content), content);
}

#[test]
fn load_agent_prompt_from_tempdir() {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("dev-agent.md"),
        "---\nname: dev-agent\n---\n\n# Dev Agent\n\nYou review code.",
    )
    .unwrap();

    let result = load_agent_prompt(dir.path(), "dev-agent");
    assert!(result.is_some());
    let body = result.unwrap();
    assert!(body.contains("# Dev Agent"));
    assert!(body.contains("You review code."));
    assert!(!body.contains("name: dev-agent"));
}

#[test]
fn load_agent_prompt_missing_file() {
    let dir = TempDir::new().unwrap();
    assert!(load_agent_prompt(dir.path(), "nonexistent-agent").is_none());
}

#[test]
fn load_agent_prompt_with_md_extension() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("test.md"), "# Test").unwrap();
    assert!(load_agent_prompt(dir.path(), "test.md").is_some());
}

#[test]
fn build_system_prompt_no_agent() {
    let prompt = build_system_prompt(None, None);
    assert!(prompt.contains("rosary-dispatched agent"));
    assert!(!prompt.contains("Agent Perspective"));
    assert!(!prompt.contains("Golden Rules"));
}

#[test]
fn build_system_prompt_with_agent() {
    let dir = TempDir::new().unwrap();
    let rules_dir = dir.path().join("rules");
    std::fs::create_dir(&rules_dir).unwrap();
    std::fs::write(
        rules_dir.join("GOLDEN_RULES.md"),
        "# Golden Rules\n\n1. Be minimal.",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("dev-agent.md"),
        "---\nname: dev-agent\n---\n\n# Dev Agent\n\nFind complexity hotspots.",
    )
    .unwrap();

    let prompt = build_system_prompt(Some("dev-agent"), Some(dir.path()));
    assert!(prompt.contains("rosary-dispatched agent"));
    assert!(prompt.contains("Golden Rules"));
    assert!(prompt.contains("Be minimal"));
    assert!(prompt.contains("Agent Perspective"));
    assert!(prompt.contains("Find complexity hotspots"));
}

#[test]
fn build_system_prompt_missing_agent_falls_back() {
    let dir = TempDir::new().unwrap();
    let rules_dir = dir.path().join("rules");
    std::fs::create_dir(&rules_dir).unwrap();
    std::fs::write(rules_dir.join("GOLDEN_RULES.md"), "# Rules").unwrap();

    let prompt = build_system_prompt(Some("nonexistent-agent"), Some(dir.path()));
    // Should still have base prompt + golden rules, just no agent section
    assert!(prompt.contains("rosary-dispatched agent"));
    assert!(prompt.contains("Golden Rules"));
    assert!(!prompt.contains("Agent Perspective"));
}

#[test]
fn pipeline_bug() {
    assert_eq!(agent_pipeline("bug"), &["dev-agent", "staging-agent"]);
}

#[test]
fn pipeline_feature() {
    assert_eq!(
        agent_pipeline("feature"),
        &["dev-agent", "staging-agent", "prod-agent"]
    );
}

#[test]
fn pipeline_task() {
    assert_eq!(agent_pipeline("task"), &["dev-agent"]);
}

#[test]
fn default_agent_maps_issue_type() {
    assert_eq!(default_agent("bug"), "dev-agent");
    assert_eq!(default_agent("review"), "staging-agent");
    assert_eq!(default_agent("epic"), "pm-agent");
    assert_eq!(default_agent("xyz"), "dev-agent");
}

#[test]
fn next_agent_advances() {
    assert_eq!(next_agent("bug", "dev-agent"), Some("staging-agent"));
    assert_eq!(next_agent("bug", "staging-agent"), None);
    assert_eq!(next_agent("feature", "staging-agent"), Some("prod-agent"));
    assert_eq!(next_agent("task", "dev-agent"), None);
    assert_eq!(next_agent("bug", "unknown"), None);
}
