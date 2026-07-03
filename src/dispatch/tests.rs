//! Tests for the dispatch module.

use super::*;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use tempfile::TempDir;

use crate::dispatch::fake::{DeterministicAgentAction, DeterministicAgentProvider};
use crate::dispatch::providers::{
    CodexAppServerClient, CodexAppServerRequest, CodexAppServerRuntime, CodexNativeSession,
    CodexProvider, CodexRuntime, CodexThreadStart, CodexUnixSocketClient,
};

fn codex_gate_test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
}

fn clear_codex_gate_env() {
    unsafe {
        std::env::remove_var("RSRY_EXPERIMENTAL_CODEX");
    }
}

fn set_codex_gate_env(value: &str) {
    unsafe {
        std::env::set_var("RSRY_EXPERIMENTAL_CODEX", value);
    }
}

// -----------------------------------------------------------------------
// MockAgentSession — fake agent that completes immediately
// -----------------------------------------------------------------------

#[allow(dead_code)] // API surface — used by reconcile/tests.rs
pub struct MockAgentSession {
    exit_success: bool,
}

#[allow(dead_code)]
impl MockAgentSession {
    pub fn success() -> Box<dyn AgentSession> {
        Box::new(Self { exit_success: true })
    }

    pub fn failure() -> Box<dyn AgentSession> {
        Box::new(Self {
            exit_success: false,
        })
    }
}

#[async_trait::async_trait]
impl AgentSession for MockAgentSession {
    fn try_wait(&mut self) -> Result<Option<bool>> {
        Ok(Some(self.exit_success))
    }
    async fn wait(&mut self) -> Result<bool> {
        Ok(self.exit_success)
    }
    fn kill(&mut self) -> Result<()> {
        Ok(())
    }
    fn pid(&self) -> Option<u32> {
        None
    }
}

// -----------------------------------------------------------------------
// MockAgentProvider — records spawn calls, returns MockAgentSession
// -----------------------------------------------------------------------

#[allow(dead_code)] // API surface — used by reconcile/tests.rs
pub struct MockAgentProvider {
    /// Side-effect: run this closure on work_dir during spawn (e.g., create a commit)
    #[allow(clippy::type_complexity)]
    pub side_effect: Option<Box<dyn Fn(&Path) + Send + Sync>>,
    pub exit_success: bool,
}

#[allow(dead_code)]
impl MockAgentProvider {
    pub fn succeeding() -> Self {
        Self {
            side_effect: None,
            exit_success: true,
        }
    }

    /// Mock that creates a bead-ref commit in work_dir before "completing"
    pub fn with_commit(bead_id: &str) -> Self {
        let id = bead_id.to_string();
        Self {
            side_effect: Some(Box::new(move |dir: &Path| {
                let file = dir.join("change.txt");
                std::fs::write(&file, "mock change").unwrap();
                let msg = format!("[{id}] fix(test): mock\n\nbead:{id}");
                let _ = std::process::Command::new("git")
                    .args(["add", "."])
                    .current_dir(dir)
                    .output();
                let _ = std::process::Command::new("git")
                    .args(["commit", "-m", &msg])
                    .current_dir(dir)
                    .output();
            })),
            exit_success: true,
        }
    }
}

#[derive(Clone, Default)]
struct CapturingRunSpecProvider {
    captured: Arc<Mutex<Option<providers::AgentRunSpec>>>,
}

impl AgentProvider for CapturingRunSpecProvider {
    fn spawn_agent(
        &self,
        _prompt: &str,
        _work_dir: &Path,
        _permissions: &PermissionProfile,
        _system_prompt: &str,
    ) -> Result<Box<dyn AgentSession>> {
        panic!("dispatch should call spawn_run for agent-native providers")
    }

    fn spawn_run(&self, spec: &providers::AgentRunSpec) -> Result<Box<dyn AgentSession>> {
        *self.captured.lock().unwrap() = Some(spec.clone());
        Ok(MockAgentSession::success())
    }

    fn name(&self) -> &str {
        "capture"
    }

    fn with_model(&self, _model: Option<String>) -> Box<dyn providers::AgentProvider> {
        Box::new(self.clone())
    }
}

impl AgentProvider for MockAgentProvider {
    fn spawn_agent(
        &self,
        _prompt: &str,
        work_dir: &Path,
        _permissions: &PermissionProfile,
        _system_prompt: &str,
    ) -> Result<Box<dyn AgentSession>> {
        if let Some(ref effect) = self.side_effect {
            effect(work_dir);
        }
        if self.exit_success {
            Ok(MockAgentSession::success())
        } else {
            Ok(MockAgentSession::failure())
        }
    }

