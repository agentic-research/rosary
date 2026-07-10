use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// Repo entries — accepts `[[repo]]` in TOML (singular).
    #[serde(alias = "repos", default)]
    pub repo: Vec<RepoConfig>,
    #[serde(default)]
    pub linear: Option<LinearConfig>,
    /// GitHub integration for PR creation.
    #[serde(default)]
    pub github: Option<GitHubConfig>,
    /// Compute provider configuration.
    #[serde(default)]
    pub compute: Option<ComputeConfig>,
    /// HTTP server + tunnel configuration.
    #[serde(default)]
    pub http: Option<HttpConfig>,
    /// Backend storage for orchestrator state (cross-repo).
    #[serde(default)]
    pub backend: Option<BackendConfig>,
    /// Dispatch pipeline behavior.
    #[serde(default)]
    pub dispatch: Option<DispatchConfig>,
    /// Directory containing git hook scripts (default: ~/.rsry/hooks/).
    /// These are installed into repos on `rsry enable` and always come from
    /// this central location — not per-branch, not per-repo.
    #[serde(default)]
    pub hooks_dir: Option<PathBuf>,
    /// Pipeline definitions: issue_type → agent sequence.
    /// Overrides the built-in defaults (bug → [dev, staging], etc.).
    /// Example: `[pipelines]\nbug = ["dev-agent", "staging-agent"]`
    #[serde(default = "default_pipelines")]
    pub pipelines: HashMap<String, Vec<String>>,
    /// Bounded content-addressed pipeline context (warm-resume, rosary-dd5828).
    #[serde(default)]
    pub context: ContextConfig,
    /// Maximum number of pipeline stages to execute per bead.
    /// 0 = unlimited (default). 1 = single-agent only.
    /// The hosted service sets this based on the customer's plan.
    #[serde(default)]
    pub max_pipeline_depth: usize,
    /// Orchestration mode and behavior.
    /// Controls whether the reconciler uses flat dispatch or hierarchical
    /// feature orchestrators with synthesis, fan-out, and plan gates.
    #[serde(default)]
    pub orchestration: Option<OrchestrationConfig>,
    /// Pipeline plugins — external tools that hook into verify/review/triage stages.
    /// Accepts `[[plugins]]` (plural) or `[[plugin]]` (singular) in TOML.
    #[serde(alias = "plugin", default)]
    pub plugins: Vec<PluginConfig>,
    /// APAS L2 attestation config — Ed25519 signing of handoff envelopes.
    /// When absent, handoffs are written but not signed.
    #[serde(default)]
    pub attestation: Option<AttestationConfig>,
}

/// APAS L2 attestation config (DSSE + in-toto).
///
/// ```toml
/// [attestation]
/// signing_key_path = "~/.rsry/keys/orchestrator.key"
/// ```
///
/// `signing_key_path` points to a 32-byte raw Ed25519 seed file.
/// When set, every handoff written by the orchestrator gets a sibling
/// `.rsry-handoff-N.dsse.json` envelope signed with this key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttestationConfig {
    /// Path to a raw 32-byte Ed25519 signing key file.
    /// Tilde (`~`) is expanded each time the key is read for signing,
    /// not at config-load time.
    pub signing_key_path: Option<PathBuf>,
}

/// Role of a plugin in the rosary pipeline.
///
/// `kind` defaults to `Hook` (backward-compatible) when absent from TOML.
/// Non-hook kinds are parsed and stored but not yet routed — they will be
/// wired in follow-on beads (MCP client, dispatch backend, state-sink).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    /// Lifecycle hook (verify / review / triage / close). Default.
    #[default]
    Hook,
    /// Context/tool provider — rosary connects as an outbound MCP client.
    Mcp,
    /// Execution backend — dispatches agents (sandboxed runners, chain-YAML, etc.).
    Dispatch,
    /// Outbound state mirror — issue trackers, dashboards, webhooks.
    StateSink,
}

