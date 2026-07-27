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
    /// Which tier this bead belongs to (ADR-0022: **location derives from
    /// role**). `Canonical` writes to the repo's bead store and thus into the
    /// git-tracked record; `Coordination` writes to `refs/agents/*` and never
    /// touches the working tree.
    ///
    /// Defaults to `Canonical`, so every existing caller keeps its behaviour.
    /// This is deliberately NOT an `Option` — ADR-0022's discipline is that a
    /// role always exists; a bead with no declared role is just a canonical
    /// one, not an unknown one.
    pub role: crate::bead_genesis::Role,
}

impl BeadCreateArgs {
    /// The single authoring gate — shared by CLI + MCP. Enforces files-required
    /// here so neither surface can bypass it. The close-condition rule is no
    /// longer a *rejection*: [`create_bead`] resolves a default one (see
    /// [`crate::bead::resolve_acceptance_criteria`]) so every bead is closeable
    /// without breaking bare `rsry bead create "title"`.
    pub fn validate(&self) -> anyhow::Result<()> {
        if crate::bead::requires_files(&self.issue_type) && self.files.is_empty() {
            anyhow::bail!(
                "files required for {} beads — specify which code this bead touches",
                self.issue_type
            );
        }
        Ok(())
    }

    /// The `acceptance_criteria` this bead will actually be stored with —
    /// explicit if given, else the honest PR-merge default for gated types.
    pub fn resolved_acceptance_criteria(&self) -> String {
        crate::bead::resolve_acceptance_criteria(
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

/// Parse a role token from a surface (CLI flag / MCP arg).
///
/// Fails loud on an unknown value rather than defaulting: silently treating
/// `--role coordinaton` as canonical would write a bead into the git-tracked
/// record that the caller explicitly asked to keep out of it.
pub fn parse_role(s: &str) -> anyhow::Result<crate::bead_genesis::Role> {
    use crate::bead_genesis::Role;
    match s.trim().to_ascii_lowercase().as_str() {
        "canonical" => Ok(Role::Canonical),
        "coordination" => Ok(Role::Coordination),
        "personal" => Ok(Role::Personal),
        other => anyhow::bail!("unknown role `{other}` (expected canonical|coordination)"),
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
    repo_root: &std::path::Path,
    id: &str,
    args: &BeadCreateArgs,
    created_by: Option<&str>,
) -> anyhow::Result<()> {
    args.validate()?;

    // ADR-0022: location derives from role. A coordination bead never reaches
    // the bead store, so it never reaches the git-tracked record and cannot be
    // swept into a code commit by the pre-commit export. This is the routing
    // that makes the coordination home real rather than merely available.
    if args.role == crate::bead_genesis::Role::Coordination {
        let record = serde_json::json!({
            "id": id,
            "role": "coordination",
            "title": args.title,
            "description": args.description,
            "priority": args.priority,
            "issue_type": args.issue_type,
            "owner": args.resolved_owner(),
            "files": args.files,
            "test_files": args.test_files,
            "acceptance_criteria": args.resolved_acceptance_criteria(),
            "created_by": created_by,
        });
        return crate::coordination::append(repo_root, id, &record.to_string());
    }

    // ADR-0012 + ADR-0022: personal beads live in `~/.rsry/personal.db`,
    // outside any project repo — so they never reach a repo store, its tracked
    // JSONL, or its working tree. Encryption + the signet attestation gate are
    // the SYNC layer (rosary-e52b24 / rosary-e55ec9), not the store.
    if args.role == crate::bead_genesis::Role::Personal {
        let personal = crate::personal::open()?;
        return write_to_store(&personal, id, args, created_by).await;
    }

    write_to_store(store, id, args, created_by).await
}

/// The store write itself, shared by the canonical and personal tiers — they
/// differ in WHICH store, not in what is written.
async fn write_to_store<S: BeadStore + ?Sized>(
    store: &S,
    id: &str,
    args: &BeadCreateArgs,
    created_by: Option<&str>,
) -> anyhow::Result<()> {
    store
        .create_bead_full(crate::store::NewBead {
            id: id.to_string(),
            title: args.title.clone(),
            description: args.description.clone(),
            priority: args.priority,
            issue_type: args.issue_type.clone(),
            owner: args.resolved_owner(),
            files: args.files.clone(),
            test_files: args.test_files.clone(),
            depends_on: args.depends_on.clone(),
            created_by: created_by.map(str::to_string),
            acceptance_criteria: args.resolved_acceptance_criteria(),
            ..Default::default()
        })
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
mod role_routing_tests {
    use super::*;
    use crate::bead_genesis::Role;

    fn args(role: Role) -> BeadCreateArgs {
        BeadCreateArgs {
            title: "t".into(),
            description: "d".into(),
            priority: 2,
            issue_type: "task".into(),
            owner: None,
            files: vec!["src/x.rs".into()],
            test_files: vec![],
            depends_on: vec![],
            acceptance_criteria: "cargo test".into(),
            force: false,
            role,
        }
    }

    fn git_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["init", "--quiet"])
            .output()
            .unwrap();
        tmp
    }

    /// ADR-0022's whole point: a coordination bead must never reach the store,
    /// because the store is what the pre-commit export sweeps into the
    /// git-tracked record. Asserted against the store itself, not a file hash,
    /// so it holds regardless of what the publish step does downstream.
    #[tokio::test]
    async fn coordination_role_never_reaches_the_bead_store() {
        let tmp = git_repo();
        let store =
            crate::bead_sqlite::SqliteBeadStore::connect(std::path::Path::new(":memory:")).unwrap();

        create_bead(
            &store,
            tmp.path(),
            "x-coord",
            &args(Role::Coordination),
            None,
        )
        .await
        .unwrap();

        assert!(
            store.list_beads("r").await.unwrap().is_empty(),
            "a coordination bead must not be written to the canonical store"
        );
        let landed = crate::coordination::read(tmp.path(), "x-coord").unwrap();
        assert!(landed.is_some(), "it must land in refs/agents instead");
        assert!(landed.unwrap().contains("\"role\":\"coordination\""));
    }

    /// The default path must be untouched — every existing caller is canonical.
    #[tokio::test]
    async fn canonical_role_still_reaches_the_store() {
        let tmp = git_repo();
        let store =
            crate::bead_sqlite::SqliteBeadStore::connect(std::path::Path::new(":memory:")).unwrap();

        create_bead(&store, tmp.path(), "x-canon", &args(Role::Canonical), None)
            .await
            .unwrap();

        assert_eq!(store.list_beads("r").await.unwrap().len(), 1);
        assert!(
            crate::coordination::read(tmp.path(), "x-canon")
                .unwrap()
                .is_none(),
            "a canonical bead must not leak into the coordination namespace"
        );
    }

    /// ADR-0022's goal: canonical storage absorbs NONE of the other roles.
    /// Personal beads land in ~/.rsry/personal.db, outside any project repo.
    #[tokio::test]
    async fn personal_role_never_reaches_the_repo_store() {
        let _g = crate::personal::tests::HomeGuard::new();
        let tmp = git_repo();
        let repo_store =
            crate::bead_sqlite::SqliteBeadStore::connect(std::path::Path::new(":memory:")).unwrap();

        create_bead(
            &repo_store,
            tmp.path(),
            "x-personal",
            &args(Role::Personal),
            None,
        )
        .await
        .unwrap();

        assert!(
            repo_store.list_beads("r").await.unwrap().is_empty(),
            "a personal bead must not be written to a project repo's store"
        );
        assert!(
            crate::coordination::read(tmp.path(), "x-personal")
                .unwrap()
                .is_none(),
            "and must not leak into the coordination namespace either"
        );
        let personal = crate::personal::open().unwrap();
        let got = personal
            .get_bead("x-personal", crate::personal::SCOPE)
            .await
            .unwrap();
        assert!(got.is_some(), "it must land in the personal store");
    }

    /// An unknown or not-yet-buildable role must FAIL, never silently fall back
    /// to canonical — that would file a bead into the git-tracked record that
    /// the caller explicitly asked to keep out of it.
    #[test]
    fn unknown_and_unbuilt_roles_are_refused() {
        assert_eq!(parse_role("canonical").unwrap(), Role::Canonical);
        assert_eq!(parse_role("  Coordination ").unwrap(), Role::Coordination);
        assert_eq!(parse_role("personal").unwrap(), Role::Personal);
        assert!(parse_role("cordination").is_err(), "typo must not pass");
        assert!(parse_role("").is_err());
    }
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
            role: crate::bead_genesis::Role::Canonical,
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
    fn missing_close_condition_defaults_to_pr_merge_not_rejected() {
        // Authoring no longer rejects a condition-less impl bead — it defaults
        // one so bare `rsry bead create` stays frictionless while the bead is
        // still closeable (ADR-0010 invariant preserved).
        let a = args("task", "just do it", &["a.rs"], &[]);
        a.validate().unwrap();
        let ac = a.resolved_acceptance_criteria();
        assert!(
            ac.contains("PR merges"),
            "expected the PR-merge default, got {ac:?}"
        );
    }

    #[test]
    fn explicit_and_command_conditions_suppress_the_default() {
        // A runnable command in the description already IS a close condition, so
        // no default is synthesized.
        assert_eq!(
            args("task", "verify: cargo test", &["a.rs"], &[]).resolved_acceptance_criteria(),
            ""
        );
        // An explicit acceptance_criteria always wins verbatim.
        let mut a = args("task", "just do it", &["a.rs"], &[]);
        a.acceptance_criteria = "closes when X".into();
        assert_eq!(a.resolved_acceptance_criteria(), "closes when X");
    }

    #[test]
    fn force_opts_out_of_the_default() {
        let mut a = args("task", "just do it", &["a.rs"], &[]);
        a.force = true;
        assert_eq!(a.resolved_acceptance_criteria(), "");
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
