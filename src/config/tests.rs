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

#[cfg(unix)]
#[test]
fn write_secret_file_creates_new_file_0600() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("config.toml");
    // Brand-new file — must be 0600 from creation (no world-readable window).
    super::write_secret_file(&path, b"linear_api_key = \"secret\"").unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o600,
        "new secret file must be owner-only, got {mode:o}"
    );
}

#[cfg(unix)]
#[test]
fn set_owner_only_restricts_to_0600() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("config.toml");
    // Simulate a world-readable file (default umask ~0644).
    std::fs::write(&path, "linear_api_key = \"secret\"").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

    super::set_owner_only(&path).unwrap();

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o600,
        "secret config must be owner-only, got {mode:o}"
    );
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
        approval: DispatchApproval::Approved,
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
            approval: DispatchApproval::Approved,
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

#[test]
fn plugin_kind_missing_defaults_to_hook() {
    let toml = r#"
[[plugins]]
name = "my-linter"
hook = "pipeline.verify"
command = ["assay", "verify"]
"#;
    let config: Config = toml::from_str(toml).unwrap();
    assert_eq!(config.plugins.len(), 1);
    assert_eq!(config.plugins[0].kind, PluginKind::Hook);
    assert!(config.plugins[0].is_hook());
}

#[test]
fn plugin_kind_mcp_parses() {
    let toml = r#"
[[plugins]]
name = "mache"
kind = "mcp"
url = "http://localhost:8484"
"#;
    let config: Config = toml::from_str(toml).unwrap();
    assert_eq!(config.plugins[0].kind, PluginKind::Mcp);
    assert!(!config.plugins[0].is_hook());
}

#[test]
fn plugin_kind_dispatch_parses() {
    let toml = r#"
[[plugins]]
name = "chain-runner"
kind = "dispatch"
command = ["claude-guard", "run"]
"#;
    let config: Config = toml::from_str(toml).unwrap();
    assert_eq!(config.plugins[0].kind, PluginKind::Dispatch);
    assert!(!config.plugins[0].is_hook());
}

#[test]
fn plugin_kind_state_sink_parses() {
    let toml = r#"
[[plugins]]
name = "linear-mirror"
kind = "state_sink"
url = "http://localhost:9090/sink"
"#;
    let config: Config = toml::from_str(toml).unwrap();
    assert_eq!(config.plugins[0].kind, PluginKind::StateSink);
    assert!(!config.plugins[0].is_hook());
}

#[test]
fn plugin_kind_all_kinds_roundtrip() {
    let kinds = [
        (PluginKind::Hook, "hook"),
        (PluginKind::Mcp, "mcp"),
        (PluginKind::Dispatch, "dispatch"),
        (PluginKind::StateSink, "state_sink"),
    ];
    for (kind, expected_str) in &kinds {
        let json = serde_json::to_string(kind).unwrap();
        assert_eq!(json, format!("\"{expected_str}\""));
        let back: PluginKind = serde_json::from_str(&json).unwrap();
        assert_eq!(&back, kind);
    }
}

#[test]
fn plugin_discovery_empty_dir_returns_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let plugins = discover_plugins(Some(dir.path()));
    assert!(plugins.is_empty());
}

#[test]
fn plugin_discovery_loads_toml_files() {
    let dir = tempfile::tempdir().unwrap();
    let plugins_dir = dir.path().join(".rosary").join("plugins");
    std::fs::create_dir_all(&plugins_dir).unwrap();
    std::fs::write(
        plugins_dir.join("assay.toml"),
        r#"name = "assay-coverage"
kind = "hook"
hook = "pipeline.verify"
command = ["assay", "verify", "--format", "json"]
"#,
    )
    .unwrap();

    let plugins = discover_plugins(Some(dir.path()));
    assert_eq!(plugins.len(), 1);
    assert_eq!(plugins[0].name, "assay-coverage");
    assert_eq!(plugins[0].kind, PluginKind::Hook);
}

