use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Repo entries — accepts `[[repo]]` in TOML (singular).
    #[serde(alias = "repos")]
    pub repo: Vec<RepoConfig>,
    pub linear: Option<LinearConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoConfig {
    /// Display name for the repo
    pub name: String,
    /// Path to the repo root (absolute or ~ prefixed)
    pub path: PathBuf,
    /// Language hint (rust, go, python, etc.). Auto-detected if absent.
    pub lang: Option<String>,
    /// Whether this repo IS loom itself (dogfooding flag).
    #[serde(default, rename = "self")]
    pub self_managed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinearConfig {
    /// Linear team key (e.g., "ART")
    pub team: String,
    /// Linear project name for cross-repo tracking
    pub project: Option<String>,
}

pub fn load(path: &str) -> Result<Config> {
    let expanded = shellexpand::tilde(path).to_string();
    let content = std::fs::read_to_string(&expanded)
        .with_context(|| format!("reading config from {expanded}"))?;
    let config: Config =
        toml::from_str(&content).with_context(|| format!("parsing config from {expanded}"))?;
    Ok(config)
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
name = "loom"
path = "~/remotes/art/loom"
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
        assert_eq!(config.repo[1].name, "loom");
        assert!(config.repo[1].self_managed);
        assert_eq!(config.linear.unwrap().team, "ART");
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
}