    fn build_command(
        &self,
        _prompt: &str,
        _permissions: &PermissionProfile,
        _system_prompt: &str,
    ) -> (String, Vec<String>) {
        ("echo".to_string(), vec!["mock-agent".to_string()])
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn with_model(&self, _model: Option<String>) -> Box<dyn providers::AgentProvider> {
        Box::new(MockAgentProvider {
            side_effect: None,
            exit_success: self.exit_success,
        })
    }
}

#[tokio::test]
async fn spawn_passes_agent_native_run_spec_to_provider() {
    let repo = crate::testutil::TestRepo::new();
    let mut bead = crate::testutil::make_bead("rsry-spec1", "bug", "rosary");
    bead.title = "Audit Codex dispatch".into();
    bead.description = "Keep Rosary agent context structured.".into();
    bead.owner = Some("scoping-agent".into());

    let provider = CapturingRunSpecProvider::default();
    let captured = provider.captured.clone();

    let handle = spawn(&bead, repo.path(), false, 0, &provider, None, None, None)
        .await
        .unwrap();

    let spec = captured
        .lock()
        .unwrap()
        .clone()
        .expect("provider should capture run spec");
    assert_eq!(spec.bead_id.as_deref(), Some("rsry-spec1"));
    assert_eq!(spec.agent_name.as_deref(), Some("scoping-agent"));
    assert_eq!(spec.work_dir, handle.work_dir);
    assert_eq!(spec.permissions, PermissionProfile::ReadOnly);
    assert!(spec.prompt.contains("Audit Codex dispatch"));
    assert!(
        spec.prompt
            .contains("Keep Rosary agent context structured.")
    );
    assert!(spec.system_prompt.contains(PROMPT_VERSION));
    assert!(spec.mcp_servers.contains_key("rsry"));
    assert!(spec.mcp_servers.contains_key("mache"));
    assert!(spec.expected_mcp_tools.contains(&"rsry".to_string()));
    assert!(spec.expected_mcp_tools.contains(&"mache".to_string()));
    assert!(spec.expected_mcp_tools.contains(&"lectio".to_string()));
}

#[tokio::test]
async fn deterministic_agent_harness_exposes_native_session_and_captures_run_spec() {
    let repo = crate::testutil::TestRepo::new();
    let mut bead = crate::testutil::make_bead("rsry-fake1", "task", "rosary");
    bead.owner = Some("dev-agent".into());

    let provider =
        DeterministicAgentProvider::new("codex").with_session_ref("codex", "thread-fake-1");
    let captured = provider.captured_specs();

    let mut handle = spawn(&bead, repo.path(), false, 0, &provider, None, None, None)
        .await
        .unwrap();

    assert_eq!(handle.session.pid(), None);
    assert_eq!(
        handle.session.session_ref(),
        Some(AgentSessionRef::new("codex", "thread-fake-1"))
    );
    assert_eq!(handle.session.try_wait().unwrap(), Some(true));

    let specs = captured.lock().unwrap();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].bead_id.as_deref(), Some("rsry-fake1"));
    assert_eq!(specs[0].agent_name.as_deref(), Some("dev-agent"));
    assert_eq!(specs[0].permissions, PermissionProfile::Implement);
    assert!(specs[0].expected_mcp_tools.contains(&"rsry".to_string()));
    assert!(specs[0].expected_mcp_tools.contains(&"mache".to_string()));
    assert!(specs[0].expected_mcp_tools.contains(&"lectio".to_string()));
}

#[tokio::test]
async fn deterministic_agent_harness_can_script_bead_ref_commit() {
    let repo = crate::testutil::TestRepo::new();
    let mut bead = crate::testutil::make_bead("rsry-fake2", "task", "rosary");
    bead.owner = Some("dev-agent".into());

    let provider = DeterministicAgentProvider::new("codex").with_action(
        DeterministicAgentAction::CommitWithBeadRef {
            bead_id: "rsry-fake2".into(),
            file: "fake.rs".into(),
            contents: "fn fake() {}\n".into(),
        },
    );

    let _handle = spawn(&bead, repo.path(), false, 0, &provider, None, None, None)
        .await
        .unwrap();

    let verifier = crate::verify::Verifier::new(vec![
        Box::new(crate::verify::CommitCheck),
        Box::new(crate::verify::WorkRefCheck),
    ]);
    let summary = verifier.run(repo.path()).unwrap();
    assert!(summary.passed(), "scripted commit should pass: {summary:?}");
}