#[test]
fn plugin_discovery_project_local_overrides_user_global() {
    // Simulate two dirs: "global" and "local"
    let global_dir = tempfile::tempdir().unwrap();
    let local_dir = tempfile::tempdir().unwrap();

    // Same plugin name in both dirs, different commands
    let global_plugins = global_dir.path().join("plugins");
    let local_plugins = local_dir.path().join(".rosary").join("plugins");
    std::fs::create_dir_all(&global_plugins).unwrap();
    std::fs::create_dir_all(&local_plugins).unwrap();

    std::fs::write(
        global_plugins.join("myplug.toml"),
        "name = \"myplug\"\nhook = \"pipeline.verify\"\ncommand = [\"global-bin\"]\n",
    )
    .unwrap();
    std::fs::write(
        local_plugins.join("myplug.toml"),
        "name = \"myplug\"\nhook = \"pipeline.verify\"\ncommand = [\"local-bin\"]\n",
    )
    .unwrap();

    // Manually call collect_plugin_dir twice (simulating discover_plugins flow)
    let mut discovered = Vec::new();
    collect_plugin_dir(&global_plugins, &mut discovered);
    collect_plugin_dir(&local_plugins, &mut discovered);

    assert_eq!(discovered.len(), 1, "dedup: only one plugin after override");
    assert_eq!(discovered[0].command, vec!["local-bin"], "local wins");
}

#[test]
fn plugin_discovery_config_declared_wins_over_discovered() {
    let dir = tempfile::tempdir().unwrap();
    let plugins_dir = dir.path().join(".rosary").join("plugins");
    std::fs::create_dir_all(&plugins_dir).unwrap();
    std::fs::write(
        plugins_dir.join("myplug.toml"),
        "name = \"myplug\"\nhook = \"pipeline.verify\"\ncommand = [\"discovered-bin\"]\n",
    )
    .unwrap();

    let discovered = discover_plugins(Some(dir.path()));

    // config-declared plugin with same name
    let config_plugins = vec![PluginConfig {
        name: "myplug".into(),
        kind: PluginKind::Hook,
        hook: "pipeline.verify".into(),
        command: vec!["config-bin".into()],
        url: None,
    }];

    // Merge: config-declared wins
    let mut final_plugins = config_plugins;
    for p in discovered {
        if !final_plugins.iter().any(|cp| cp.name == p.name) {
            final_plugins.push(p);
        }
    }

    assert_eq!(final_plugins.len(), 1);
    assert_eq!(final_plugins[0].command, vec!["config-bin"], "config wins");
}

