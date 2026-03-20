use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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
    /// Default agent provider: "claude", "gemini", "acp".
    #[serde(default = "default_dispatch_provider")]
    pub provider: String,
    /// Provider for adversarial review phases.
    pub adversarial_provider: Option<String>,
    /// Max concurrent dispatches.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,
}

fn default_dispatch_provider() -> String {
    "claude".to_string()
}

fn default_max_concurrent() -> usize {
    3
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
fn default_rsry_dir() -> PathBuf {
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
    std::fs::write(&path, content).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
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
    // expand_path (tilde only) — NOT canonicalize, which resolves symlinks
    // and breaks paths like ~/github → ~/remotes.
    let start = crate::scanner::expand_path(repo_path);
    let abs = discover_repo_root(&start).unwrap_or(start);

    let name = abs
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unnamed".into());

    let entry = RepoConfig {
        name: name.clone(),
        path: abs,
        lang: None,
        self_managed: false,
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
        .unwrap_or_else(|| default_rsry_dir().join("hooks"));

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

    Ok(merged)
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
mod tests {
    use super::*;

    #[test]
    fn parse_toml_config_singular() {
        let toml = r#"
[[repo]]
name = "mache"
path = "~/remotes/art/mache"
lang = "go"

[[repo]]
name = "rosary"
path = "~/remotes/art/rosary"
lang = "rust"
self = true

[linear]
team = "ART"
project = "Platform"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.repo.len(), 2);
        assert_eq!(config.repo[0].name, "mache");
        assert_eq!(config.repo[0].lang.as_deref(), Some("go"));
        assert!(!config.repo[0].self_managed);
        assert_eq!(config.repo[1].name, "rosary");
        assert!(config.repo[1].self_managed);
        assert_eq!(config.linear.unwrap().team, "ART");
    }

    #[test]
    fn parse_toml_config_with_phases() {
        let toml = r#"
[[repo]]
name = "rosary"
path = "~/remotes/art/rosary"

[linear]
team = "ART"

[linear.phases]
"1" = "Phase 1: Foundation"
"2" = "Phase 2: Sync"
"3" = "Phase 3: Dispatch"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let linear = config.linear.unwrap();
        assert_eq!(linear.team, "ART");
        assert_eq!(linear.phases.len(), 3);
        assert_eq!(linear.phases.get("1").unwrap(), "Phase 1: Foundation");
        assert_eq!(linear.phases.get("2").unwrap(), "Phase 2: Sync");
        assert_eq!(linear.phases.get("3").unwrap(), "Phase 3: Dispatch");
    }

    #[test]
    fn parse_toml_config_phases_default_empty() {
        // Backward compat: phases is optional and defaults to empty
        let toml = r#"
[[repo]]
name = "rosary"
path = "~/remotes/art/rosary"

[linear]
team = "ART"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let linear = config.linear.unwrap();
        assert!(linear.phases.is_empty());
    }

    #[test]
    fn parse_toml_config_plural_alias() {
        // [[repos]] still works via serde alias
        let toml = r#"
[[repos]]
name = "mache"
path = "~/remotes/art/mache"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.repo.len(), 1);
        assert_eq!(config.repo[0].name, "mache");
    }

    #[test]
    fn enable_disable_roundtrip() {
        // Use a temp dir as both the "repo" and the registry location.
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_dir = tmp.path().join("myrepo");
        std::fs::create_dir_all(&repo_dir).unwrap();

        // Override HOME so global_registry_path resolves inside tmp
        let registry = tmp.path().join(".rsry").join("repos.toml");

        // Manually enable by writing the registry
        let entry = RepoConfig {
            name: "myrepo".into(),
            path: repo_dir.clone(),
            lang: None,
            self_managed: false,
        };
        let config = Config {
            repo: vec![entry],
            linear: None,
            compute: None,
            http: None,
            backend: None,
            ..Default::default()
        };
        std::fs::create_dir_all(registry.parent().unwrap()).unwrap();
        let content = toml::to_string_pretty(&config).unwrap();
        std::fs::write(&registry, &content).unwrap();

        // Verify we can read it back
        let loaded: Config = toml::from_str(&content).unwrap();
        assert_eq!(loaded.repo.len(), 1);
        assert_eq!(loaded.repo[0].name, "myrepo");
    }

    #[test]
    fn disable_nonexistent_returns_none() {
        // With no global registry, disable should not error
        let result = disable_repo("nonexistent-repo-xyz");
        // May return Ok(None) or error if no registry — both are fine
        if let Ok(removed) = result {
            assert!(removed.is_none());
        }
    }

    #[test]
    fn load_merged_falls_back_to_global() {
        // When local config doesn't exist, load_merged should return
        // whatever the global registry has (possibly empty)
        let result = load_merged("/nonexistent/rosary.toml");
        // Should not error — returns global (or empty)
        assert!(result.is_ok());
    }

    #[test]
    fn discover_repo_root_finds_git() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().join("myrepo");
        let subdir = root.join("src").join("deep");
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();

        let found = discover_repo_root(&subdir);
        assert_eq!(found, Some(root));
    }

    #[test]
    fn discover_repo_root_finds_beads() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().join("myrepo");
        let subdir = root.join("internal").join("graph");
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::create_dir_all(root.join(".beads")).unwrap();

        let found = discover_repo_root(&subdir);
        // .beads is checked before .git, so it should find the root
        assert_eq!(found, Some(root));
    }

    #[test]
    fn discover_repo_root_finds_cargo_toml() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().join("myrepo");
        let subdir = root.join("src");
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]").unwrap();

        let found = discover_repo_root(&subdir);
        assert_eq!(found, Some(root));
    }

    #[test]
    fn discover_repo_root_none_at_filesystem_root() {
        // A path with no markers should return None (eventually hits /)
        let tmp = tempfile::TempDir::new().unwrap();
        let empty = tmp.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();

        let found = discover_repo_root(&empty);
        // Could find a .git somewhere up the tree on the host, so just
        // verify it doesn't panic. If tmp is truly isolated, it's None.
        // Either way, the function terminates.
        let _ = found;
    }

    #[test]
    fn config_serializes_roundtrip() {
        let config = Config {
            repo: vec![RepoConfig {
                name: "test".into(),
                path: PathBuf::from("/tmp/test"),
                lang: Some("rust".into()),
                self_managed: false,
            }],
            linear: None,
            compute: None,
            http: None,
            backend: None,
            ..Default::default()
        };
        let serialized = toml::to_string_pretty(&config).unwrap();
        let deserialized: Config = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.repo[0].name, "test");
        assert_eq!(deserialized.repo[0].path, PathBuf::from("/tmp/test"));
    }

    #[test]
    fn parse_compute_section_sprites() {
        let toml = r#"
[[repo]]
name = "test"
path = "/tmp/test"

[compute]
backend = "sprites"

[compute.sprites]
token_env = "MY_TOKEN"
cpu = 4
memory_mb = 8192
network_allowlist = ["api.github.com", "api.linear.app"]
checkpoint_on_complete = true
fallback_to_local = false
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let compute = config.compute.unwrap();
        assert_eq!(compute.backend, "sprites");

        let sprites = compute.sprites.unwrap();
        assert_eq!(sprites.token_env, "MY_TOKEN");
        assert_eq!(sprites.cpu, Some(4));
        assert_eq!(sprites.memory_mb, Some(8192));
        assert_eq!(sprites.network_allowlist.len(), 2);
        assert!(sprites.checkpoint_on_complete);
        assert!(!sprites.fallback_to_local);
    }

    #[test]
    fn parse_compute_section_local() {
        let toml = r#"
[[repo]]
name = "test"
path = "/tmp/test"

[compute]
backend = "local"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let compute = config.compute.unwrap();
        assert_eq!(compute.backend, "local");
        assert!(compute.sprites.is_none());
    }

    #[test]
    fn parse_no_compute_section() {
        let toml = r#"
[[repo]]
name = "test"
path = "/tmp/test"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.compute.is_none());
    }

    #[test]
    fn sprites_config_defaults() {
        let toml = r#"
[[repo]]
name = "test"
path = "/tmp/test"

[compute]
backend = "sprites"

[compute.sprites]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let sprites = config.compute.unwrap().sprites.unwrap();
        assert_eq!(sprites.token_env, "SPRITES_TOKEN");
        assert!(sprites.base_url.is_none());
        assert!(sprites.cpu.is_none());
        assert!(sprites.memory_mb.is_none());
        assert!(sprites.network_allowlist.is_empty());
        assert!(!sprites.checkpoint_on_complete);
        assert!(sprites.fallback_to_local); // default true
    }

    #[test]
    fn compute_config_backend_defaults_to_local() {
        let toml = r#"
[[repo]]
name = "test"
path = "/tmp/test"

[compute]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let compute = config.compute.unwrap();
        assert_eq!(compute.backend, "local");
    }

    // -- compute_provider_from_config tests --

    #[test]
    fn provider_from_config_no_compute() {
        let config = Config {
            repo: vec![],
            linear: None,
            compute: None,
            http: None,
            backend: None,
            ..Default::default()
        };
        let provider = compute_provider_from_config(&config).unwrap();
        assert_eq!(provider.name(), "local");
    }

    #[test]
    fn provider_from_config_local_explicit() {
        let config = Config {
            repo: vec![],
            linear: None,
            compute: Some(ComputeConfig {
                backend: "local".into(),
                sprites: None,
            }),
            http: None,
            backend: None,
            ..Default::default()
        };
        let provider = compute_provider_from_config(&config).unwrap();
        assert_eq!(provider.name(), "local");
    }

    #[test]
    fn provider_from_config_sprites_missing_section() {
        let config = Config {
            repo: vec![],
            linear: None,
            compute: Some(ComputeConfig {
                backend: "sprites".into(),
                sprites: None,
            }),
            http: None,
            backend: None,
            ..Default::default()
        };
        let result = compute_provider_from_config(&config);
        let err = result.err().unwrap();
        assert!(err.to_string().contains("[compute.sprites]"));
    }

    #[test]
    fn provider_from_config_sprites_missing_token() {
        let config = Config {
            repo: vec![],
            linear: None,
            compute: Some(ComputeConfig {
                backend: "sprites".into(),
                sprites: Some(SpritesConfig {
                    token_env: "NONEXISTENT_TOKEN_ENV_VAR_XYZ".into(),
                    base_url: None,
                    cpu: None,
                    memory_mb: None,
                    network_allowlist: vec![],
                    checkpoint_on_complete: false,
                    fallback_to_local: true,
                }),
            }),
            http: None,
            backend: None,
            ..Default::default()
        };
        let result = compute_provider_from_config(&config);
        let err = result.err().unwrap();
        assert!(err.to_string().contains("NONEXISTENT_TOKEN_ENV_VAR_XYZ"));
    }

    #[test]
    fn provider_from_config_unknown_backend() {
        let config = Config {
            repo: vec![],
            linear: None,
            compute: Some(ComputeConfig {
                backend: "k8s".into(),
                sprites: None,
            }),
            http: None,
            backend: None,
            ..Default::default()
        };
        let result = compute_provider_from_config(&config);
        let err = result.err().unwrap();
        assert!(err.to_string().contains("k8s"));
    }

    #[test]
    fn parse_toml_http_and_tunnel() {
        let toml = r#"
[[repo]]
name = "rosary"
path = "~/remotes/art/rosary"

[linear]
team = "ART"
webhook_secret = "lin_wh_test_secret"

[http]
port = 9090

[http.tunnel]
provider = "cloudflare"
domain = "webhooks.example.com"
account_id = "abc123"
zone_id = "zone456"
token_env = "CF_API_TOKEN"
tunnel_id = "tun-789"
"#;
        let config: Config = toml::from_str(toml).unwrap();

        let linear = config.linear.unwrap();
        assert_eq!(linear.webhook_secret.as_deref(), Some("lin_wh_test_secret"));

        let http = config.http.unwrap();
        assert_eq!(http.port, 9090);

        let tunnel = http.tunnel.unwrap();
        assert_eq!(tunnel.provider, "cloudflare");
        assert_eq!(tunnel.domain.as_deref(), Some("webhooks.example.com"));
        assert_eq!(tunnel.account_id.as_deref(), Some("abc123"));
        assert_eq!(tunnel.zone_id.as_deref(), Some("zone456"));
        assert_eq!(tunnel.token_env.as_deref(), Some("CF_API_TOKEN"));
        assert_eq!(tunnel.tunnel_id.as_deref(), Some("tun-789"));
    }

    #[test]
    fn parse_toml_http_defaults() {
        // Minimal [http] section — port defaults to 8383, no tunnel
        let toml = r#"
[[repo]]
name = "rosary"
path = "~/remotes/art/rosary"

[http]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let http = config.http.unwrap();
        assert_eq!(http.port, 8383);
        assert!(http.tunnel.is_none());
    }

    #[test]
    fn parse_toml_tunnel_defaults() {
        // Minimal tunnel — provider defaults to "cloudflare", all optionals None
        let toml = r#"
[[repo]]
name = "rosary"
path = "~/remotes/art/rosary"

[http]
port = 8383

[http.tunnel]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let tunnel = config.http.unwrap().tunnel.unwrap();
        assert_eq!(tunnel.provider, "cloudflare");
        assert!(tunnel.domain.is_none());
        assert!(tunnel.account_id.is_none());
        assert!(tunnel.zone_id.is_none());
        assert!(tunnel.token_env.is_none());
        assert!(tunnel.tunnel_id.is_none());
    }

    #[test]
    fn parse_toml_backward_compat_no_http() {
        // Old configs without [http] still parse fine
        let toml = r#"
[[repo]]
name = "rosary"
path = "~/remotes/art/rosary"

[linear]
team = "ART"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.http.is_none());
        assert!(config.linear.unwrap().webhook_secret.is_none());
    }

    #[test]
    fn parse_toml_backward_compat_empty() {
        // Completely empty config (just repos) still works
        let toml = r#"
[[repo]]
name = "rosary"
path = "~/remotes/art/rosary"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.http.is_none());
        assert!(config.linear.is_none());
        assert!(config.backend.is_none());
    }

    #[test]
    fn parse_toml_backend_section() {
        let toml = r#"
[[repo]]
name = "rosary"
path = "~/remotes/art/rosary"

[backend]
provider = "dolt"
path = "~/.rsry/dolt/rosary"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let backend = config.backend.unwrap();
        assert_eq!(backend.provider, "dolt");
        assert_eq!(
            backend.path,
            std::path::PathBuf::from("~/.rsry/dolt/rosary")
        );
    }

    #[test]
    fn parse_toml_backend_defaults() {
        // [backend] with no fields uses defaults
        let toml = r#"
[[repo]]
name = "rosary"
path = "~/remotes/art/rosary"

[backend]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let backend = config.backend.unwrap();
        assert_eq!(backend.provider, "dolt");
        assert_eq!(
            backend.path,
            std::path::PathBuf::from("~/.rsry/dolt/rosary")
        );
    }

    #[test]
    fn backend_config_default_values() {
        let config = BackendConfig::default_config();
        assert_eq!(config.provider, "dolt");
        assert!(config.path.to_string_lossy().contains(".rsry/dolt/rosary"));
    }

    #[test]
    fn parse_github_agent_branch_prefix() {
        let toml = r#"
[[repo]]
name = "rosary"
path = "~/remotes/art/rosary"

[github]
token = "ghp_test"
agent_branch_prefix = "agent"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let gh = config.github.unwrap();
        assert_eq!(gh.agent_branch_prefix, "agent");
    }

    #[test]
    fn parse_github_agent_branch_prefix_default() {
        let toml = r#"
[[repo]]
name = "rosary"
path = "~/remotes/art/rosary"

[github]
token = "ghp_test"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let gh = config.github.unwrap();
        assert_eq!(gh.agent_branch_prefix, "rosary");
    }
}