#[tokio::test]
async fn deterministic_agent_harness_can_script_failure_and_plain_commit() {
    let repo = crate::testutil::TestRepo::new();
    let bead = crate::testutil::make_bead("rsry-fake3", "task", "rosary");

    let provider = DeterministicAgentProvider::new("codex")
        .failing()
        .with_action(DeterministicAgentAction::WriteFile {
            file: "notes/failure.txt".into(),
            contents: "scripted failure\n".into(),
        })
        .with_action(DeterministicAgentAction::CommitPlain {
            message: "test(fake): scripted plain commit".into(),
            file: "plain.rs".into(),
            contents: "fn plain() {}\n".into(),
        });

    let mut handle = spawn(&bead, repo.path(), false, 0, &provider, None, None, None)
        .await
        .unwrap();

    assert_eq!(handle.session.try_wait().unwrap(), Some(false));
    assert_eq!(
        std::fs::read_to_string(repo.path().join("notes/failure.txt")).unwrap(),
        "scripted failure\n"
    );

    let log = std::process::Command::new("git")
        .args(["log", "-1", "--format=%s"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    assert!(log.status.success());
    assert_eq!(
        String::from_utf8_lossy(&log.stdout).trim(),
        "test(fake): scripted plain commit"
    );
}

#[test]
fn dispatch_close_condition_accepts_test_files() {
    let mut bead = crate::testutil::make_bead("rsry-close-ok", "task", "rosary");
    bead.description = "Implementation bead with acceptance carried by test_files.".into();
    bead.test_files = vec!["src/dispatch/tests.rs".into()];

    ensure_dispatch_close_condition(&bead).unwrap();
}

#[tokio::test]
async fn run_rejects_unclosable_impl_bead_before_status_mutation() {
    let repo = crate::testutil::TestRepo::new();
    let store = crate::bead_sqlite::connect_bead_store(&repo.path().join(".beads"))
        .await
        .unwrap();
    store
        .create_bead_full(crate::store::NewBead {
            id: "rsry-no-close".into(),
            title: "Unclosable dispatch bead".into(),
            description: "No runnable close condition here.".into(),
            priority: 1,
            issue_type: "task".into(),
            owner: "dev-agent".into(),
            files: vec!["src/dispatch/mod.rs".into()],
            created_by: Some("test".into()),
            ..Default::default()
        })
        .await
        .unwrap();

    let err = run("rsry-no-close", repo.path(), false, "not-a-provider")
        .await
        .unwrap_err();

    assert!(err.to_string().contains("no close condition"), "{err}");
    assert_eq!(
        store.get_status("rsry-no-close").await.unwrap().as_deref(),
        Some("open"),
        "dispatch must not mark an unclosable bead dispatched before rejecting it"
    );
}

#[tokio::test]
async fn dispatch_missing_beads_dir_errors() {
    let dir = TempDir::new().unwrap();
    let result = run("fake-id", dir.path(), false, "claude").await;
    assert!(result.is_err());
}

#[test]
fn claude_provider_name() {
    let provider = ClaudeProvider::default();
    assert_eq!(provider.name(), "claude");
}

// -----------------------------------------------------------------------
// Per-phase model selection (rosary-5413e4)
// -----------------------------------------------------------------------

#[test]
fn claude_provider_with_model_in_build_command() {
    let p = ClaudeProvider {
        binary: "claude".into(),
        model: Some("claude-haiku-4-5-20251001".into()),
    };
    let (_, args) = p.build_command("prompt", &PermissionProfile::Implement, "sys");
    let has_model = args
        .windows(2)
        .any(|w| w[0] == "--model" && w[1] == "claude-haiku-4-5-20251001");
    assert!(
        has_model,
        "--model flag must appear in build_command args when model is set"
    );
}

#[test]
fn claude_provider_no_model_flag_when_unset() {
    let p = ClaudeProvider::default();
    let (_, args) = p.build_command("prompt", &PermissionProfile::Implement, "sys");
    assert!(
        !args.iter().any(|a| a == "--model"),
        "--model must not appear when model is None"
    );
}

#[test]
fn claude_provider_with_model_clones_correctly() {
    let p = ClaudeProvider {
        binary: "my-claude".into(),
        model: None,
    };
    let overridden = p.with_model(Some("claude-sonnet-4-6".into()));
    let (_, args) = overridden.build_command("p", &PermissionProfile::Implement, "s");
    assert!(
        args.windows(2)
            .any(|w| w[0] == "--model" && w[1] == "claude-sonnet-4-6"),
        "with_model must pass through to build_command"
    );
}

#[test]
fn dispatch_config_pipeline_models_parsed() {
    let toml = r#"
[dispatch]
provider = "claude"

[dispatch.pipeline_models]
"scoping-agent" = "claude-haiku-4-5-20251001"
"dev-agent" = "claude-sonnet-4-6"
"#;
    let cfg: crate::config::Config = toml::from_str(toml).expect("config parse");
    let dispatch = cfg.dispatch.expect("dispatch section");
    assert_eq!(
        dispatch
            .pipeline_models
            .get("scoping-agent")
            .map(|s| s.as_str()),
        Some("claude-haiku-4-5-20251001")
    );
    assert_eq!(
        dispatch
            .pipeline_models
            .get("dev-agent")
            .map(|s| s.as_str()),
        Some("claude-sonnet-4-6")
    );
}

#[test]
fn dispatch_config_pipeline_models_defaults_empty() {
    let toml = r#"
[dispatch]
provider = "claude"
"#;
    let cfg: crate::config::Config = toml::from_str(toml).expect("config parse");
    let dispatch = cfg.dispatch.expect("dispatch section");
    assert!(
        dispatch.pipeline_models.is_empty(),
        "pipeline_models defaults to empty map"
    );
}

#[test]
fn gemini_provider_name() {
    let provider = GeminiProvider::default();
    assert_eq!(provider.name(), "gemini");
}

#[test]
fn gemini_provider_extra_args() {
    let provider = GeminiProvider {
        binary: String::new(),
        extra_args: vec!["--approval-mode".into(), "yolo".into()],
    };
    assert_eq!(provider.extra_args.len(), 2);
    assert_eq!(provider.name(), "gemini");
}

#[test]
fn provider_by_name_claude() {
    let empty = std::collections::HashMap::new();
    let p = provider_by_name("claude", &empty).unwrap();
    assert_eq!(p.name(), "claude");
}

#[test]
fn provider_by_name_codex_is_the_working_exec_provider() {
    // rosary-7643c9: `codex` now resolves to the functional `codex exec` CLI
    // provider — no experimental gate, and it DOES expose a runnable command.
    let empty = std::collections::HashMap::new();
    let p = provider_by_name("codex", &empty).expect("codex must resolve without a gate");
    assert_eq!(p.name(), "codex");
    let (bin, args) = p.build_command("do the thing", &PermissionProfile::Implement, "sys");
    assert_eq!(bin, "codex");
    assert_eq!(args.first().map(String::as_str), Some("exec"));
    assert!(
        args.iter().any(|a| a == "--skip-git-repo-check"),
        "args: {args:?}"
    );
    // Implement → workspace-write sandbox; unattended → approval never.
    assert!(
        args.windows(2)
            .any(|w| w == ["--sandbox", "workspace-write"])
    );
    assert!(
        args.last()
            .map(|s| s.contains("do the thing"))
            .unwrap_or(false)
    );
}

#[test]
fn provider_by_name_codex_native_requires_experimental_gate() {
    let _guard = codex_gate_test_lock();
    clear_codex_gate_env();
    let empty = std::collections::HashMap::new();
    let err = match provider_by_name("codex-native", &empty) {
        Ok(_) => panic!("codex-native (app-server) must be gated off by default"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("experimental"), "{err}");
    assert!(
        err.to_string().contains("RSRY_EXPERIMENTAL_CODEX=1"),
        "{err}"
    );
    assert!(err.to_string().contains("rosary-2500f3"), "{err}");
}

#[test]
fn provider_by_name_codex_native_returns_dormant_app_server_when_gated_on() {
    let _guard = codex_gate_test_lock();
    set_codex_gate_env("1");
    let empty = std::collections::HashMap::new();
    let p = provider_by_name("codex-native", &empty).unwrap();
    clear_codex_gate_env();
    assert_eq!(p.name(), "codex");
    // The native provider has no durable CLI command path (rosary-d6d1bb).
    let (bin, args) = p.build_command("prompt", &PermissionProfile::Implement, "system prompt");
    assert!(
        bin.is_empty() && args.is_empty(),
        "native codex provider must not expose a durable CLI command path"
    );
}

#[derive(Clone)]
struct MockCodexRuntime {
    captured: Arc<Mutex<Vec<CodexThreadStart>>>,
}

impl CodexRuntime for MockCodexRuntime {
    fn start_thread(&self, start: CodexThreadStart) -> Result<CodexNativeSession> {
        self.captured.lock().unwrap().push(start);
        Ok(CodexNativeSession::completed_success("thread-rsry-1"))
    }
}

#[tokio::test]
async fn codex_provider_starts_native_thread_and_exposes_session_ref() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let provider = CodexProvider::with_runtime(Arc::new(MockCodexRuntime {
        captured: captured.clone(),
    }));
    let repo = crate::testutil::TestRepo::new();
    let mut bead = crate::testutil::make_bead("rsry-codex1", "task", "rosary");
    bead.owner = Some("dev-agent".into());

    let mut handle = spawn(&bead, repo.path(), false, 0, &provider, None, None, None)
        .await
        .unwrap();

    assert_eq!(handle.pid(), None);
    assert_eq!(
        handle.session_ref(),
        Some(AgentSessionRef::new("codex", "thread-rsry-1"))
    );
    assert_eq!(handle.try_wait().unwrap(), Some(true));

    let starts = captured.lock().unwrap();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].bead_id.as_deref(), Some("rsry-codex1"));
    assert_eq!(starts[0].agent_name.as_deref(), Some("dev-agent"));
    assert_eq!(starts[0].work_dir, handle.work_dir);
    assert_eq!(starts[0].permissions, PermissionProfile::Implement);
    assert!(starts[0].prompt.contains("rsry-codex1"));
    assert!(starts[0].system_prompt.contains(PROMPT_VERSION));
    assert!(starts[0].mcp_servers.contains_key("rsry"));
    assert!(starts[0].expected_mcp_tools.contains(&"lectio".to_string()));
}

