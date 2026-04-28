//! Pipeline plugin system — external tools that hook into verify/review/triage stages.
//!
//! Plugins are external processes or HTTP endpoints. Rosary acts as an MCP-style *client*:
//! it sends structured context (JSON) to the plugin and receives a structured verdict.
//!
//! # Protocol
//!
//! ## Subprocess transport
//! Rosary spawns the configured command, writes JSON to stdin, reads JSON from stdout.
//! Stderr is inherited so interactive TUIs can render normally.
//!
//! Input (stdin):
//! ```json
//! {
//!   "hook":     "pipeline.review",
//!   "bead_id":  "rosary-abc123",
//!   "repo":     "rosary",
//!   "work_dir": "/path/to/worktree"
//! }
//! ```
//!
//! Output (stdout):
//! ```json
//! { "verdict": "approve", "message": "optional human-readable note" }
//! ```
//!
//! ## HTTP transport
//! Rosary POSTs the same JSON body to the configured URL, reads the same JSON response.
//!
//! # Verdict values
//!
//! | Verdict            | Maps to        | Notes                              |
//! |--------------------|----------------|------------------------------------|
//! | `"pass"`           | `Pass`         | Check succeeded                    |
//! | `"approve"`        | `Pass`         | Review approved (alias for pass)   |
//! | `"skip"`           | `Pass`         | Plugin chose not to run (treated as pass) |
//! | `"fail"`           | `Fail`         | Check failed, pipeline blocks      |
//! | `"request-changes"`| `Partial`      | Soft rejection, needs human review |
//! | anything else      | `Partial`      | Unknown verdict — advisory         |
//!
//! # Hook points
//!
//! | Hook                | When it fires                                   |
//! |---------------------|-------------------------------------------------|
//! | `pipeline.verify`   | End of verify tier chain (after built-in tiers) |
//! | `pipeline.review`   | After ReviewCheck in the verify pipeline        |
//! | `pipeline.triage`   | During reconciler triage phase                  |
//! | `pipeline.close`    | When a bead transitions to done                 |

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;
use std::process::Stdio;

use crate::config::PluginConfig;
use crate::verify::{VerifyResult, VerifyTier};

// ── Context ───────────────────────────────────────────────────────────────────

/// Runtime context rosary provides to every plugin call.
#[derive(Debug, Clone, Serialize)]
pub struct PluginContext {
    pub bead_id: String,
    pub repo: String,
}

impl PluginContext {
    pub fn new(bead_id: impl Into<String>, repo: impl Into<String>) -> Self {
        Self {
            bead_id: bead_id.into(),
            repo: repo.into(),
        }
    }
}

// ── Wire types ────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct HookInput<'a> {
    hook: &'a str,
    bead_id: &'a str,
    repo: &'a str,
    work_dir: &'a str,
}

#[derive(Debug, Deserialize)]
struct HookOutput {
    verdict: String,
    #[serde(default)]
    message: Option<String>,
}

impl HookOutput {
    fn into_verify_result(self, plugin_name: &str) -> VerifyResult {
        match self.verdict.as_str() {
            "pass" | "approve" | "skip" => VerifyResult::Pass,
            "fail" | "reject" => VerifyResult::Fail(
                self.message
                    .unwrap_or_else(|| format!("plugin '{plugin_name}' rejected")),
            ),
            "request-changes" => VerifyResult::Partial(
                self.message
                    .unwrap_or_else(|| format!("plugin '{plugin_name}': request-changes")),
            ),
            other => {
                VerifyResult::Partial(format!("plugin '{plugin_name}': unknown verdict '{other}'"))
            }
        }
    }
}

// ── Transport ─────────────────────────────────────────────────────────────────

