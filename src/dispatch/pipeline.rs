//! Agent pipeline — phase progression and agent directory resolution.

use std::path::PathBuf;

use crate::scanner::expand_path;

/// The agent pipeline for a given issue type.
pub fn agent_pipeline(issue_type: &str) -> &'static [&'static str] {
    match issue_type {
        "bug" => &["dev-agent", "staging-agent"],
        "feature" => &["dev-agent", "staging-agent", "prod-agent"],
        "task" | "chore" => &["dev-agent"],
        "review" => &["staging-agent"],
        "design" | "research" => &["architect-agent"],
        "epic" => &["pm-agent"],
        _ => &["dev-agent"],
    }
}

/// The default (first) agent for a given issue type.
pub fn default_agent(issue_type: &str) -> &'static str {
    agent_pipeline(issue_type)
        .first()
        .copied()
        .unwrap_or("dev-agent")
}

/// The next agent in the pipeline after `current`, or None if done.
pub fn next_agent(issue_type: &str, current: &str) -> Option<&'static str> {
    let pipeline = agent_pipeline(issue_type);
    let idx = pipeline.iter().position(|&a| a == current)?;
    pipeline.get(idx + 1).copied()
}

/// Resolve agents_dir from config by finding the self-managed repo.
pub fn resolve_agents_dir() -> Option<PathBuf> {
    let cfg = crate::config::load_global().ok()?;
    cfg.repo
        .iter()
        .find(|r| r.self_managed)
        .map(|r| expand_path(&r.path).join("agents"))
        .filter(|p| p.exists())
}