/// A pipeline plugin: an external process or HTTP endpoint that participates
/// in the pipeline as a hook, MCP tool provider, dispatch backend, or state sink.
///
/// ```toml
/// # Lifecycle hook (default — no `kind` needed)
/// [[plugins]]
/// name = "review-tui"
/// hook = "pipeline.review"
/// command = ["review-tui", "--rsry-hook"]
///
/// # MCP context provider
/// [[plugins]]
/// name = "mache"
/// kind = "mcp"
/// url = "http://localhost:8484"
///
/// # Execution backend
/// [[plugins]]
/// name = "chain-runner"
/// kind = "dispatch"
/// command = ["claude-guard", "run"]
/// ```
///
/// Hook points (only used when `kind = "hook"`):
///   `pipeline.verify`  — appended to the verify tier chain (runs after built-in tiers)
///   `pipeline.review`  — replaces/extends ReviewCheck during the review phase
///   `pipeline.triage`  — called during reconciler triage; can skip a bead
///   `pipeline.close`   — called when a bead transitions to done
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginConfig {
    /// Display name (used in logs and verify summary).
    pub name: String,
    /// Plugin role. Defaults to `Hook` when absent (backward-compatible).
    #[serde(default)]
    pub kind: PluginKind,
    /// Hook point (only meaningful when `kind = "hook"`). See struct docs.
    #[serde(default)]
    pub hook: String,
    /// Subprocess command (mutually exclusive with `url`).
    /// First element is the executable, remaining are arguments.
    /// Receives JSON context on stdin; must write JSON verdict to stdout.
    #[serde(default)]
    pub command: Vec<String>,
    /// HTTP endpoint URL (mutually exclusive with `command`).
    /// Receives JSON context via POST body; must return JSON verdict.
    pub url: Option<String>,
}

impl PluginConfig {
    /// Returns true if this plugin is a lifecycle hook (the only kind that
    /// participates in verify/triage/close today).
    pub fn is_hook(&self) -> bool {
        self.kind == PluginKind::Hook
    }
}

/// Compute provider selection + backend-specific settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputeConfig {
    /// Provider name: "local" (default), "sprites".
    #[serde(default = "default_compute_backend")]
    pub backend: String,
    /// Sprites-specific settings (only read when backend = "sprites").
    pub sprites: Option<SpritesConfig>,
}

fn default_compute_backend() -> String {
    "local".to_string()
}

/// Configuration for the sprites.dev compute provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpritesConfig {
    /// Env var name holding the API token (default: "SPRITES_TOKEN").
    #[serde(default = "default_sprites_token_env")]
    pub token_env: String,
    /// Base URL override (for testing/self-hosted).
    pub base_url: Option<String>,
    /// Default CPU cores.
    pub cpu: Option<u32>,
    /// Default memory in MB.
    pub memory_mb: Option<u32>,
    /// Network egress allowlist (domains).
    #[serde(default)]
    pub network_allowlist: Vec<String>,
    /// Create checkpoint on agent completion.
    #[serde(default)]
    pub checkpoint_on_complete: bool,
    /// Fall back to local execution if sprite provisioning fails.
    #[serde(default = "default_true")]
    pub fallback_to_local: bool,
}

fn default_sprites_token_env() -> String {
    "SPRITES_TOKEN".to_string()
}

fn default_true() -> bool {
    true
}

/// User approval state for agent dispatch on a repo.
///
/// Inspired by Warp's `OrchestrationConfigStatus`. The reconciler only
/// auto-launches agents when this is `Approved`; otherwise beads are
/// queued but held. Targeted dispatch (`--bead`) overrides the gate.
///
/// The check itself is opt-in via `[dispatch] require_approval = true`.
/// When that flag is unset (default), `approval` is ignored — preserving
/// existing behavior for users who don't want a confirmation step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DispatchApproval {
    /// No decision yet. Beads are held until the user runs `rsry approve <repo>`.
    None,
    /// User has approved dispatch. Default for backward compatibility and
    /// for `self_managed = true` repos (rosary dogfooding itself).
    #[default]
    Approved,
    /// User has rejected dispatch. Reconciler skips beads from this repo.
    Rejected,
}