fn call_subprocess(command: &[String], input_json: &str) -> Result<String> {
    let (prog, args) = command.split_first().context("plugin command is empty")?;

    let mut child = std::process::Command::new(prog)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit()) // let TUIs render to the terminal
        .spawn()
        .with_context(|| format!("spawning plugin '{prog}'"))?;

    // nosemgrep: blocking-subprocess-in-async — plugin hooks are called from sync VerifyTier::check
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input_json.as_bytes())?;

    let out = child
        .wait_with_output()
        .context("waiting for plugin process")?;

    String::from_utf8(out.stdout).context("plugin stdout is not valid UTF-8")
}

fn call_http(url: &str, input_json: &str) -> Result<String> {
    // nosemgrep: blocking-subprocess-in-async — called from sync VerifyTier::check
    let client = reqwest::blocking::Client::new();
    client
        .post(url)
        .header("content-type", "application/json")
        .body(input_json.to_string())
        .send()
        .with_context(|| format!("calling plugin at {url}"))?
        .text()
        .context("reading plugin HTTP response")
}

fn call_plugin(plugin: &PluginConfig, input: &HookInput<'_>) -> Result<HookOutput> {
    let json = serde_json::to_string(input)?;

    let raw = if !plugin.command.is_empty() {
        call_subprocess(&plugin.command, &json)?
    } else if let Some(url) = &plugin.url {
        call_http(url, &json)?
    } else {
        anyhow::bail!(
            "plugin '{}' has neither command nor url configured",
            plugin.name
        );
    };

    serde_json::from_str(&raw)
        .with_context(|| format!("plugin '{}' returned invalid JSON: {raw}", plugin.name))
}

// ── VerifyTier impl ───────────────────────────────────────────────────────────

/// A verify tier that delegates to an external plugin.
///
/// Constructed by `PluginRegistry::verify_tiers()` and appended to the
/// `Verifier` tier chain so plugin checks run after all built-in checks.
pub struct PluginTier {
    plugin: PluginConfig,
    context: PluginContext,
}

impl PluginTier {
    pub fn new(plugin: PluginConfig, context: PluginContext) -> Self {
        Self { plugin, context }
    }
}

impl VerifyTier for PluginTier {
    fn name(&self) -> &str {
        &self.plugin.name
    }

    fn check(&self, work_dir: &Path) -> Result<VerifyResult> {
        let input = HookInput {
            hook: &self.plugin.hook,
            bead_id: &self.context.bead_id,
            repo: &self.context.repo,
            work_dir: work_dir.to_str().unwrap_or(""),
        };

        let output = match call_plugin(&self.plugin, &input) {
            Ok(o) => o,
            Err(e) => {
                // Fail-open: if the plugin can't be reached, warn and pass.
                eprintln!(
                    "[plugin] warning: '{}' unavailable — {e:#}",
                    self.plugin.name
                );
                return Ok(VerifyResult::Pass);
            }
        };

        Ok(output.into_verify_result(&self.plugin.name))
    }
}

// ── Registry ──────────────────────────────────────────────────────────────────

/// All configured plugins, indexed at startup.
pub struct PluginRegistry {
    plugins: Vec<PluginConfig>,
}

impl PluginRegistry {
    pub fn new(plugins: Vec<PluginConfig>) -> Self {
        Self { plugins }
    }

    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// Build `VerifyTier` boxes for all `pipeline.verify` and `pipeline.review` plugins.
    ///
    /// Call after constructing the base `Verifier` to append plugin tiers:
    /// ```rust,ignore
    /// let mut verifier = Verifier::for_language("rust");
    /// for tier in registry.verify_tiers(context) {
    ///     verifier.push(tier);
    /// }
    /// ```
    pub fn verify_tiers(&self, context: PluginContext) -> Vec<Box<dyn VerifyTier>> {
        self.plugins
            .iter()
            .filter(|p| p.hook == "pipeline.verify" || p.hook == "pipeline.review")
            .map(|p| Box::new(PluginTier::new(p.clone(), context.clone())) as Box<dyn VerifyTier>)
            .collect()
    }