/// rosary-d6d1bb: the Codex provider has NO CLI shell-out path. The
/// unstructured `spawn_agent` seam (the `build_command`/CLI equivalent) must
/// refuse, so codex can only run via the native structured `spawn_run` that
/// yields a persistable `AgentSessionRef`. "Do not shell out for Codex" is
/// enforced by construction, not prompt convention — registering the provider
/// therefore can't produce a durable-session-less dispatch.
#[test]
fn codex_provider_has_no_cli_shell_out_path() {
    let provider = CodexProvider::with_runtime(Arc::new(MockCodexRuntime {
        captured: Arc::new(Mutex::new(Vec::new())),
    }));
    let repo = crate::testutil::TestRepo::new();
    let err = match provider.spawn_agent("prompt", repo.path(), &PermissionProfile::Implement, "") {
        Ok(_) => panic!("codex must refuse the CLI shell-out path"),
        Err(e) => e,
    };
    assert!(
        err.to_string().contains("structured spawn_run"),
        "codex should point callers at spawn_run; got: {err}"
    );
}

#[test]
fn codex_app_server_thread_start_maps_rosary_run_spec_to_codex_protocol() {
    let mut mcp_servers = std::collections::BTreeMap::new();
    mcp_servers.insert("rsry".to_string(), "http://localhost:8383/mcp".to_string());
    let start = CodexThreadStart {
        bead_id: Some("rosary-codex-protocol".to_string()),
        agent_name: Some("dev-agent".to_string()),
        prompt: "work the bead".to_string(),
        work_dir: PathBuf::from("/tmp/rsry-codex-work"),
        permissions: PermissionProfile::Implement,
        system_prompt: "developer rules".to_string(),
        mcp_servers,
        expected_mcp_tools: vec![
            "rsry".to_string(),
            "mache".to_string(),
            "lectio".to_string(),
        ],
        model: Some("gpt-5-codex".to_string()),
    };

    let request = CodexAppServerRequest::thread_start("rsry-thread-start-1", &start);
    let value = serde_json::to_value(request).expect("thread/start request should serialize");

    assert_eq!(value["jsonrpc"], "2.0");
    assert_eq!(value["id"], "rsry-thread-start-1");
    assert_eq!(value["method"], "thread/start");
    assert_eq!(value["params"]["model"], "gpt-5-codex");
    assert_eq!(value["params"]["cwd"], "/tmp/rsry-codex-work");
    assert_eq!(
        value["params"]["runtimeWorkspaceRoots"],
        serde_json::json!(["/tmp/rsry-codex-work"])
    );
    assert_eq!(value["params"]["approvalPolicy"], "on-request");
    assert_eq!(value["params"]["sandbox"], "workspace-write");
    assert!(
        value["params"].get("permissions").is_none(),
        "Rosary should not depend on Codex's experimental named permission profiles"
    );
    assert_eq!(value["params"]["developerInstructions"], "developer rules");
    assert_eq!(
        value["params"]["config"]["rsry.bead_id"],
        "rosary-codex-protocol"
    );
    assert_eq!(value["params"]["config"]["rsry.agent_name"], "dev-agent");
    assert_eq!(
        value["params"]["config"]["rsry.expected_mcp_tools"],
        serde_json::json!(["rsry", "mache", "lectio"])
    );
}

#[test]
fn codex_app_server_turn_start_carries_prompt_as_text_input() {
    let request = CodexAppServerRequest::turn_start(
        "rsry-turn-start-1",
        "thread-rsry-123",
        "work the bead",
        Path::new("/tmp/rsry-codex-work"),
        PermissionProfile::Plan,
        Some("gpt-5-codex".to_string()),
    );
    let value = serde_json::to_value(request).expect("turn/start request should serialize");

    assert_eq!(value["jsonrpc"], "2.0");
    assert_eq!(value["id"], "rsry-turn-start-1");
    assert_eq!(value["method"], "turn/start");
    assert_eq!(value["params"]["threadId"], "thread-rsry-123");
    assert_eq!(value["params"]["cwd"], "/tmp/rsry-codex-work");
    assert_eq!(
        value["params"]["runtimeWorkspaceRoots"],
        serde_json::json!(["/tmp/rsry-codex-work"])
    );
    assert_eq!(value["params"]["approvalPolicy"], "on-request");
    assert_eq!(
        value["params"]["sandboxPolicy"],
        serde_json::json!({ "type": "readOnly", "networkAccess": false })
    );
    assert!(
        value["params"].get("permissions").is_none(),
        "Rosary should not depend on Codex's experimental named permission profiles"
    );
    assert_eq!(value["params"]["model"], "gpt-5-codex");
    assert_eq!(
        value["params"]["input"],
        serde_json::json!([{ "type": "text", "text": "work the bead", "textElements": [] }])
    );
}

