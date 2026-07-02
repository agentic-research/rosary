//! Rosary-owned **bead operation core** — the `API` layer in CLI ↔ API ↔ MCP.
//!
//! The authoring/mutation logic for a bead op lives here ONCE. The CLI
//! (`main.rs`, HCI: clap flags + prose) and the MCP tools (`serve/handlers.rs`,
//! ACI: JSON args) are thin adapters that parse their own surface into these
//! typed args and call this core. Validation is therefore enforced 1:1 *by
//! construction*, not by remembering to call a guard on both surfaces.
//!
//! This is also the shape rosary *owns* as its op contract: a future `$Op`
//! Cap'n Proto schema for `bead_create` is a projection of [`BeadCreateArgs`],
//! and downstream consumers (cloister, agents) import that contract rather than
//! redeclaring it.

use crate::store::BeadStore;

/// Max byte length for a comment body (and bead description). Shared by every
/// surface so the limit is defined once, not per-handler.
pub const BODY_MAX_LEN: usize = 50_000;

/// Validate a comment body — the single gate shared by CLI (`rsry bead comment
/// add`) and MCP (`rsry_bead_comment`). Rejects blank and over-long bodies on
/// both surfaces (previously only MCP enforced this; the CLI accepted anything).
pub fn validate_comment_body(body: &str) -> anyhow::Result<()> {
    if body.trim().is_empty() {
        anyhow::bail!("body must not be blank");
    }
    if body.len() > BODY_MAX_LEN {
        anyhow::bail!("body exceeds {BODY_MAX_LEN} bytes (got {})", body.len());
    }
    Ok(())
}

/// Typed inputs for authoring a new bead. Both front-ends build this; the core
/// validates + creates. `owner`/`created_by`/`id` are resolved by the caller
/// (they differ by surface — CLI uses the repo name + git config; MCP resolves
/// a prefix + user scope), so this struct holds only the surface-neutral inputs.
#[derive(Debug, Clone)]
pub struct BeadCreateArgs {
    pub title: String,
    pub description: String,
    pub priority: u8,
    pub issue_type: String,
    /// Explicit owner/assignee; defaults to the issue-type's default agent.
    pub owner: Option<String>,
    pub files: Vec<String>,
    pub test_files: Vec<String>,
    pub depends_on: Vec<String>,
    /// Structured close condition — how "done" is verified (a command or a
    /// resolution statement). The gate checks this field's presence, so it can't
    /// be fooled by prose. Empty falls back to test_files / a command in the
    /// description for legacy compatibility.
    pub acceptance_criteria: String,
    /// Escape hatch for the close-condition gate (planning/legacy beads).
    pub force: bool,
}

impl BeadCreateArgs {
    /// The single authoring gate — shared by CLI + MCP. Files-required and
    /// close-condition are enforced here, so neither surface can bypass them.
    pub fn validate(&self) -> anyhow::Result<()> {
        if crate::bead::requires_files(&self.issue_type) && self.files.is_empty() {
            anyhow::bail!(
                "files required for {} beads — specify which code this bead touches",
                self.issue_type
            );
        }
        crate::bead::ensure_close_condition(
            &self.issue_type,
            &self.description,
            &self.test_files,
            &self.acceptance_criteria,
            self.force,
        )
    }

    /// The resolved owner (explicit, or the issue-type default agent).
    pub fn resolved_owner(&self) -> String {
        self.owner
            .clone()
            .unwrap_or_else(|| crate::dispatch::default_agent(&self.issue_type).to_string())
    }
}

/// Create a new bead: validate, resolve owner, persist. The single create
/// chokepoint for the *authoring* intent (distinct from `bead_move`/`import`,
/// which relocate/replicate existing beads and must not re-validate "done").
///
/// Generic over `?Sized` so callers can pass either a concrete store or a
/// `&dyn BeadStore`.
pub async fn create_bead<S: BeadStore + ?Sized>(
    store: &S,
    id: &str,
    args: &BeadCreateArgs,
    created_by: Option<&str>,
) -> anyhow::Result<()> {
    args.validate()?;
    let owner = args.resolved_owner();
    store
        .create_bead_full(
            id,
            &args.title,
            &args.description,
            args.priority,
            &args.issue_type,
            &owner,
            &args.files,
            &args.test_files,
            &args.depends_on,
            created_by,
            "",
            &[],
            &args.acceptance_criteria,
        )
        .await
}

/// Close a bead through the shared CLI/MCP gate. Implementation beads must
/// carry a runnable verification command unless the caller explicitly forces
/// the close for legacy/planning recovery.
pub async fn close_bead<S: BeadStore + ?Sized>(
    store: &S,
    id: &str,
    repo_name: &str,
    force: bool,
) -> anyhow::Result<()> {
    if !force {
        let beads = store.list_beads(repo_name).await?;
        let short_id_suffix = format!("-{id}");
        if let Some(bead) = beads
            .iter()
            .find(|b| b.id == id || b.id.ends_with(&short_id_suffix))
            && !bead.has_verifiable_test_command()
        {
            anyhow::bail!(
                "bead {} ({}) has no verifiable test command in its description.\n\
                 Add e.g. `cargo test -p <crate>` to success criteria, or pass --force/force to override.",
                bead.id,
                bead.issue_type
            );
        }
    }
    store.close_bead(id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(
        issue_type: &str,
        description: &str,
        files: &[&str],
        test_files: &[&str],
    ) -> BeadCreateArgs {
        BeadCreateArgs {
            title: "T".into(),
            description: description.into(),
            priority: 2,
            issue_type: issue_type.into(),
            owner: None,
            files: files.iter().map(|s| s.to_string()).collect(),
            test_files: test_files.iter().map(|s| s.to_string()).collect(),
            depends_on: vec![],
            acceptance_criteria: String::new(),
            force: false,
        }
    }

    #[test]
    fn validate_requires_files_for_impl_types() {
        let err = args("task", "verify: cargo test", &[], &[])
            .validate()
            .unwrap_err();
        assert!(err.to_string().contains("files required"), "{err}");
    }

    #[test]
    fn validate_requires_close_condition() {
        let err = args("task", "just do it", &["a.rs"], &[])
            .validate()
            .unwrap_err();
        assert!(err.to_string().contains("no close condition"), "{err}");
    }

    #[test]
    fn validate_passes_with_files_and_test_command() {
        args("task", "verify: cargo test", &["a.rs"], &[])
            .validate()
            .unwrap();
    }

    #[test]
    fn comment_body_rejects_blank_and_overlong_accepts_normal() {
        assert!(validate_comment_body("").is_err());
        assert!(validate_comment_body("   \n ").is_err());
        assert!(validate_comment_body(&"a".repeat(BODY_MAX_LEN + 1)).is_err());
        validate_comment_body("a real comment").unwrap();
    }

    #[test]
    fn resolved_owner_defaults_to_agent_then_honors_explicit() {
        let mut a = args("bug", "cargo test", &["a.rs"], &[]);
        assert_eq!(a.resolved_owner(), crate::dispatch::default_agent("bug"));
        a.owner = Some("staging-agent".into());
        assert_eq!(a.resolved_owner(), "staging-agent");
    }
}
