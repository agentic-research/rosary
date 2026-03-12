use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub repos: Vec<RepoConfig>,
    pub linear: Option<LinearConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoConfig {
    /// Display name for the repo
    pub name: String,
    /// Path to the repo root (absolute or ~ prefixed)
    pub path: PathBuf,
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

/// Default config for when no file exists — scans known ART repos
pub fn default_config() -> Config {
    Config {
        repos: vec![
            repo("mache", "~/remotes/art/mache"),
            repo("assay", "~/remotes/art/assay"),
            repo("tropo", "~/remotes/art/tropo"),
            repo("ley-line", "~/remotes/art/ley-line"),
            repo("loom", "~/remotes/art/loom"),
        ],
        linear: None,
    }
}

fn repo(name: &str, path: &str) -> RepoConfig {
    RepoConfig {
        name: name.to_string(),
        path: PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_toml_config() {
        let toml = r#"
[[repos]]
name = "mache"
path = "~/remotes/art/mache"

[[repos]]
name = "assay"
path = "~/remotes/art/assay"

[linear]
team = "ART"
project = "Platform"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.repos.len(), 2);
        assert_eq!(config.repos[0].name, "mache");
        assert_eq!(config.linear.unwrap().team, "ART");
    }
}
