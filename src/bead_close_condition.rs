//! Close-condition family, split out of bead.rs (rosary-2643b6): the
//! predicates that decide whether a bead declares a verifiable way to know
//! it's done, and the one honest default (`DEFAULT_PR_MERGE_CLOSE_CONDITION`)
//! applied when it doesn't. Self-contained — `mod verify` is used only by
//! `has_close_condition` and nothing outside this module needs it directly.
//!
//! Re-exported from `bead.rs` (`pub use crate::bead_close_condition::*`) so
//! every existing `crate::bead::X` call site keeps working unchanged.

/// Whether a bead of this type must declare a *close condition* — a verifiable
/// way to know it's done. Mirrors [`Bead::has_verifiable_test_command`]'s
/// exemption: planning/review beads (`epic`/`design`/`research`/`review`)
/// describe work, they don't ship a behavior that can be verified.
pub fn requires_close_condition(issue_type: &str) -> bool {
    matches!(issue_type, "bug" | "feature" | "task" | "chore")
}

/// Whether the given (type, description, test_files) triple carries a close
/// condition. True when the type is exempt, OR the description contains a
/// runnable test/build command, OR test files are declared.
///
/// Enforced at `rsry bead create` (fail-loud) so we can't mint an un-closable
/// bead — the sediment root cause. Complements the `rsry bead close` gate,
/// which checks the description alone once the bead already exists.
pub fn has_close_condition(
    issue_type: &str,
    description: &str,
    test_files: &[String],
    acceptance_criteria: &str,
) -> bool {
    !requires_close_condition(issue_type)
        // Structured close condition — the primary, negation-immune signal.
        || !acceptance_criteria.trim().is_empty()
        || !test_files.is_empty()
        // Legacy/compat: a runnable command in the description. Narrow on
        // purpose (commands don't appear in negations the way an intent word
        // like "close condition" does), so no prose-scraping fragility.
        || verify::looks_like_test_command(description)
}

/// Whether "a linked PR merged" is BY ITSELF sufficient to close this bead —
/// the question `close-merged` (a git/GitHub scan) must answer before acting
/// on the merge signal, since it cannot itself run tests or verify anything
/// beyond "a PR referencing this bead merged" (rosary-c75925, rosary-e0e19f).
///
/// An empty condition, or exactly [`DEFAULT_PR_MERGE_CLOSE_CONDITION`], names
/// the merge itself as the completion signal — accepting it here is not a
/// bypass, it's honoring what the bead already declared. An EXPLICIT,
/// DIFFERENT condition (e.g. `cargo test -p foo`) claims something
/// close-merged cannot verify; auto-closing on it anyway is rosary-e0e19f's
/// bug — a proxy signal (a commit mentioned the id) treated as proof of a
/// property (the declared verification actually ran).
///
/// A strict "refuse unless the description looks like a runnable command"
/// gate was tried and rejected (2026-07-29 handoff, `git stash`: "TOO STRICT
/// — would stop auto-close for 78-94% of live beads"). Re-measured against
/// today's live set: 59% of open gated-type beads still have no
/// `acceptance_criteria` at all (they predate `resolve_acceptance_criteria`'s
/// default-backfill) — the concern holds. This is the narrower gate: refuse
/// only when a condition was explicitly declared and isn't just "a PR
/// merges", not whenever one happens to be absent.
pub fn merge_alone_satisfies_close(acceptance_criteria: &str) -> bool {
    let ac = acceptance_criteria.trim();
    ac.is_empty() || ac == DEFAULT_PR_MERGE_CLOSE_CONDITION
}