impl DispatchApproval {
    /// True when the gate (if active) admits this repo's beads.
    pub fn admits(self) -> bool {
        matches!(self, Self::Approved)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoConfig {
    /// Display name for the repo
    pub name: String,
    /// Path to the repo root (absolute or ~ prefixed)
    pub path: PathBuf,
    /// Language hint (rust, go, python, etc.). Auto-detected if absent.
    pub lang: Option<String>,
    /// Whether this repo IS rosary itself (dogfooding flag).
    #[serde(default, rename = "self")]
    pub self_managed: bool,
    /// User approval state for agent dispatch on this repo. Only consulted
    /// when `[dispatch] require_approval = true`. Defaults to `Approved`
    /// so existing configs continue to dispatch without a migration step.
    #[serde(default)]
    pub approval: DispatchApproval,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinearConfig {
    /// Linear team key (e.g., "AGE")
    pub team: String,
    /// Linear API key (alternative to LINEAR_API_KEY env var)
    pub api_key: Option<String>,
    /// Linear project name for cross-repo tracking
    pub project: Option<String>,
    /// Optional bead status → Linear state name overrides.
    /// Keys are bead statuses (open, dispatched, verifying, done, blocked).
    /// Values are the Linear state names in your team's workflow.
    /// Example: { dispatched = "Working", verifying = "Peer Review" }
    #[serde(default)]
    pub states: HashMap<String, String>,
    /// Phase-to-Linear-project mapping (e.g., "1" → "Phase 1: Foundation")
    /// Beads with "phase:N" or "Phase N" in their description get assigned
    /// to the corresponding Linear project.
    #[serde(default)]
    pub phases: HashMap<String, String>,
    /// Webhook signing secret (alternative to LINEAR_WEBHOOK_SECRET env var)
    #[serde(default)]
    pub webhook_secret: Option<String>,
}

/// GitHub integration for PR creation from dispatch pipeline.
///
/// Supports two auth modes:
/// 1. **GitHub App** (preferred): `app_id` + `installation_id` + `private_key_path`
///    PRs/commits appear as `rosary-stringer[bot]`.
/// 2. **PAT fallback**: `token` (fine-grained personal access token).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubConfig {
    /// Personal access token (fine-grained PAT). Fallback when App is not configured.
    pub token: Option<String>,
    /// Default owner for PR creation (e.g., "agentic-research").
    pub owner: Option<String>,
    /// Default base branch for PRs.
    #[serde(default = "default_base_branch")]
    pub base: String,
    /// Auto-create PR when pipeline completes.
    #[serde(default)]
    pub auto_pr: bool,
    /// Branch prefix for thread feature branches (default: "rosary").
    /// Dev agents PR `fix/<bead>` into `<prefix>/<thread>`,
    /// feature-agent PRs `<prefix>/<thread>` into main.
    #[serde(default = "default_agent_branch_prefix")]
    pub agent_branch_prefix: String,
    /// GitHub App ID (from app registration page).
    pub app_id: Option<u64>,
    /// GitHub App installation ID (from org/repo installation).
    pub installation_id: Option<u64>,
    /// OAuth client ID (informational, not used for auth flow).
    pub client_id: Option<String>,
    /// Path to the PEM private key file for JWT signing.
    pub private_key_path: Option<String>,
    /// Webhook secret for verifying `X-Hub-Signature-256` on incoming GitHub events.
    #[serde(default)]
    pub webhook_secret: Option<String>,
}

fn default_base_branch() -> String {
    "main".to_string()
}

fn default_agent_branch_prefix() -> String {
    "rosary".to_string()
}

/// Dispatch pipeline behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchConfig {
    /// Default agent provider: "claude", "gemini", "acp", "codex".
    #[serde(default = "default_dispatch_provider")]
    pub provider: String,
    /// Provider for adversarial review phases.
    pub adversarial_provider: Option<String>,
    /// Max concurrent dispatches.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,
    /// Binary paths per provider. Overrides default PATH lookup.
    /// Example: `[dispatch.binaries]\nclaude = "/Users/me/.local/bin/claude"`
    #[serde(default)]
    pub binaries: HashMap<String, String>,
    /// Anthropic API key / OAuth token for dispatched agents.
    ///
    /// Equivalent to `CLAUDE_CODE_OAUTH_TOKEN` or `ANTHROPIC_API_KEY` env vars,
    /// but stored in `~/.rsry/config.toml` for hosts (e.g. wasteland rigs) where
    /// per-repo `.envrc` files are not available.
    ///
    /// Priority: env vars → per-repo `.envrc` → this field.
    pub anthropic_api_key: Option<String>,
    /// Per-agent model overrides. Maps agent name → model string.
    ///
    /// Light prompts (scoping, triage) should use haiku; heavy prompts
    /// (dev, architect) should use sonnet or opus.
    ///
    /// Example:
    /// ```toml
    /// [dispatch.pipeline_models]
    /// "scoping-agent" = "claude-haiku-4-5-20251001"
    /// "dev-agent" = "claude-sonnet-4-6"
    /// ```
    #[serde(default)]
    pub pipeline_models: HashMap<String, String>,
    /// When true, dispatch is gated by `RepoConfig.approval` — only beads from
    /// repos with `approval = "approved"` are auto-launched. `--bead` overrides.
    /// Default false: backward-compatible, no gate.
    ///
    /// Toggle with `rsry approve <repo>` / `rsry reject <repo>`.
    #[serde(default)]
    pub require_approval: bool,
    /// MCP servers exposed to DISPATCHED agents (rosary-563b3f) so their
    /// granted `mcp__rsry__*` / `mcp__mache__*` tools actually connect during a
    /// run. Map of server name → HTTP MCP URL. Defaults to the local rsry +
    /// mache HTTP services; override here (e.g. point at a cloister gateway).
    /// Empty disables injecting `--mcp-config`.
    #[serde(default = "default_agent_mcp")]
    pub agent_mcp: BTreeMap<String, String>,
    /// Skills every dispatch must be able to resolve by name under
    /// `{agents_dir}/skills/{name}/SKILL.md` before spawning (rosary-cf52cf).
    /// A missing skill fails the dispatch deterministically instead of the agent
    /// discovering it can't find `/pr-review-kit` mid-run. Default: none.
    #[serde(default)]
    pub required_skills: Vec<String>,
}

fn default_dispatch_provider() -> String {
    "claude".to_string()
}

fn default_max_concurrent() -> usize {
    3
}

/// Default MCP servers for dispatched agents: the local rsry + mache HTTP
/// services (rosary-563b3f). Swap for a cloister-gateway URL via config.
pub(crate) fn default_agent_mcp() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("rsry".to_string(), "http://localhost:8383/mcp".to_string()),
        ("mache".to_string(), "http://localhost:7532/mcp".to_string()),
    ])
}

