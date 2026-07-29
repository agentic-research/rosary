//! Pure `.gitignore` shape classification and editing — no filesystem or git
//! subprocess calls, so it's directly mutation-testable in isolation
//! (`task mutants:gitignore`, rosary-e97360). The I/O side (reading the real
//! file, re-verifying with `git check-ignore`, deciding when to call this at
//! all) lives in `mod hooks` (`src/main.rs`), which owns `fix_gitignore_shadow`.

/// Which recognized `.gitignore` shape is shadowing `.beads/beads.jsonl` —
/// pure classification over content, testable without touching the
/// filesystem or git (rosary-e97360, the fix half of `hooks audit`'s
/// gitignore-shadow check, rosary-b5c8a1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GitignoreShadowShape {
    /// A single top-level rule exactly `.beads` or `.beads/` — safe to
    /// auto-remove, because `.beads/.gitignore` (written by `rsry init` /
    /// `hooks install`) already owns the fine-grained rules: deleting the
    /// blanket rule can only WIDEN what's trackable to exactly what rosary
    /// itself already declared safe. Found live in `lectio`.
    Simple { pattern: &'static str },
    /// A default-deny allowlist (`*` plus `!`-negated exceptions) — a
    /// deliberate security posture. Refuse to guess which negation to add;
    /// suggest the fix and let a human apply it. Found live in `notme.bot`.
    Allowlist,
    /// Shadowed, but not a shape this automation recognizes.
    Unrecognized,
}

pub(crate) fn classify_gitignore_shadow_shape(content: &str) -> GitignoreShadowShape {
    // Checked first, deliberately: the conservative default when a file
    // could arguably match more than one shape is to refuse rather than
    // guess.
    let has_bare_star = content.lines().any(|l| l.trim() == "*");
    let has_negation = content.lines().any(|l| l.trim_start().starts_with('!'));
    if has_bare_star && has_negation {
        return GitignoreShadowShape::Allowlist;
    }
    for pattern in [".beads/", ".beads"] {
        if content.lines().any(|l| l.trim() == pattern) {
            return GitignoreShadowShape::Simple { pattern };
        }
    }
    GitignoreShadowShape::Unrecognized
}