/// The default close condition applied at *authoring* time when an
/// implementation bead is created without an explicit one. It's honest, not a
/// placeholder: rosary's GitHub merge webhook already advances a bead when its
/// linked PR merges, so "the PR merged" is a real, observable close signal.
///
/// This is what lets `rsry bead create "title"` stay frictionless while still
/// guaranteeing the ADR-0010 invariant — no bead exists without a declared way
/// to close it. Callers who want something sharper pass `acceptance_criteria`,
/// `test_files`, or a runnable command in the description.
pub const DEFAULT_PR_MERGE_CLOSE_CONDITION: &str = concat!(
    "Resolved when the linked PR merges — rosary's default close signal ",
    "(the GitHub merge webhook advances the bead). ",
    "Set a specific acceptance_criteria to override.",
);

/// Resolve the `acceptance_criteria` to persist for a *newly authored* bead.
///
/// Precedence: an explicit `acceptance_criteria` always wins; otherwise if the
/// bead already carries a close condition another way (exempt type, declared
/// `test_files`, or a runnable command in the description) we store nothing
/// extra; otherwise a gated implementation bead with nothing declared gets the
/// honest [`DEFAULT_PR_MERGE_CLOSE_CONDITION`]. `force` is the deliberate
/// escape hatch — it opts out of the default, minting a condition-less bead
/// that `rsry bead close` will still gate on.
///
/// Guards *authoring* only — `bead_move`/`import` replicate existing beads and
/// must not synthesize a condition.
pub fn resolve_acceptance_criteria(
    issue_type: &str,
    description: &str,
    test_files: &[String],
    acceptance_criteria: &str,
    force: bool,
) -> String {
    if !acceptance_criteria.trim().is_empty() {
        return acceptance_criteria.to_string();
    }
    if force || has_close_condition(issue_type, description, test_files, "") {
        return String::new();
    }
    DEFAULT_PR_MERGE_CLOSE_CONDITION.to_string()
}

/// Fail-loud guard that a bead carries a close condition. Authoring now
/// *defaults* one (see [`resolve_acceptance_criteria`]) rather than rejecting,
/// so this is no longer used at `bead create`; it still guards the **dispatch**
/// path — beads that entered via `import`/`bead_move`/`--force` (which don't
/// synthesize a condition) must not have an agent dispatched against an
/// undefined "done". `force` mirrors `rsry bead close --force`.
pub fn ensure_close_condition(
    issue_type: &str,
    description: &str,
    test_files: &[String],
    acceptance_criteria: &str,
    force: bool,
) -> anyhow::Result<()> {
    if force || has_close_condition(issue_type, description, test_files, acceptance_criteria) {
        return Ok(());
    }
    anyhow::bail!(
        "bead has no close condition — {issue_type} beads must declare how \"done\" is verified,\n\
         so an observation (PR-merge/verify) can actually close them (ADR-0010).\n\
         Set `acceptance_criteria` (a command or a resolution statement), pass\n\
         test_files, or force to override."
    )
}

mod verify {
    /// Heuristic: does this text contain a runnable test/build command?
    /// Recognised: cargo test/check/build, pytest, npm/pnpm/yarn test,
    /// go test, make test, task test, just test.
    pub fn looks_like_test_command(text: &str) -> bool {
        const PATTERNS: &[&str] = &[
            "cargo test",
            "cargo check",
            "cargo build",
            "pytest",
            "npm test",
            "npm run test",
            "pnpm test",
            "yarn test",
            "go test",
            "make test",
            "task test",
            "just test",
        ];
        let lower = text.to_lowercase();
        PATTERNS.iter().any(|p| lower.contains(p))
    }

    #[cfg(test)]
    mod tests {
        use super::looks_like_test_command;

        #[test]
        fn detects_cargo_test() {
            assert!(looks_like_test_command(
                "Run with `cargo test -p rosary verify`"
            ));
        }

        #[test]
        fn detects_pytest_in_inline_block() {
            assert!(looks_like_test_command(
                "Success when: pytest tests/test_x.py passes"
            ));
        }

        #[test]
        fn rejects_plain_prose() {
            assert!(!looks_like_test_command(
                "This bead refactors the docs section and adds an example."
            ));
        }