/// Orchestration mode and behavior.
///
/// Controls how the reconciler manages per-bead lifecycles:
/// - `flat` (default): current behavior — reconciler directly dispatches agents
/// - `hierarchical`: reconciler spawns FeatureOrchestrators that coordinate workers
///
/// The hierarchical mode enables synthesis between phases, parallel research
/// fan-out, plan-mode approval gates, and mid-flight communication.
///
/// Designed as a generic pipeline — orchestrators can nest to arbitrary depth,
/// like ComfyUI/n8n workflow nodes. Each node in the pipeline can itself
/// contain a sub-pipeline, enabling patterns like:
///
/// ```text
/// grand-orchestrator
///   └─ feature-orchestrator (bead X)
///        ├─ research-pipeline (parallel fan-out)
///        │    ├─ impl-researcher
///        │    ├─ test-researcher
///        │    └─ deps-researcher
///        ├─ synthesis (reads research, builds prompt)
///        └─ impl-pipeline (sequential)
///             ├─ dev-agent
///             ├─ staging-agent
///             └─ prod-agent
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestrationConfig {
    /// "flat" (default) or "hierarchical".
    #[serde(default = "default_orchestration_mode")]
    pub mode: String,
    /// Enable synthesis LLM call between phases.
    /// The orchestrator reads worker output and builds contextualized prompts
    /// instead of forwarding raw handoff JSON.
    #[serde(default = "default_true")]
    pub synthesis: bool,
    /// Enable parallel research fan-out for scoping phase.
    /// Spawns multiple read-only workers in parallel instead of one scoping-agent.
    #[serde(default)]
    pub fan_out: bool,
    /// Enable plan-mode approval gate before implementation.
    /// Workers propose plans; orchestrator validates scope/risk before approving.
    #[serde(default)]
    pub plan_gate: bool,
    /// Max parallel research workers during fan-out.
    #[serde(default = "default_max_research_workers")]
    pub max_research_workers: usize,
    /// Pass transcript excerpts (fork-style) instead of just handoff JSON.
    /// Gives the next agent concrete observations from the previous agent's work.
    #[serde(default = "default_true")]
    pub fork_context: bool,
    /// Maximum nesting depth for sub-orchestrators.
    /// 0 = no nesting (feature orchestrator only). N = N levels of sub-pipelines.
    #[serde(default)]
    pub max_nesting_depth: usize,
}

fn default_orchestration_mode() -> String {
    "flat".to_string()
}

fn default_max_research_workers() -> usize {
    3
}

impl Default for OrchestrationConfig {
    fn default() -> Self {
        Self {
            mode: default_orchestration_mode(),
            synthesis: true,
            fan_out: false,
            plan_gate: false,
            max_research_workers: default_max_research_workers(),
            fork_context: true,
            max_nesting_depth: 0,
        }
    }
}

/// Warm-resume context-cache mode (rosary-a9f5dc). `off` = always re-derive
/// (Phase A behavior, the default + escape hatch); `shadow` = compute the warm
/// render, assert it equals cold, but serve cold; `on` = serve warm (deferred to
/// B4 — treated as `shadow` until certified).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheMode {
    Off,
    Shadow,
    On,
}

fn default_context_cache() -> CacheMode {
    CacheMode::Off
}

/// Bounded content-addressed pipeline context (`[context]`). Governs how the
/// handoff chain is pruned into a budgeted envelope before prompt-building.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextConfig {
    /// Pruning policy: `tiers` (default, current phase hot) or `recency`.
    #[serde(default = "default_context_policy")]
    pub policy: String,
    /// Hard ceiling (bytes) the rendered context must stay under.
    #[serde(default = "default_context_budget")]
    pub budget: usize,
    /// Max inline refs before older ones roll up into a single blob.
    #[serde(default = "default_context_max_refs")]
    pub max_refs: usize,
    /// Warm-resume cache mode. Default `off` — no cache until deliberately enabled.
    #[serde(default = "default_context_cache")]
    pub cache: CacheMode,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            policy: default_context_policy(),
            budget: default_context_budget(),
            max_refs: default_context_max_refs(),
            cache: default_context_cache(),
        }
    }
}