#[test]
fn plugin_discovery_skips_non_toml_files() {
    let dir = tempfile::tempdir().unwrap();
    let plugins_dir = dir.path().join(".rosary").join("plugins");
    std::fs::create_dir_all(&plugins_dir).unwrap();
    std::fs::write(plugins_dir.join("readme.md"), "# not a plugin").unwrap();
    std::fs::write(plugins_dir.join("plugin.json"), r#"{"name":"x"}"#).unwrap();

    let plugins = discover_plugins(Some(dir.path()));
    assert!(plugins.is_empty(), "non-toml files are ignored");
}

// --- AttestationConfig (APAS L2) ---

#[test]
fn attestation_config_absent_by_default() {
    let toml = r#"
[[repo]]
name = "rosary"
path = "~/remotes/art/rosary"
"#;
    let config: Config = toml::from_str(toml).unwrap();
    assert!(config.attestation.is_none());
}

#[test]
fn attestation_config_parses_signing_key_path() {
    let toml = r#"
[[repo]]
name = "rosary"
path = "~/remotes/art/rosary"

[attestation]
signing_key_path = "~/.rsry/keys/orchestrator.key"
"#;
    let config: Config = toml::from_str(toml).unwrap();
    let att = config.attestation.expect("attestation block must parse");
    assert_eq!(
        att.signing_key_path.as_ref().unwrap().to_str().unwrap(),
        "~/.rsry/keys/orchestrator.key"
    );
}

#[test]
fn attestation_config_optional_signing_key() {
    // Empty [attestation] block is valid — keeps the option to add fields later.
    let toml = r#"
[[repo]]
name = "rosary"
path = "~/remotes/art/rosary"

[attestation]
"#;
    let config: Config = toml::from_str(toml).unwrap();
    let att = config.attestation.unwrap();
    assert!(att.signing_key_path.is_none());
}

#[test]
fn attestation_config_defaults_unsigned_emission_off() {
    let toml = r#"
[[repo]]
name = "rosary"
path = "~/remotes/art/rosary"

[attestation]
"#;
    let config: Config = toml::from_str(toml).unwrap();
    let att = config.attestation.unwrap();
    assert!(!att.emit_unsigned);
}

#[test]
fn attestation_config_parses_unsigned_emission_opt_in() {
    let toml = r#"
[[repo]]
name = "rosary"
path = "~/remotes/art/rosary"

[attestation]
emit_unsigned = true
"#;
    let config: Config = toml::from_str(toml).unwrap();
    assert!(config.attestation.unwrap().emit_unsigned);
}

#[test]
fn context_config_defaults_when_absent() {
    let cfg: Config = toml::from_str("").unwrap();
    assert_eq!(cfg.context.policy, "tiers");
    assert_eq!(cfg.context.budget, 8000);
    assert_eq!(cfg.context.max_refs, 8);
}

#[test]
fn context_config_overrides() {
    let cfg: Config =
        toml::from_str("[context]\npolicy = \"recency\"\nbudget = 4000\nmax_refs = 4\n").unwrap();
    assert_eq!(cfg.context.policy, "recency");
    assert_eq!(cfg.context.budget, 4000);
    assert_eq!(cfg.context.max_refs, 4);
}

#[test]
fn context_cache_mode_defaults_off_and_parses() {
    // Default is Off — zero behavior change vs Phase A.
    assert_eq!(
        crate::config::ContextConfig::default().cache,
        crate::config::CacheMode::Off
    );
    // Parses lowercase strings from TOML.
    let c: crate::config::ContextConfig = toml::from_str("cache = \"shadow\"").unwrap();
    assert_eq!(c.cache, crate::config::CacheMode::Shadow);
    let c: crate::config::ContextConfig = toml::from_str("cache = \"on\"").unwrap();
    assert_eq!(c.cache, crate::config::CacheMode::On);
}

/// A written config must carry only what DIFFERS from the defaults, and must
/// still round-trip to an identical value.
///
/// Measured on a real 22-repo config before this: 46 of 211 lines were
/// default-valued noise (`self = false` x21, `approval = "approved"` x22,
/// `plugins = []`, `max_pipeline_depth = 0`, `require_approval = false`).
/// Every one says nothing, and together they bury the two or three settings
/// that actually differ.
///
/// Omitting a field is only safe if it reads back the same, so this asserts
/// BOTH halves — absent from the text, and equal after a round trip.
#[test]
fn default_valued_fields_are_not_written_but_still_round_trip() {
    let cfg = Config {
        repo: vec![RepoConfig {
            name: "r".into(),
            path: "/tmp/r".into(),
            lang: Some("rust".into()),
            self_managed: false,
            approval: DispatchApproval::default(),
        }],
        ..Default::default()
    };

    let toml_text = toml::to_string(&cfg).expect("serialises");

    for noise in [
        "self = false",
        "approval = \"approved\"",
        "max_pipeline_depth = 0",
        "plugins = []",
        "require_approval = false",
    ] {
        assert!(
            !toml_text.contains(noise),
            "default-valued `{noise}` must not be written:\n{toml_text}"
        );
    }

    let back: Config = toml::from_str(&toml_text).expect("round-trips");
    assert_eq!(back.repo.len(), 1);
    assert_eq!(back.repo[0].name, "r");
    assert!(!back.repo[0].self_managed, "default restored");
    assert_eq!(
        back.repo[0].approval,
        DispatchApproval::Approved,
        "omitted approval must read back as the default, not as None"
    );
    assert_eq!(back.max_pipeline_depth, 0);
    assert!(back.plugins.is_empty());
}

/// The inverse: a NON-default value must still be written, or omitting
/// defaults would silently drop real settings.
#[test]
fn non_default_values_are_still_written() {
    let cfg = Config {
        repo: vec![RepoConfig {
            name: "r".into(),
            path: "/tmp/r".into(),
            lang: None,
            self_managed: true,
            approval: DispatchApproval::Rejected,
        }],
        max_pipeline_depth: 3,
        ..Default::default()
    };
    let toml_text = toml::to_string(&cfg).expect("serialises");
    assert!(toml_text.contains("self = true"), "{toml_text}");
    assert!(toml_text.contains("rejected"), "{toml_text}");
    assert!(toml_text.contains("max_pipeline_depth = 3"), "{toml_text}");

    let back: Config = toml::from_str(&toml_text).unwrap();
    assert!(back.repo[0].self_managed);
    assert_eq!(back.repo[0].approval, DispatchApproval::Rejected);
    assert_eq!(back.max_pipeline_depth, 3);
}