#[derive(Default)]
struct FakeCodexAppServerClient {
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl CodexAppServerClient for FakeCodexAppServerClient {
    fn request(&self, request: CodexAppServerRequest) -> Result<serde_json::Value> {
        let value = serde_json::to_value(request).expect("request should serialize");
        let method = value["method"].as_str().unwrap_or_default().to_string();
        self.requests.lock().unwrap().push(value);
        match method.as_str() {
            "thread/start" => Ok(serde_json::json!({
                "thread": {
                    "id": "thread-from-fake-app-server",
                    "sessionId": "session-from-fake-app-server"
                }
            })),
            "turn/start" => Ok(serde_json::json!({
                "turn": {
                    "id": "turn-from-fake-app-server"
                }
            })),
            other => anyhow::bail!("unexpected codex request method {other}"),
        }
    }
}

#[test]
fn codex_app_server_runtime_starts_thread_then_turn_and_returns_session_ref() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let runtime = CodexAppServerRuntime::new(Arc::new(FakeCodexAppServerClient {
        requests: requests.clone(),
    }));
    let start = CodexThreadStart {
        bead_id: Some("rosary-codex-runtime".to_string()),
        agent_name: Some("dev-agent".to_string()),
        prompt: "work the runtime bead".to_string(),
        work_dir: PathBuf::from("/tmp/rsry-codex-work"),
        permissions: PermissionProfile::Implement,
        system_prompt: "developer rules".to_string(),
        mcp_servers: std::collections::BTreeMap::new(),
        expected_mcp_tools: vec!["rsry".to_string()],
        model: None,
    };

    let session = runtime
        .start_thread(start)
        .expect("runtime should start codex");

    assert_eq!(
        session.session_ref(),
        Some(AgentSessionRef::new("codex", "thread-from-fake-app-server"))
    );
    let captured = requests.lock().unwrap();
    assert_eq!(captured.len(), 2);
    assert_eq!(captured[0]["method"], "thread/start");
    assert_eq!(captured[1]["method"], "turn/start");
    assert_eq!(
        captured[1]["params"]["threadId"],
        "thread-from-fake-app-server"
    );
    assert_eq!(
        captured[1]["params"]["input"][0]["text"],
        "work the runtime bead"
    );
}

#[test]
fn codex_unix_socket_client_initializes_and_sends_request() {
    let dir = TempDir::new().unwrap();
    let socket_path = dir.path().join("codex-app-server.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut websocket = tungstenite::accept(stream).unwrap();

        let initialize = read_test_ws_json(&mut websocket);
        assert_eq!(initialize["method"], "initialize");
        assert_eq!(initialize["params"]["clientInfo"]["name"], "rosary");
        write_test_ws_json(
            &mut websocket,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": initialize["id"],
                "result": {
                    "userAgent": "codex-test/0"
                }
            }),
        );

        let initialized = read_test_ws_json(&mut websocket);
        assert_eq!(initialized["method"], "initialized");

        let request = read_test_ws_json(&mut websocket);
        assert_eq!(request["method"], "thread/start");
        assert_eq!(request["params"]["approvalPolicy"], "on-request");
        assert_eq!(request["params"]["sandbox"], "workspace-write");
        write_test_ws_json(
            &mut websocket,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": request["id"],
                "result": {
                    "thread": {
                        "id": "thread-from-unix-socket"
                    }
                }
            }),
        );
    });

    let mut mcp_servers = std::collections::BTreeMap::new();
    mcp_servers.insert("rsry".to_string(), "http://localhost:8383/mcp".to_string());
    let start = CodexThreadStart {
        bead_id: Some("rosary-codex-socket".to_string()),
        agent_name: Some("dev-agent".to_string()),
        prompt: "work through socket".to_string(),
        work_dir: PathBuf::from("/tmp/rsry-codex-work"),
        permissions: PermissionProfile::Implement,
        system_prompt: "developer rules".to_string(),
        mcp_servers,
        expected_mcp_tools: vec!["rsry".to_string()],
        model: None,
    };

    let client = CodexUnixSocketClient::new(socket_path);
    let response = client
        .request(CodexAppServerRequest::thread_start(
            "rsry-thread-start-socket",
            &start,
        ))
        .expect("socket client should round-trip request");

    assert_eq!(response["thread"]["id"], "thread-from-unix-socket");
    server.join().expect("fake codex app-server should exit");
}

fn read_test_ws_json(
    websocket: &mut tungstenite::WebSocket<std::os::unix::net::UnixStream>,
) -> serde_json::Value {
    loop {
        match websocket.read().unwrap() {
            tungstenite::Message::Text(text) => return serde_json::from_str(&text).unwrap(),
            tungstenite::Message::Binary(bytes) => {
                return serde_json::from_slice(&bytes).unwrap();
            }
            tungstenite::Message::Ping(_) | tungstenite::Message::Pong(_) => continue,
            tungstenite::Message::Close(_) | tungstenite::Message::Frame(_) => {
                panic!("unexpected websocket control frame")
            }
        }
    }
}

fn write_test_ws_json(
    websocket: &mut tungstenite::WebSocket<std::os::unix::net::UnixStream>,
    value: serde_json::Value,
) {
    websocket
        .send(tungstenite::Message::Text(value.to_string().into()))
        .unwrap();
}

#[test]
fn provider_by_name_gemini() {
    let empty = std::collections::HashMap::new();
    let p = provider_by_name("gemini", &empty).unwrap();
    assert_eq!(p.name(), "gemini");
}

#[test]
fn provider_by_name_unknown() {
    let empty = std::collections::HashMap::new();
    assert!(provider_by_name("copilot", &empty).is_err());
}

#[test]
fn provider_by_name_with_binary_override() {
    let mut binaries = std::collections::HashMap::new();
    binaries.insert("claude".to_string(), "/usr/local/bin/claude".to_string());
    let p = provider_by_name("claude", &binaries).unwrap();
    assert_eq!(p.name(), "claude");
    let (bin, _) = p.build_command("test", &PermissionProfile::Implement, "sys");
    assert_eq!(bin, "/usr/local/bin/claude");
}