fn default_context_policy() -> String {
    "tiers".to_string()
}
fn default_context_budget() -> usize {
    8000
}
fn default_context_max_refs() -> usize {
    8
}

/// Built-in pipeline definitions: issue_type → ordered agent sequence.
/// These are the defaults when no `[pipelines]` section is in config.
pub fn default_pipelines() -> HashMap<String, Vec<String>> {
    let mut m = HashMap::new();
    m.insert(
        "bug".into(),
        vec![
            "scoping-agent".into(),
            "dev-agent".into(),
            "staging-agent".into(),
        ],
    );
    m.insert(
        "feature".into(),
        vec![
            "scoping-agent".into(),
            "dev-agent".into(),
            "staging-agent".into(),
            "prod-agent".into(),
        ],
    );
    m.insert("task".into(), vec!["dev-agent".into()]);
    m.insert("chore".into(), vec!["dev-agent".into()]);
    m.insert("review".into(), vec!["staging-agent".into()]);
    m.insert("design".into(), vec!["architect-agent".into()]);
    m.insert("research".into(), vec!["architect-agent".into()]);
    m.insert("epic".into(), vec!["pm-agent".into()]);
    m
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpConfig {
    /// Port the HTTP server listens on.
    #[serde(default = "default_http_port")]
    pub port: u16,
    /// Optional tunnel configuration for exposing the server publicly.
    #[serde(default)]
    pub tunnel: Option<TunnelConfig>,
}

fn default_http_port() -> u16 {
    8383
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelConfig {
    /// Tunnel provider name (e.g., "cloudflare").
    #[serde(default = "default_tunnel_provider")]
    pub provider: String,
    /// Custom domain — omit for random *.trycloudflare.com.
    #[serde(default)]
    pub domain: Option<String>,
    /// Cloudflare account ID.
    #[serde(default)]
    pub account_id: Option<String>,
    /// Cloudflare zone ID.
    #[serde(default)]
    pub zone_id: Option<String>,
    /// Env var name holding the API token for the tunnel provider.
    #[serde(default)]
    pub token_env: Option<String>,
    /// Tunnel ID — persisted after first creation.
    #[serde(default)]
    pub tunnel_id: Option<String>,
}

fn default_tunnel_provider() -> String {
    "cloudflare".to_string()
}

/// Backend storage configuration for rosary orchestrator state.
///
/// Orchestrator state (pipeline tracking, dispatch history, cross-repo linkage)
/// lives here — separate from per-repo `.beads/` Dolt databases.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendConfig {
    /// Backend provider: "dolt" (default).
    #[serde(default = "default_backend_provider")]
    pub provider: String,
    /// Path to the backend database directory.
    #[serde(default = "default_backend_path")]
    pub path: std::path::PathBuf,
}

fn default_backend_provider() -> String {
    "dolt".to_string()
}

fn default_backend_path() -> std::path::PathBuf {
    std::path::PathBuf::from("~/.rsry/dolt/rosary")
}

impl BackendConfig {
    /// Returns a config with default values.
    #[allow(dead_code)] // Used in Phase 2 when reconciler wires the backend
    pub fn default_config() -> Self {
        Self {
            provider: default_backend_provider(),
            path: default_backend_path(),
        }
    }

    /// Connect to the configured backend. Returns a trait object.
    /// For sqlite, the DB file must already exist (use `connect_or_create` for migration).
    pub async fn connect(&self) -> anyhow::Result<Box<dyn crate::store::BackendStore>> {
        let path = crate::scanner::expand_path(&self.path);
        match self.provider.as_str() {
            "sqlite" => {
                if !path.exists() {
                    anyhow::bail!(
                        "SQLite backend not found at {}. Run `rsry migrate --to sqlite` first, \
                         or set [backend].provider = \"dolt\" in config.",
                        path.display()
                    );
                }
                let backend = crate::store_sqlite::SqliteBackend::connect(&path)?;
                Ok(Box::new(backend))
            }
            "dolt" => {
                let backend = crate::store_dolt::DoltBackend::connect(self).await?;
                Ok(Box::new(backend))
            }
            other => {
                anyhow::bail!(
                    "unknown [backend].provider \"{other}\". Supported: \"dolt\", \"sqlite\""
                );
            }
        }
    }

    /// Connect with BackendExport capability — used by migration and backup.
    pub async fn connect_exportable(&self) -> anyhow::Result<Box<dyn crate::store::BackendExport>> {
        let path = crate::scanner::expand_path(&self.path);
        match self.provider.as_str() {
            "sqlite" => {
                if !path.exists() {
                    anyhow::bail!(
                        "SQLite backend not found at {}. Run `rsry migrate --to sqlite` first, \
                         or set [backend].provider = \"dolt\" in config.",
                        path.display()
                    );
                }
                let backend = crate::store_sqlite::SqliteBackend::connect(&path)?;
                Ok(Box::new(backend))
            }
            "dolt" => {
                let backend = crate::store_dolt::DoltBackend::connect(self).await?;
                Ok(Box::new(backend))
            }
            other => {
                anyhow::bail!(
                    "unknown [backend].provider \"{other}\". Supported: \"dolt\", \"sqlite\""
                );
            }
        }
    }

    /// Connect or create — used by migration to create a fresh target DB.
    pub async fn connect_or_create(&self) -> anyhow::Result<Box<dyn crate::store::BackendExport>> {
        let path = crate::scanner::expand_path(&self.path);
        match self.provider.as_str() {
            "sqlite" => {
                let backend = crate::store_sqlite::SqliteBackend::connect(&path)?;
                Ok(Box::new(backend))
            }
            "dolt" => {
                let backend = crate::store_dolt::DoltBackend::connect(self).await?;
                Ok(Box::new(backend))
            }
            other => {
                anyhow::bail!(
                    "unknown [backend].provider \"{other}\". Supported: \"dolt\", \"sqlite\""
                );
            }
        }
    }
}

/// Resolve config path: $RSRY_CONFIG → ~/.rsry/config.toml → ./rosary.toml
pub fn resolve_config_path() -> String {
    if let Ok(p) = std::env::var("RSRY_CONFIG") {
        return p;
    }
    if let Some(home) = dirs_next::home_dir() {
        let global = home.join(".rsry").join("config.toml");
        if global.exists() {
            return global.to_string_lossy().to_string();
        }
    }
    "rosary.toml".to_string()
}

/// Load config from a specific file path.
pub fn load(path: &str) -> Result<Config> {
    let expanded = shellexpand::tilde(path).to_string();
    let content = std::fs::read_to_string(&expanded)
        .with_context(|| format!("reading config from {expanded}"))?;
    let config: Config =
        toml::from_str(&content).with_context(|| format!("parsing config from {expanded}"))?;
    Ok(config)
}

/// Path to `~/.rsry/`.
pub fn rsry_dir() -> PathBuf {
    dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".rsry")
}

