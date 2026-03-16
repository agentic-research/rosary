//! GitHub PR creation — minimal REST API client via reqwest.
//!
//! Uses fine-grained PAT with `contents:write` + `pull_requests:write` perms.
//! Token read from `GITHUB_TOKEN` env var or `~/.rsry/config.toml` [github].

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Minimal GitHub client for PR operations.
pub struct GitHubClient {
    token: String,
    client: reqwest::Client,
}

#[derive(Debug, Serialize)]
struct CreatePrRequest {
    title: String,
    body: String,
    head: String,
    base: String,
}

#[derive(Debug, Deserialize)]
pub struct PrResponse {
    pub number: u64,
    pub html_url: String,
}

impl GitHubClient {
    /// Create a client from env var or config.
    pub fn from_env() -> Result<Self> {
        let token = std::env::var("GITHUB_TOKEN")
            .or_else(|_| token_from_config())
            .context("GITHUB_TOKEN not set and not found in ~/.rsry/config.toml [github]")?;

        Ok(GitHubClient {
            token,
            client: reqwest::Client::new(),
        })
    }

    /// Push a local branch to origin, then create a PR.
    ///
    /// `workspace_dir`: path to the git worktree
    /// `owner`/`repo`: GitHub owner/repo (e.g. "agentic-research", "rosary")
    /// `branch`: branch name (e.g. "fix/rosary-abc")
    /// `base`: target branch (e.g. "main")
    /// `title`: PR title
    /// `body`: PR body (handoff chain, SBOM summary, etc.)
    #[allow(clippy::too_many_arguments)]
    pub async fn create_pr_from_worktree(
        &self,
        workspace_dir: &std::path::Path,
        owner: &str,
        repo: &str,
        branch: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<PrResponse> {
        // Push the branch
        let push = tokio::process::Command::new("git")
            .args(["push", "origin", branch])
            .current_dir(workspace_dir)
            .output()
            .await
            .context("git push")?;

        if !push.status.success() {
            let stderr = String::from_utf8_lossy(&push.stderr);
            anyhow::bail!("git push failed: {stderr}");
        }
        eprintln!("[github] pushed {branch}");

        // Create PR via REST API
        let url = format!("https://api.github.com/repos/{owner}/{repo}/pulls");
        let pr_request = CreatePrRequest {
            title: title.to_string(),
            body: body.to_string(),
            head: branch.to_string(),
            base: base.to_string(),
        };

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "rosary")
            .json(&pr_request)
            .send()
            .await
            .context("GitHub API request")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub PR creation failed ({status}): {body}");
        }

        let pr: PrResponse = resp.json().await.context("parsing PR response")?;
        eprintln!("[github] created PR #{}: {}", pr.number, pr.html_url);
        Ok(pr)
    }
}

/// Read GitHub token from ~/.rsry/config.toml [github] section.
fn token_from_config() -> Result<String> {
    let home = dirs_next::home_dir().context("no home dir")?;
    let config_path = home.join(".rsry").join("config.toml");
    let content = std::fs::read_to_string(&config_path)
        .with_context(|| format!("reading {}", config_path.display()))?;
    let config: toml::Value = content.parse().context("parsing config.toml")?;
    config
        .get("github")
        .and_then(|g| g.get("token"))
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .context("[github] token not found in config.toml")
}

/// Build a PR body from handoff chain + manifest.
pub fn build_pr_body(
    handoffs: &[crate::handoff::Handoff],
    manifest: Option<&crate::manifest::Manifest>,
) -> String {
    let mut body = String::from("## Agent-Generated PR\n\n");

    // Handoff summaries
    if !handoffs.is_empty() {
        body.push_str("### Pipeline Phases\n\n");
        for h in handoffs {
            body.push_str(&format!(
                "**Phase {} — {}** ({})\n",
                h.phase, h.from_agent, h.provider
            ));
            body.push_str(&format!("{}\n", h.summary));
            if let Some(ref v) = h.verdict {
                body.push_str(&format!("Verdict: {}\n", v.decision));
                for c in &v.concerns {
                    body.push_str(&format!("- {c}\n"));
                }
            }
            body.push('\n');
        }
    }

    // Cost summary from manifest
    if let Some(m) = manifest {
        if let Some(cost) = m.cost.total_cost_usd {
            body.push_str(&format!("### Cost\n\n${cost:.3}\n\n"));
        }
        body.push_str(&format!(
            "### Files Changed\n\n{}\n\n",
            m.work
                .files_changed
                .iter()
                .map(|f| format!("- `{f}`"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    body.push_str("---\n*Generated by [rosary](https://github.com/agentic-research/rosary)*\n");
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_pr_body_empty() {
        let body = build_pr_body(&[], None);
        assert!(body.contains("Agent-Generated PR"));
        assert!(body.contains("rosary"));
    }

    #[test]
    fn build_pr_body_with_handoffs() {
        let work = crate::manifest::Work {
            commits: vec![],
            files_changed: vec!["src/foo.rs".into()],
            lines_added: 10,
            lines_removed: 2,
            diff_stat: None,
        };
        let h = crate::handoff::Handoff::new(
            0,
            "dev-agent",
            Some("staging-agent"),
            "rosary-test",
            "claude",
            &work,
        );
        let body = build_pr_body(&[h], None);
        assert!(body.contains("Phase 0"));
        assert!(body.contains("dev-agent"));
    }

    #[test]
    fn token_from_config_missing_file() {
        // Should fail gracefully
        assert!(token_from_config().is_err());
    }
}