/// Remove the first line trim-equal to `pattern`, preserving everything else
/// verbatim — including surrounding comments. This bead's contract is
/// removing the matching RULE, not guessing which comments describe it;
/// leaving stale prose behind is harmless and honest about what changed.
pub(crate) fn remove_gitignore_line(content: &str, pattern: &str) -> String {
    let mut removed = false;
    let mut out = String::with_capacity(content.len());
    for line in content.lines() {
        if !removed && line.trim() == pattern {
            removed = true;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// The exact two-step allowlist diff a human should apply. gitignore(5)
/// requires un-excluding the parent directory before a child pattern can be
/// re-included, so this can't be flattened to one line — order matters, and
/// getting it subtly wrong is exactly the risk that makes this a suggestion
/// rather than an automatic edit.
pub(crate) fn allowlist_fix_suggestion() -> &'static str {
    "!.beads/\n\
     .beads/*\n\
     !.beads/.gitignore\n\
     !.beads/beads.jsonl\n\
     !.beads/metadata.json\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_gitignore_shadow_shape_detects_simple_with_trailing_slash() {
        let content = "# a comment\n.beads/\n";
        assert_eq!(
            classify_gitignore_shadow_shape(content),
            GitignoreShadowShape::Simple { pattern: ".beads/" }
        );
    }

    #[test]
    fn classify_gitignore_shadow_shape_detects_simple_without_trailing_slash() {
        let content = ".beads\n";
        assert_eq!(
            classify_gitignore_shadow_shape(content),
            GitignoreShadowShape::Simple { pattern: ".beads" }
        );
    }

    #[test]
    fn classify_gitignore_shadow_shape_detects_allowlist() {
        let content = "*\n!.gitignore\n!src/\n";
        assert_eq!(
            classify_gitignore_shadow_shape(content),
            GitignoreShadowShape::Allowlist
        );
    }

    #[test]
    fn classify_gitignore_shadow_shape_prefers_allowlist_when_both_present() {
        // Deliberately conservative: if a file could arguably match both
        // shapes, refuse (Allowlist) rather than guess (Simple).
        let content = "*\n!.gitignore\n.beads/\n";
        assert_eq!(
            classify_gitignore_shadow_shape(content),
            GitignoreShadowShape::Allowlist
        );
    }

    #[test]
    fn classify_gitignore_shadow_shape_unrecognized_for_unrelated_content() {
        let content = "target/\nnode_modules/\n*.log\n";
        assert_eq!(
            classify_gitignore_shadow_shape(content),
            GitignoreShadowShape::Unrecognized
        );
    }

    /// A bare `*` alone (no negations at all) is a real, if unusual,
    /// gitignore — but with nothing to un-exclude anything, it is not the
    /// ALLOWLIST shape this bead automates a suggestion for. Distinguishes
    /// the `&&` in the classifier from a `||` that would over-match.
    #[test]
    fn classify_gitignore_shadow_shape_bare_star_without_negation_is_unrecognized() {
        let content = "*\ntarget/\n";
        assert_eq!(
            classify_gitignore_shadow_shape(content),
            GitignoreShadowShape::Unrecognized
        );
    }

    /// A negation alone (no bare `*` default-deny) is also not the
    /// ALLOWLIST shape — it is an ordinary gitignore with one exception
    /// rule, not a hardened default-deny posture.
    #[test]
    fn classify_gitignore_shadow_shape_negation_without_bare_star_is_unrecognized() {
        let content = "target/\n!target/keep.txt\n";
        assert_eq!(
            classify_gitignore_shadow_shape(content),
            GitignoreShadowShape::Unrecognized
        );
    }

    #[test]
    fn remove_gitignore_line_removes_only_the_matching_line() {
        let content = "# keep me\n.beads/\n# also keep\ntarget/\n";
        let out = remove_gitignore_line(content, ".beads/");
        assert_eq!(out, "# keep me\n# also keep\ntarget/\n");
    }

    #[test]
    fn remove_gitignore_line_removes_only_first_match() {
        // Defensive: if the pattern somehow appears twice, only the first
        // occurrence is removed — never silently eat every line that
        // happens to match.
        let content = ".beads/\nkeep/\n.beads/\n";
        let out = remove_gitignore_line(content, ".beads/");
        assert_eq!(out, "keep/\n.beads/\n");
    }

    #[test]
    fn remove_gitignore_line_noop_when_pattern_absent() {
        let content = "target/\n*.log\n";
        let out = remove_gitignore_line(content, ".beads/");
        assert_eq!(out, content);
    }

    /// Distinguishes trim-equality from raw equality: a pattern with
    /// leading/trailing whitespace on the line must still match and be
    /// removed (real .gitignore files are inconsistently indented).
    #[test]
    fn remove_gitignore_line_matches_pattern_with_surrounding_whitespace() {
        let content = "keep/\n  .beads/  \nkeep2/\n";
        let out = remove_gitignore_line(content, ".beads/");
        assert_eq!(out, "keep/\nkeep2/\n");
    }

    #[test]
    fn allowlist_fix_suggestion_is_the_two_step_pattern() {
        let s = allowlist_fix_suggestion();
        assert!(s.contains("!.beads/"));
        assert!(s.contains(".beads/*"));
        assert!(s.contains("!.beads/beads.jsonl"));
        // Order matters (gitignore(5) parent-exclusion rule): the
        // un-exclude-the-directory line must come before the narrowing
        // re-ignore line.
        assert!(s.find("!.beads/").unwrap() < s.find(".beads/*").unwrap());
    }
}