/// Path to the single global config: `~/.rsry/config.toml`.
/// This is the ONE config file. Repos, linear settings, everything.
pub fn global_registry_path() -> Result<PathBuf> {
    let home = dirs_next::home_dir().context("cannot determine home directory")?;
    Ok(home.join(".rsry").join("config.toml"))
}

/// Load the global registry, creating it if absent.
/// Returns an empty Config if the file doesn't exist yet.
pub fn load_global() -> Result<Config> {
    let path = global_registry_path()?;
    if !path.exists() {
        return Ok(Config {
            repo: Vec::new(),
            linear: None,
            compute: None,
            http: None,
            backend: None,
            ..Default::default()
        });
    }
    warn_if_perms_too_open(&path);
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("reading global registry {}", path.display()))?;
    let config: Config =
        toml::from_str(&content).with_context(|| format!("parsing {}", path.display()))?;
    Ok(config)
}

/// Save the global registry.
fn save_global(config: &Config) -> Result<()> {
    let path = global_registry_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let content = toml::to_string_pretty(config).context("serializing config")?;
    write_secret_file(&path, content.as_bytes())?;
    Ok(())
}

/// Write a secret-bearing file, owner-only from creation. Avoids the TOCTOU
/// window of `fs::write` + later `chmod`: a NEW file is created `0600`
/// atomically (via `OpenOptions::mode`), so the secret is never world-readable
/// even briefly. A pre-existing file keeps its perms on open, so we also
/// tighten explicitly (its contents were already the prior secret — this
/// exposes nothing new and leaves it locked).
fn write_secret_file(path: &Path, content: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("creating {} (0600)", path.display()))?;
        f.write_all(content)
            .with_context(|| format!("writing {}", path.display()))?;
        // Tighten a pre-existing file whose perms predate this write.
        set_owner_only(path)?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, content).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