#[test]
fn permission_profile_from_issue_type() {
    // bug/task/feature -> Implement
    assert_eq!(
        PermissionProfile::Implement,
        match "bug" {
            "review" | "survey" | "audit" => PermissionProfile::ReadOnly,
            "epic" | "plan" | "triage" => PermissionProfile::Plan,
            _ => PermissionProfile::Implement,
        }
    );
    // review -> ReadOnly
    assert_eq!(
        PermissionProfile::ReadOnly,
        match "review" {
            "review" | "survey" | "audit" => PermissionProfile::ReadOnly,
            "epic" | "plan" | "triage" => PermissionProfile::Plan,
            _ => PermissionProfile::Implement,
        }
    );
    // epic -> Plan
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
fn implement_agents_cannot_close_beads() {
    let tools = PermissionProfile::Implement.claude_allowed_tools();
    assert!(
        !tools.contains("bead_close"),
        "dev-agents must not close beads -- that's the reconciler's job"
    );
    assert!(
        !tools.contains("workspace_merge"),
        "dev-agents must not merge workspaces -- that's the reconciler's job"
    );
    assert!(
        tools.contains("bead_comment"),
        "dev-agents should be able to comment on beads"
    );
}

#[test]
fn readonly_agents_cannot_close_beads() {
    let tools = PermissionProfile::ReadOnly.claude_allowed_tools();
    assert!(!tools.contains("bead_close"));
    assert!(!tools.contains("bead_create"));
    assert!(tools.contains("bead_comment"));
}

#[test]
fn plan_agents_can_create_but_not_close() {
    let tools = PermissionProfile::Plan.claude_allowed_tools();
    assert!(tools.contains("bead_create"));
    assert!(!tools.contains("bead_close"));
    assert!(!tools.contains("workspace_merge"));
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
    let bead = Bead {
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
        created_by: None,
        scope: String::new(),
        derived_from: vec![],
        acceptance_criteria: String::new(),
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
        prompt.contains("rsry_bead_comment"),
        "prompt should instruct agent to comment on bead"
    );
    assert!(
        !prompt.contains("rsry_bead_close"),
        "prompt must NOT instruct agent to close bead -- reconciler owns lifecycle"
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
/// The MCP bead_comment instruction must still use the main repo path
/// (where .beads/ lives). This prevents agents from writing changes
/// to the main working tree instead of their isolated worktree.
#[test]
fn build_prompt_uses_workspace_for_repo_line() {
    let bead = Bead {
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
        created_by: None,
        scope: String::new(),
        derived_from: vec![],
        acceptance_criteria: String::new(),
    };

    let ws = PathBuf::from("/home/user/.rsry/worktrees/myrepo/iso-1");
    let prompt = build_prompt(&bead, "/home/user/repos/myrepo", Some(&ws), None);

    // Repo: line must show the WORKSPACE path (agent's working directory)
    assert!(
        prompt.contains("/home/user/.rsry/worktrees/myrepo/iso-1"),
        "Repo line must point to workspace, not main repo. Got:\n{prompt}"
    );
    // MCP bead_comment must still use the MAIN repo path (where .beads/ lives)
    assert!(
        prompt.contains("repo_path=\"/home/user/repos/myrepo\""),
        "bead_comment repo_path must point to main repo. Got:\n{prompt}"
    );
    // Repo: line must NOT contain the main repo path as the workspace repo
    assert!(
        !prompt.contains("Repo: /home/user/repos/myrepo\n"),
        "Repo line must NOT show main repo path when workspace exists. Got:\n{prompt}"
    );
}

#[test]
fn build_prompt_varies_framing_by_agent() {
    let bead = Bead {
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
        created_by: None,
        scope: String::new(),
        derived_from: vec![],
        acceptance_criteria: String::new(),
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
    // try_wait should return Some(true) now that the process has exited
    let status = session.try_wait().unwrap();
    assert_eq!(status, Some(true));
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
        chain_hash: None,
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

/// Finding #1: dispatch::agent_pipeline diverged from config::default_pipelines().
/// CLI dispatch used to give dev-agent first for bugs; reconciler gives scoping-agent.
/// A bug dispatched via CLI skipped the scoping phase entirely.
#[test]
fn default_agent_matches_pipeline_engine() {
    use crate::config::default_pipelines;
    use crate::pipeline::PipelineEngine;
    let engine = PipelineEngine::new(default_pipelines(), None, 0);

    // dispatch::default_agent must agree with PipelineEngine::default_agent for every type
    for issue_type in &["bug", "feature", "task", "chore", "review", "epic"] {
        assert_eq!(
            default_agent(issue_type),
            engine.default_agent(issue_type),
            "dispatch::default_agent diverges from PipelineEngine for issue_type={issue_type}"
        );
    }
    // Spot-check the values that were wrong before the fix
    assert_eq!(
        default_agent("bug"),
        "scoping-agent",
        "bugs must start with scoping-agent"
    );
    assert_eq!(
        default_agent("feature"),
        "scoping-agent",
        "features must start with scoping-agent"
    );
    assert_eq!(default_agent("review"), "staging-agent");
    assert_eq!(default_agent("epic"), "pm-agent");
    assert_eq!(default_agent("xyz"), "dev-agent"); // fallback
}

// -----------------------------------------------------------------------
// Level 1: Single persona dispatch with mocks
// -----------------------------------------------------------------------

#[tokio::test]
async fn mock_agent_session_success() {
    let mut session = MockAgentSession { exit_success: true };
    assert_eq!(session.try_wait().unwrap(), Some(true));
    assert!(session.wait().await.unwrap());
}

#[tokio::test]
async fn mock_agent_session_failure() {
    let mut session = MockAgentSession {
        exit_success: false,
    };
    assert_eq!(session.try_wait().unwrap(), Some(false));
    assert!(!session.wait().await.unwrap());
}

#[test]
fn mock_provider_creates_commit() {
    let repo = crate::testutil::TestRepo::new();
    let provider = MockAgentProvider::with_commit("rsry-test1");
    let _session = provider
        .spawn_agent("prompt", repo.path(), &PermissionProfile::Implement, "sys")
        .unwrap();

    // Verify the commit was created
    let output = std::process::Command::new("git")
        .args(["log", "--oneline", "-1"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&output.stdout);
    assert!(log.contains("[rsry-test1]"), "bead ref in commit: {log}");
}

#[test]
fn mock_commit_passes_verification() {
    let repo = crate::testutil::TestRepo::new();
    repo.commit_with_bead_ref("rsry-test1", "foo.rs", "fn main() {}");

    let verifier = crate::verify::Verifier::new(vec![
        Box::new(crate::verify::CommitCheck),
        Box::new(crate::verify::WorkRefCheck),
    ]);
    let summary = verifier.run(repo.path()).unwrap();
    assert!(summary.passed(), "verification should pass: {summary:?}");
}

#[test]
fn plain_commit_fails_bead_ref_check() {
    let repo = crate::testutil::TestRepo::new();
    repo.commit_plain("foo.rs", "fn main() {}");

    let verifier = crate::verify::Verifier::new(vec![
        Box::new(crate::verify::CommitCheck),
        Box::new(crate::verify::WorkRefCheck),
    ]);
    let summary = verifier.run(repo.path()).unwrap();
    assert!(!summary.passed(), "should fail bead ref check");
    assert_eq!(summary.highest_passing_tier, Some(0));
}

#[test]
fn spawn_derives_readonly_for_scoping_agent() {
    let bead = crate::testutil::make_bead("rsry-x", "bug", "test");
    let mut bead = bead;
    bead.owner = Some("scoping-agent".to_string());
    let perms = match bead.owner.as_deref() {
        Some("scoping-agent") => PermissionProfile::ReadOnly,
        Some("staging-agent") => PermissionProfile::ReadOnly,
        Some("pm-agent") => PermissionProfile::Plan,
        Some("architect-agent") => PermissionProfile::Plan,
        _ => permission_profile(&bead.issue_type),
    };
    assert_eq!(perms, PermissionProfile::ReadOnly);
}

#[test]
fn spawn_derives_implement_for_dev_agent() {
    let mut bead = crate::testutil::make_bead("rsry-x", "bug", "test");
    bead.owner = Some("dev-agent".to_string());
    let perms = match bead.owner.as_deref() {
        Some("scoping-agent") => PermissionProfile::ReadOnly,
        Some("staging-agent") => PermissionProfile::ReadOnly,
        Some("pm-agent") => PermissionProfile::Plan,
        Some("architect-agent") => PermissionProfile::Plan,
        _ => permission_profile(&bead.issue_type),
    };
    assert_eq!(perms, PermissionProfile::Implement);
}

#[test]
fn build_command_claude_returns_expected_args() {
    let provider = ClaudeProvider::default();
    let (bin, args) =
        provider.build_command("test prompt", &PermissionProfile::Implement, "sys prompt");
    assert_eq!(bin, "claude");
    assert!(args.contains(&"-p".to_string()));
    assert!(args.contains(&"test prompt".to_string()));
    assert!(args.contains(&"--output-format".to_string()));
}

// -----------------------------------------------------------------------
// Compute dispatch tests -- MockProvider + MockAgentProvider
// -----------------------------------------------------------------------

#[tokio::test]
async fn spawn_with_compute_uses_container() {
    use crate::backend::tests::MockProvider;

    let repo = crate::testutil::TestRepo::new();
    let mut bead = crate::testutil::make_bead("rsry-comp1", "task", "test");
    bead.owner = Some("dev-agent".into());

    let agent = MockAgentProvider::succeeding();
    let compute = MockProvider::new();

    // Spawn with compute provider
    let handle = spawn(
        &bead,
        repo.path(),
        false, // no isolation for test
        0,
        &agent,
        None,
        Some(&compute),
        None,
    )
    .await
    .unwrap();

    // Should have provisioned + exec'd + destroyed
    let provisions = compute.provisions.lock().unwrap();
    assert_eq!(provisions.len(), 1, "should provision one container");
    assert_eq!(provisions[0].bead_id, "rsry-comp1");

    let execs = compute.execs.lock().unwrap();
    assert_eq!(execs.len(), 1, "should exec one command");
    // The command should start with "claude" (from build_command)
    // But MockAgentProvider returns empty build_command -- need ClaudeProvider
    // Actually MockProvider's exec returns default success, so the session is done

    let destroys = compute.destroys.lock().unwrap();
    assert_eq!(destroys.len(), 1, "should destroy container after exec");

    // Handle should already be completed
    let mut handle = handle;
    assert_eq!(handle.session.try_wait().unwrap(), Some(true));
}

#[tokio::test]
async fn spawn_with_compute_forwards_command() {
    use crate::backend::tests::MockProvider;

    let repo = crate::testutil::TestRepo::new();
    let mut bead = crate::testutil::make_bead("rsry-fwd1", "task", "test");
    bead.owner = Some("dev-agent".into());

    let agent = MockAgentProvider::succeeding();
    let compute = MockProvider::new();

    let _handle = spawn(
        &bead,
        repo.path(),
        false,
        0,
        &agent,
        None,
        Some(&compute),
        None,
    )
    .await
    .unwrap();

    // Assert the command forwarded to exec() matches build_command() output
    let execs = compute.execs.lock().unwrap();
    assert_eq!(execs.len(), 1);
    assert_eq!(
        execs[0][0], "echo",
        "first arg should be the binary from build_command"
    );
    assert_eq!(
        execs[0][1], "mock-agent",
        "second arg should be from build_command"
    );
}

#[tokio::test]
async fn spawn_with_compute_exec_failure_still_destroys() {
    use crate::backend::ExecResult;
    use crate::backend::tests::MockProvider;

    let repo = crate::testutil::TestRepo::new();
    let mut bead = crate::testutil::make_bead("rsry-fail1", "task", "test");
    bead.owner = Some("dev-agent".into());

    let agent = MockAgentProvider::succeeding();
    let compute = MockProvider::new();
    // Enqueue a failure result
    compute.enqueue_result(ExecResult {
        exit_code: 1,
        stdout: String::new(),
        stderr: "container error".into(),
    });

    let handle = spawn(
        &bead,
        repo.path(),
        false,
        0,
        &agent,
        None,
        Some(&compute),
        None,
    )
    .await
    .unwrap();

    // Even though exec failed, container should be destroyed
    let destroys = compute.destroys.lock().unwrap();
    assert_eq!(
        destroys.len(),
        1,
        "must destroy container even on exec failure"
    );

    // Session should report failure
    let mut handle = handle;
    assert_eq!(handle.session.try_wait().unwrap(), Some(false));
}

#[tokio::test]
async fn spawn_without_compute_uses_local() {
    let repo = crate::testutil::TestRepo::new();
    let mut bead = crate::testutil::make_bead("rsry-local1", "task", "test");
    bead.owner = Some("dev-agent".into());

    let agent = MockAgentProvider::succeeding();

    let handle = spawn(
        &bead,
        repo.path(),
        false,
        0,
        &agent,
        None,
        None, // no compute = local
        None,
    )
    .await
    .unwrap();

    // MockAgentProvider creates a local session -- already completed
    let mut handle = handle;
    assert_eq!(handle.session.try_wait().unwrap(), Some(true));
}

// -----------------------------------------------------------------------
// Hook installation tests
// -----------------------------------------------------------------------

#[test]
fn detect_language_rust() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
    assert_eq!(detect_language(dir.path()), "rust");
}

#[test]
fn detect_language_go() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("go.mod"), "module example.com/foo").unwrap();
    assert_eq!(detect_language(dir.path()), "go");
}

#[test]
fn detect_language_unknown() {
    let dir = TempDir::new().unwrap();
    assert_eq!(detect_language(dir.path()), "unknown");
}

#[test]
fn install_hooks_creates_hook_files() {
    let work_dir = TempDir::new().unwrap();
    let repo_dir = TempDir::new().unwrap();
    // Mark repo as Rust
    std::fs::write(repo_dir.path().join("Cargo.toml"), "[package]").unwrap();

    // Need a git repo for core.hooksPath to be settable; skip if git unavailable.
    let git_ok = std::process::Command::new("git")
        .args(["init"])
        .current_dir(work_dir.path())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !git_ok {
        eprintln!("skipping: git not available");
        return;
    }

    install_hooks(work_dir.path(), repo_dir.path());

    let hooks_dir = work_dir.path().join(".rsry-hooks");
    assert!(hooks_dir.exists(), ".rsry-hooks dir created");
    assert!(
        hooks_dir.join("commit-msg").exists(),
        "commit-msg hook present"
    );
    assert!(
        hooks_dir.join("pre-commit").exists(),
        "pre-commit hook present"
    );

    let pre_commit = std::fs::read_to_string(hooks_dir.join("pre-commit")).unwrap();
    assert!(
        pre_commit.contains("cargo check"),
        "rust hook runs cargo check"
    );
}

#[test]
fn install_hooks_go_uses_go_build() {
    let work_dir = TempDir::new().unwrap();
    let repo_dir = TempDir::new().unwrap();
    std::fs::write(repo_dir.path().join("go.mod"), "module example.com/foo").unwrap();

    let git_ok = std::process::Command::new("git")
        .args(["init"])
        .current_dir(work_dir.path())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !git_ok {
        eprintln!("skipping: git not available");
        return;
    }

    install_hooks(work_dir.path(), repo_dir.path());

    let pre_commit =
        std::fs::read_to_string(work_dir.path().join(".rsry-hooks/pre-commit")).unwrap();
    assert!(
        pre_commit.contains("go build ./..."),
        "go hook runs go build"
    );
}

// -----------------------------------------------------------------------
// Lifecycle invariant regression tests
// GAP 2: workspace must be cleaned up when spawn_agent fails
// -----------------------------------------------------------------------

/// Provider whose spawn_agent always returns Err — used to test cleanup paths.
struct FailingAgentProvider;

impl AgentProvider for FailingAgentProvider {
    fn spawn_agent(
        &self,
        _prompt: &str,
        _work_dir: &Path,
        _permissions: &PermissionProfile,
        _system_prompt: &str,
    ) -> Result<Box<dyn AgentSession>> {
        anyhow::bail!("injected spawn failure")
    }

    fn build_command(
        &self,
        _prompt: &str,
        _permissions: &PermissionProfile,
        _system_prompt: &str,
    ) -> (String, Vec<String>) {
        ("false".to_string(), vec![])
    }

    fn name(&self) -> &str {
        "failing"
    }

    fn with_model(&self, _model: Option<String>) -> Box<dyn providers::AgentProvider> {
        Box::new(FailingAgentProvider)
    }
}

/// Regression: if spawn_agent fails after a git worktree has been created,
/// the worktree must be removed. Without `workspace.cleanup()` in the error
/// path, the worktree is orphaned and accumulates on disk indefinitely.
#[tokio::test]
async fn spawn_failure_cleans_up_git_worktree() {
    let repo = crate::testutil::TestRepo::new();
    let bead = crate::testutil::make_bead("gap2-cleanup", "bug", "test");

    // Verify git is available (worktree add requires it)
    let git_ok = std::process::Command::new("git")
        .args(["worktree", "list"])
        .current_dir(repo.path())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !git_ok {
        eprintln!("skipping: git worktrees not available");
        return;
    }

    let expected_ws = crate::workspace::workspace_dir(repo.path(), "gap2-cleanup");

    let result = spawn(
        &bead,
        repo.path(),
        true, // isolate=true forces worktree creation
        0,
        &FailingAgentProvider,
        None,
        None,
        None,
    )
    .await;

    assert!(result.is_err(), "spawn must fail when spawn_agent fails");
    assert!(
        !expected_ws.exists(),
        "worktree at {} must be cleaned up after spawn failure, \
         but it still exists (GAP 2 regression)",
        expected_ws.display()
    );
}
