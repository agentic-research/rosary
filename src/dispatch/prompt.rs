//! Prompt building and agent definition loading.

use std::path::Path;

/// Prompt version for traceability — agents include this in bead comments
/// so output can be traced back to the prompt configuration that produced it.
pub const PROMPT_VERSION: &str = "v0.2.0";

/// System prompt prepended to all dispatched agents.
/// Tells agents about available MCP tools, workflow expectations,
/// and bead lifecycle management.
const AGENT_SYSTEM_PROMPT: &str = "\
You are a rosary-dispatched agent working on a bead (work item).\n\
\n\
## Available Tools\n\
- **mache MCP** (`mcp__mache__*`): Structural code navigation — \
  find_definition, find_callers, find_callees, search, get_overview. \
  Prefer mache over grep for understanding code structure.\n\
- **rsry MCP** (`mcp__rsry__*`): Bead management — \
  bead_create, bead_close, bead_comment, bead_search, bead_link.\n\
\n\
## Workflow\n\
- Use `task build` / `task test` — never raw `cargo` or `go` commands. \
  The Taskfile runs linters and sets required env vars that raw commands skip.\n\
- Read the relevant code before making claims about it. \
  If you haven't opened a file, don't assert what it contains.\n\
- Make minimal, focused changes.\n\
- Commit with descriptive messages including `bead:ID` reference.\n\
- Do NOT add co-author lines to commits.\n\
\n\
## Bead Lifecycle\n\
Your prompt includes a Bead ID and Repo path. Manage the bead throughout:\n\
1. **Comment progress** via `mcp__rsry__rsry_bead_comment` as you work — \
   not just at the end. Other agents and humans read these for context.\n\
2. **Close** the bead via `mcp__rsry__rsry_bead_close` after tests pass and commit is made.\n\
3. If you cannot fix the issue, comment explaining what you tried and why — do NOT close it.\n\
";

/// Task framing varies by agent perspective so dispatched agents receive
/// role-appropriate instructions rather than a generic "fix this" prompt.
fn task_framing(agent_name: Option<&str>) -> &'static str {
    match agent_name.unwrap_or("dev-agent") {
        "staging-agent" => "Review this change. Verify tests validate real behavior, not mocks.",
        "prod-agent" => {
            "Audit this code for production readiness: resource leaks, error handling, concurrency."
        }
        "feature-agent" => {
            "Check cross-file coherence: dependencies, API contracts, error consistency."
        }
        "architect-agent" => {
            "Analyze this problem. Evaluate approaches, write an ADR, decompose into beads."
        }
        "pm-agent" => {
            "Assess from a strategic perspective: scope, cross-repo overlap, prioritization."
        }
        _ => "Fix this issue. Make the minimal change needed.",
    }
}

/// Build the prompt for a bead.
///
/// Includes the bead ID and repo path so the agent can self-manage its
/// lifecycle via MCP tools (comment, close). When a workspace path is
/// provided, reads the handoff chain for context from previous phases.
///
/// The prompt uses XML tags to separate sections so the model can
/// unambiguously parse task, context, and instructions.
pub fn build_prompt(
    bead: &crate::bead::Bead,
    repo_path: &str,
    workspace: Option<&Path>,
    agent_name: Option<&str>,
) -> String {
    let handoff_context = workspace
        .map(|ws| {
            let chain = crate::handoff::Handoff::read_chain(ws);
            crate::handoff::Handoff::format_for_prompt(&chain)
        })
        .unwrap_or_default();

    // Use workspace path for Repo: line (agent's actual working directory)
    // to prevent agents from resolving absolute paths against the main repo.
    // Keep repo_path for MCP bead tools where .beads/ lives.
    let work_repo = workspace
        .map(|ws| ws.display().to_string())
        .unwrap_or_else(|| repo_path.to_string());

    let framing = task_framing(agent_name);

    let handoff_section = if handoff_context.is_empty() {
        String::new()
    } else {
        format!("\n<handoff>\n{handoff_context}</handoff>\n")
    };

    format!(
        "<task>\n\
         {framing}\n\
         </task>\n\
         \n\
         <bead>\n\
         Bead ID: {bead_id}\n\
         Repo: {work_repo}\n\
         Title: {title}\n\
         Description: {desc}\n\
         </bead>\n\
         {handoff}\
         \n\
         <instructions>\n\
         After completing your work:\n\
         1. Run tests via `task test`\n\
         2. Commit your changes (git add + git commit with bead:{bead_id} in message)\n\
         3. Close this bead: call mcp__rsry__rsry_bead_close with repo_path=\"{bead_repo}\" and id=\"{bead_id}\"\n\
         4. Report what you changed\n\
         </instructions>",
        bead_id = bead.id,
        bead_repo = repo_path,
        title = bead.title,
        desc = bead.description,
        handoff = handoff_section,
    )
}

/// Strip YAML frontmatter from a markdown file.
/// Frontmatter is delimited by `---` on its own line at the start.
pub fn strip_frontmatter(content: &str) -> String {
    if !content.starts_with("---") {
        return content.to_string();
    }
    // Find the closing "---" after the opening one
    if let Some(end) = content[3..].find("\n---") {
        let after = 3 + end + 4; // 3 for "---", end for body, 4 for "\n---"
        content[after..].trim_start_matches('\n').to_string()
    } else {
        content.to_string()
    }
}

/// Load an agent definition from its markdown file.
///
/// Reads `{agents_dir}/{agent_name}.md`, strips YAML frontmatter,
/// and returns the markdown body.
pub fn load_agent_prompt(agents_dir: &Path, agent_name: &str) -> Option<String> {
    let file_name = if agent_name.ends_with(".md") {
        agent_name.to_string()
    } else {
        format!("{agent_name}.md")
    };
    let path = agents_dir.join(&file_name);
    let content = std::fs::read_to_string(&path).ok()?;
    Some(strip_frontmatter(&content))
}

/// Load GOLDEN_RULES.md from the agents/rules/ directory.
fn load_golden_rules(agents_dir: &Path) -> Option<String> {
    let path = agents_dir.join("rules").join("GOLDEN_RULES.md");
    std::fs::read_to_string(&path).ok()
}

/// Build the complete system prompt for an agent dispatch.
///
/// Layers:
/// 1. Base AGENT_SYSTEM_PROMPT (MCP tools, workflow, bead lifecycle)
/// 2. GOLDEN_RULES.md (if agents_dir provided)
/// 3. Agent-specific definition (if agent_name set and file exists)
///
/// Falls back gracefully — missing files produce warnings, not errors.
pub fn build_system_prompt(agent_name: Option<&str>, agents_dir: Option<&Path>) -> String {
    let mut parts = vec![format!(
        "Prompt version: {PROMPT_VERSION}\n\n{AGENT_SYSTEM_PROMPT}"
    )];

    if let Some(dir) = agents_dir {
        if let Some(rules) = load_golden_rules(dir) {
            parts.push(format!("\n## Golden Rules\n\n{rules}"));
        } else {
            eprintln!(
                "[dispatch] warning: GOLDEN_RULES.md not found in {}",
                dir.display()
            );
        }

        if let Some(name) = agent_name {
            if let Some(agent_prompt) = load_agent_prompt(dir, name) {
                parts.push(format!("\n## Agent Perspective\n\n{agent_prompt}"));
                eprintln!("[dispatch] loaded agent definition: {name}");
            } else {
                eprintln!("[dispatch] warning: agent definition not found: {name}");
            }
        }
    }

    parts.join("\n")
}