/// Restrict a file to owner read/write (`0600`) on Unix. No-op elsewhere.
/// Called after writing config so secrets never sit at a world-readable
/// default umask (rsry-5af158).
fn set_owner_only(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("securing permissions on {}", path.display()))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Warn (ssh-style) when a secret-bearing config file is group/other-accessible.
/// Read-time advisory only — does not fail the load.
fn warn_if_perms_too_open(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mode = meta.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                eprintln!(
                    "warning: {} is group/other-accessible (mode {:o}); it stores secrets. \
                     Run: chmod 600 {}",
                    path.display(),
                    mode,
                    path.display()
                );
            }
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Walk up the directory tree from `start` to find a repo root.
///
/// Looks for markers in order: `.beads/`, `.git/`, `.jj/`, `Cargo.toml`,
/// `go.mod`, `package.json`, `pyproject.toml`. Returns the first directory
/// that contains any marker. Like uv's pyproject.toml discovery.
pub fn discover_repo_root(start: &Path) -> Option<PathBuf> {
    const MARKERS: &[&str] = &[
        ".beads",
        ".git",
        ".jj",
        "Cargo.toml",
        "go.mod",
        "package.json",
        "pyproject.toml",
    ];

    let mut current = start.to_path_buf();
    loop {
        for marker in MARKERS {
            if current.join(marker).exists() {
                return Some(current);
            }
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Register a repo in the global registry. Idempotent — updates path if name exists.
///
/// Walks up from `repo_path` to discover the repo root (like uv's
/// pyproject.toml discovery). This means `rsry enable` works from
/// any subdirectory.
pub fn enable_repo(repo_path: &Path) -> Result<RepoConfig> {
    let abs = crate::scanner::resolve_repo_path(repo_path);

    let name = abs
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unnamed".into());

    let entry = RepoConfig {
        name: name.clone(),
        path: abs,
        lang: None,
        self_managed: false,
        // New entries default to Approved (matches the field default), so
        // existing dispatch behavior is preserved when require_approval is off.
        approval: DispatchApproval::Approved,
    };

    let mut config = load_global()?;

    // Upsert: replace existing entry with same name, or append
    if let Some(existing) = config.repo.iter_mut().find(|r| r.name == name) {
        existing.path = entry.path.clone();
    } else {
        config.repo.push(entry.clone());
    }

    save_global(&config)?;

    // Install hooks from central hooks_dir into the repo
    install_hooks(&entry.path, &config);

    Ok(entry)
}

/// Install git hooks from the central hooks_dir into a repo.
/// Hooks live at ~/.rsry/hooks/ (or config.hooks_dir), not per-branch.
fn install_hooks(repo_path: &Path, config: &Config) {
    let hooks_dir = config
        .hooks_dir
        .clone()
        .unwrap_or_else(|| rsry_dir().join("hooks"));

    if !hooks_dir.exists() {
        // First time — create the default hooks dir and seed it
        if let Err(e) = std::fs::create_dir_all(&hooks_dir) {
            eprintln!("[hooks] failed to create {}: {e}", hooks_dir.display());
            return;
        }
        seed_default_hooks(&hooks_dir);
    }

    let git_hooks_dir = repo_path.join(".git").join("hooks");
    if !git_hooks_dir.exists() {
        return; // not a git repo
    }

    // Symlink each hook from central dir into .git/hooks/
    if let Ok(entries) = std::fs::read_dir(&hooks_dir) {
        for entry in entries.flatten() {
            let src = entry.path();
            if !src.is_file() {
                continue;
            }
            let name = entry.file_name();
            let dst = git_hooks_dir.join(&name);
            // Don't overwrite existing hooks
            if dst.exists() {
                continue;
            }
            #[cfg(unix)]
            {
                if std::os::unix::fs::symlink(&src, &dst).is_ok() {
                    eprintln!(
                        "[hooks] installed {} → {}",
                        name.to_string_lossy(),
                        src.display()
                    );
                }
            }
        }
    }
}

/// Seed the default hooks directory with rosary's standard hooks.
fn seed_default_hooks(hooks_dir: &Path) {
    // commit-msg hook: Golden Rule 11
    let commit_msg = hooks_dir.join("commit-msg");
    let script = r#"#!/usr/bin/env bash
# Golden Rule 11: every commit must reference a bead.
msg=$(cat "$1")
if echo "$msg" | grep -qiE "^Merge |^initial commit"; then exit 0; fi
if echo "$msg" | grep -qE '^\[[-a-zA-Z0-9]+\] '; then exit 0; fi
if echo "$msg" | grep -qiE "bead:"; then exit 0; fi
echo "ERROR: commit message must start with [bead-id] (Golden Rule 11)"
echo "  Format: [rosary-abc123] type(scope): description"
echo "  Got: $(echo "$msg" | head -1)"
exit 1
"#;
    if let Ok(()) = std::fs::write(&commit_msg, script) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&commit_msg, std::fs::Permissions::from_mode(0o755));
        }
    }
}

/// Unregister a repo from the global registry by name or path.
pub fn disable_repo(name_or_path: &str) -> Result<Option<String>> {
    let mut config = load_global()?;
    let before = config.repo.len();

    config
        .repo
        .retain(|r| r.name != name_or_path && r.path.to_string_lossy() != name_or_path);

    if config.repo.len() == before {
        return Ok(None);
    }

    save_global(&config)?;
    Ok(Some(name_or_path.to_string()))
}