    /// Call all `pipeline.triage` hooks for a bead.
    ///
    /// Returns `Some(reason)` if any plugin says to skip the bead, `None` to proceed.
    pub fn call_triage_hooks(&self, context: &PluginContext) -> Option<String> {
        for plugin in self.plugins.iter().filter(|p| p.hook == "pipeline.triage") {
            let input = HookInput {
                hook: &plugin.hook,
                bead_id: &context.bead_id,
                repo: &context.repo,
                work_dir: "",
            };
            match call_plugin(plugin, &input) {
                Ok(out) => match out.verdict.as_str() {
                    "skip" | "fail" => {
                        let reason = out
                            .message
                            .unwrap_or_else(|| format!("plugin '{}' skipped bead", plugin.name));
                        return Some(reason);
                    }
                    _ => {}
                },
                Err(e) => {
                    eprintln!("[plugin] triage hook '{}' unavailable — {e:#}", plugin.name);
                }
            }
        }
        None
    }

    /// Call all `pipeline.close` hooks when a bead finishes.
    pub fn call_close_hooks(&self, context: &PluginContext) {
        for plugin in self.plugins.iter().filter(|p| p.hook == "pipeline.close") {
            let input = HookInput {
                hook: &plugin.hook,
                bead_id: &context.bead_id,
                repo: &context.repo,
                work_dir: "",
            };
            if let Err(e) = call_plugin(plugin, &input) {
                eprintln!("[plugin] close hook '{}' unavailable — {e:#}", plugin.name);
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_hook_output(verdict: &str, message: Option<&str>) -> HookOutput {
        HookOutput {
            verdict: verdict.to_string(),
            message: message.map(str::to_string),
        }
    }

    #[test]
    fn verdict_pass_variants() {
        for v in ["pass", "approve", "skip"] {
            let r = make_hook_output(v, None).into_verify_result("test");
            assert_eq!(r, VerifyResult::Pass, "verdict '{v}' should map to Pass");
        }
    }

    #[test]
    fn verdict_fail_variants() {
        let r = make_hook_output("fail", Some("broken")).into_verify_result("test");
        assert!(matches!(r, VerifyResult::Fail(_)));

        let r = make_hook_output("reject", None).into_verify_result("test");
        assert!(matches!(r, VerifyResult::Fail(_)));
    }

    #[test]
    fn verdict_partial_variants() {
        let r = make_hook_output("request-changes", Some("nit")).into_verify_result("test");
        assert!(matches!(r, VerifyResult::Partial(_)));
    }

    #[test]
    fn verify_tiers_filters_by_hook() {
        let plugins = vec![
            PluginConfig {
                name: "a".into(),
                hook: "pipeline.verify".into(),
                command: vec!["echo".into()],
                url: None,
            },
            PluginConfig {
                name: "b".into(),
                hook: "pipeline.triage".into(),
                command: vec!["echo".into()],
                url: None,
            },
            PluginConfig {
                name: "c".into(),
                hook: "pipeline.review".into(),
                command: vec!["echo".into()],
                url: None,
            },
        ];
        let registry = PluginRegistry::new(plugins);
        let ctx = PluginContext::new("rosary-abc", "rosary");
        let tiers = registry.verify_tiers(ctx);
        // only pipeline.verify and pipeline.review; not triage
        assert_eq!(tiers.len(), 2);
        assert_eq!(tiers[0].name(), "a");
        assert_eq!(tiers[1].name(), "c");
    }

    #[test]
    fn plugin_tier_fails_open_on_unavailable_command() {
        let plugin = PluginConfig {
            name: "nonexistent".into(),
            hook: "pipeline.verify".into(),
            command: vec!["__rsry_no_such_binary_9x__".into()],
            url: None,
        };
        let tier = PluginTier::new(plugin, PluginContext::new("x", "y"));
        // should not error — should return Pass (fail-open)
        let result = tier.check(Path::new("/tmp")).unwrap();
        assert_eq!(result, VerifyResult::Pass);
    }
}