        #[test]
        fn rejects_empty() {
            assert!(!looks_like_test_command(""));
        }

        #[test]
        fn case_insensitive() {
            assert!(looks_like_test_command("CARGO TEST"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_close_condition_checks_structure_then_falls_back() {
        // No structure, no test files, no command -> no close condition.
        assert!(!has_close_condition("task", "just do the thing", &[], ""));
        // Structured acceptance_criteria satisfies it (the primary signal).
        assert!(has_close_condition(
            "task",
            "just do the thing",
            &[],
            "Resolved when the widget renders."
        ));
        // Compat: a runnable command in the description...
        assert!(has_close_condition(
            "task",
            "implement X; verify with `cargo test -p rosary`",
            &[],
            ""
        ));
        // ...or declared test files.
        assert!(has_close_condition(
            "feature",
            "no command here",
            &["tests/foo.rs".to_string()],
            ""
        ));
    }

    /// rosary-c75925: the gate close-merged uses to decide whether a mere
    /// merge is enough, as opposed to `has_close_condition` (whether a bead
    /// is closeable AT ALL, by any means). Distinct question, distinct test.
    #[test]
    fn merge_alone_satisfies_close_only_for_absent_or_default_condition() {
        assert!(
            merge_alone_satisfies_close(""),
            "no declared condition — a merge is what close-merged observed, honor it"
        );
        assert!(
            merge_alone_satisfies_close("   "),
            "whitespace-only counts as absent"
        );
        assert!(
            merge_alone_satisfies_close(DEFAULT_PR_MERGE_CLOSE_CONDITION),
            "the condition literally IS \"a PR merges\" — a merge satisfies it"
        );
        assert!(
            !merge_alone_satisfies_close("cargo test -p widget must pass"),
            "an explicit, different condition demands verification a merge alone can't provide"
        );
        assert!(
            !merge_alone_satisfies_close("Resolved when the widget renders."),
            "any other explicit resolution statement is equally unverifiable by a mere merge"
        );
    }

    #[test]
    fn close_condition_gate_is_negation_immune() {
        // The whole point of the structured field: prose that *mentions* a close
        // condition (esp. a negation) must NOT satisfy the gate. Substring-
        // matching the description would wrongly pass this.
        assert!(!has_close_condition(
            "task",
            "No runnable close condition here. Acceptance criteria: none yet.",
            &[],
            "" // empty structured field
        ));
    }

    #[test]
    fn resolve_acceptance_criteria_defaults_only_when_needed() {
        // Bare gated impl bead → honest PR-merge default (no double-spacing from
        // the concat! literal).
        let d = resolve_acceptance_criteria("task", "just do it", &[], "", false);
        assert_eq!(d, DEFAULT_PR_MERGE_CLOSE_CONDITION);
        assert!(
            !d.contains("  "),
            "default must not contain double spaces: {d:?}"
        );
        // Explicit wins verbatim.
        assert_eq!(
            resolve_acceptance_criteria("task", "d", &[], "closes when X", false),
            "closes when X"
        );
        // Already-satisfied (command / test_files / exempt) → no synthesized text.
        assert_eq!(
            resolve_acceptance_criteria("task", "run cargo test", &[], "", false),
            ""
        );
        assert_eq!(
            resolve_acceptance_criteria("task", "d", &["t.rs".into()], "", false),
            ""
        );
        assert_eq!(resolve_acceptance_criteria("epic", "d", &[], "", false), "");
        // Force opts out of the default.
        assert_eq!(resolve_acceptance_criteria("task", "d", &[], "", true), "");
    }

    #[test]
    fn planning_types_exempt_from_close_condition() {
        // Planning/review beads describe work; nothing to verify at close.
        for t in ["epic", "design", "research", "review"] {
            assert!(!requires_close_condition(t));
            assert!(has_close_condition(t, "no test command", &[], ""));
        }
    }
}