/// Update a repo's dispatch approval state in the global registry.
/// Returns `Some(name)` on success, `None` if the repo wasn't found.
pub fn set_repo_approval(name: &str, approval: DispatchApproval) -> Result<Option<String>> {
    let mut config = load_global()?;
    let entry = config.repo.iter_mut().find(|r| r.name == name);
    let Some(entry) = entry else {
        return Ok(None);
    };
    entry.approval = approval;
    save_global(&config)?;
    Ok(Some(name.to_string()))
}

/// Merge global registry with a local config file.
/// Local entries take precedence (by name) over global ones.
pub fn load_merged(local_path: &str) -> Result<Config> {
    let global = load_global()?;

    let local = match load(local_path) {
        Ok(cfg) => cfg,
        Err(_) => return Ok(global),
    };

    let mut merged = local.clone();
    for global_repo in &global.repo {
        if !merged.repo.iter().any(|r| r.name == global_repo.name) {
            merged.repo.push(global_repo.clone());
        }
    }

    // Discover plugins from ~/.rsry/plugins/ and <repo>/.rosary/plugins/.
    // Config-declared plugins win over discovered ones with the same name.
    let repo_root = Path::new(local_path).parent().map(|p| {
        if p == Path::new("") {
            Path::new(".")
        } else {
            p
        }
    });
    for plugin in discover_plugins(repo_root) {
        if !merged.plugins.iter().any(|p| p.name == plugin.name) {
            merged.plugins.push(plugin);
        }
    }

    Ok(merged)
}

/// Discover plugins from the filesystem plugin directories.
///
/// Priority (later entries override earlier ones with the same name):
/// 1. `~/.rsry/plugins/*.toml` — user-global
/// 2. `<repo_root>/.rosary/plugins/*.toml` — project-local
///
/// Each `.toml` file has the same shape as a `[[plugins]]` entry in config.
/// Non-parseable files are silently skipped (fail-open for discoverability).
pub fn discover_plugins(repo_root: Option<&Path>) -> Vec<PluginConfig> {
    let mut discovered: Vec<PluginConfig> = Vec::new();

    if let Some(home) = dirs_next::home_dir() {
        collect_plugin_dir(&home.join(".rsry").join("plugins"), &mut discovered);
    }

    if let Some(root) = repo_root {
        collect_plugin_dir(&root.join(".rosary").join("plugins"), &mut discovered);
    }

    discovered
}

fn collect_plugin_dir(dir: &Path, out: &mut Vec<PluginConfig>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("toml"))
        .collect();
    paths.sort(); // deterministic order within a directory
    for path in paths {
        if let Ok(content) = std::fs::read_to_string(&path)
            && let Ok(plugin) = toml::from_str::<PluginConfig>(&content)
        {
            // project-local overrides user-global with same name
            if let Some(existing) = out.iter_mut().find(|p| p.name == plugin.name) {
                *existing = plugin;
            } else {
                out.push(plugin);
            }
        }
    }
}

/// Build a `ComputeProvider` from config.
///
/// Returns `LocalProvider` when no `[compute]` section or `backend = "local"`.
/// Returns `SpritesProvider` when `backend = "sprites"` and token is available.
#[allow(dead_code)] // Wired in rsry-e608bb (reconciler integration)
pub fn compute_provider_from_config(
    config: &Config,
) -> Result<Box<dyn crate::backend::ComputeProvider>> {
    let Some(compute) = &config.compute else {
        return Ok(Box::new(crate::backend::LocalProvider));
    };

    match compute.backend.as_str() {
        "local" | "" => Ok(Box::new(crate::backend::LocalProvider)),
        "sprites" => {
            let sprites_cfg = compute
                .sprites
                .as_ref()
                .context("backend = \"sprites\" requires [compute.sprites] section")?;

            let token = std::env::var(&sprites_cfg.token_env).with_context(|| {
                format!(
                    "sprites API token: set ${} or change compute.sprites.token_env",
                    sprites_cfg.token_env
                )
            })?;

            let client = if let Some(ref base_url) = sprites_cfg.base_url {
                crate::sprites::SpritesClient::with_base_url(&token, base_url)?
            } else {
                crate::sprites::SpritesClient::new(&token)?
            };

            let provider = crate::sprites_provider::SpritesProvider::new(client)
                .with_network_allowlist(sprites_cfg.network_allowlist.clone())
                .with_checkpoints(sprites_cfg.checkpoint_on_complete);

            Ok(Box::new(provider))
        }
        other => anyhow::bail!("unknown compute backend: \"{other}\" (available: local, sprites)"),
    }
}

#[cfg(test)]
mod tests;
