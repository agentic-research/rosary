//! `ScopeId` — generic identifier for "where a bead lives" (rosary-b5da2f).
//!
//! Today, every bead-related MCP call takes a `repo_path: &str` that points at
//! a local git checkout. That assumption is load-bearing in several places
//! (per-repo `.beads/` lookup, `LinkageStore` cross-repo dep schema, etc.) and
//! it breaks down for three real use cases:
//!
//! 1. **Cross-repo dependencies** — `rsry_bead_link` with `depends_on=signet-X`
//!    fails to find `signet-X` when the caller passes `repo_path=cloister`
//!    (`rosary-98ee93`). The partial fix in PR #205 auto-detects the prefix;
//!    the full fix is a richer identifier here.
//! 2. **External sources** — zen PR inbox, cloister-hosted MCP tools, etc.
//!    have bead-like work items that aren't tied to a local checkout.
//! 3. **Global / org-level beads** — the incoming triage queue
//!    (`rosary-1db9c9`) is conceptually scope-less; today it has to be filed
//!    in some repo as a placeholder.
//!
//! `ScopeId` is the type that names all three cases. This module is the
//! type + parsing only — no call-sites have been updated yet. Threading it
//! through `BeadStore`, `LinkageStore`, and the MCP handlers ships in later
//! PRs in the rosary-b5da2f series.
//!
//! ## Canonical string forms
//!
//! Designed for unambiguous round-tripping through MCP JSON args, CLI flags,
//! and bead-id prefixes:
//!
//! | Variant            | Canonical form        | Notes                                  |
//! |--------------------|-----------------------|----------------------------------------|
//! | `Repo("rosary")`   | `"repo:rosary"`       | bare `"rosary"` also parses for ergonomics |
//! | `External("uri")`  | `"external:uri"`      | uri is arbitrary; rosary doesn't fetch it |
//! | `Global`           | `"global"`            | no parameters                          |
//!
//! `FromStr` / `Display` are inverse on canonical forms. Path-shaped strings
//! (e.g. `~/remotes/art/rosary`) are NOT accepted by `FromStr` — use
//! [`ScopeId::from_repo_path`] instead so the path-vs-canonical distinction
//! is explicit at the call site.

use std::fmt;
use std::str::FromStr;

/// Where a bead lives. Replaces the `repo_path: &str` parameter pattern
/// across the rosary surface for use cases that aren't naturally a local
/// git checkout (cross-repo deps, external sources, global queues).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ScopeId {
    /// A repo by name (resolved to a local path via config when needed).
    /// The canonical bead-id prefix scheme — `signet-9605a3` lives in
    /// `ScopeId::Repo("signet")`.
    Repo(String),
    /// A non-repo source — zen PR inbox, cloister-hosted MCP, anything
    /// addressable by URI but not a local checkout. The string is the
    /// caller-supplied identifier; rosary stores it but doesn't fetch.
    External(String),
    /// Org-level beads with no specific repo home — the incoming triage
    /// queue (`rosary-1db9c9`) is the canonical use case.
    Global,
}

/// Parse errors are kept narrow so callers can distinguish "bad input"
/// from "valid but unsupported." `FromStr` only ever returns these.
#[derive(Debug, PartialEq, Eq)]
pub enum ScopeParseError {
    /// The input was empty or whitespace-only after trimming.
    Empty,
    /// A `repo:` or `external:` prefix had nothing after it.
    EmptyAfterPrefix(&'static str),
}

impl fmt::Display for ScopeParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "scope id is empty"),
            Self::EmptyAfterPrefix(prefix) => {
                write!(f, "scope id `{prefix}:` has no value after the prefix")
            }
        }
    }
}

impl std::error::Error for ScopeParseError {}

impl ScopeId {
    /// If this scope is a `Repo`, return the repo name. Otherwise None.
    /// Use to gate repo-only code paths (e.g. local Dolt store lookup)
    /// without exhaustive matching on every call site.
    pub fn as_repo_name(&self) -> Option<&str> {
        match self {
            Self::Repo(name) => Some(name),
            _ => None,
        }
    }

    /// Build a `Repo` scope from a filesystem path by taking the basename.
    /// This is the back-compat helper for the existing `repo_path: &str`
    /// MCP arg shape — when the caller passes `~/remotes/art/rosary`, the
    /// scope is `Repo("rosary")`. Trailing slashes are stripped.
    ///
    /// Returns `Repo("unknown")` for paths with no basename (e.g. `"/"`)
    /// so the surface stays infallible at call sites that don't want to
    /// thread a `Result` for what was previously an `&str`. Callers that
    /// need strictness should construct `ScopeId::Repo(...)` directly.
    pub fn from_repo_path(path: &str) -> Self {
        let trimmed = path.trim().trim_end_matches('/');
        let name = std::path::Path::new(trimmed)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        Self::Repo(name)
    }
}

impl fmt::Display for ScopeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Repo(name) => write!(f, "repo:{name}"),
            Self::External(uri) => write!(f, "external:{uri}"),
            Self::Global => write!(f, "global"),
        }
    }
}

impl FromStr for ScopeId {
    type Err = ScopeParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Err(ScopeParseError::Empty);
        }
        if trimmed == "global" {
            return Ok(Self::Global);
        }
        if let Some(rest) = trimmed.strip_prefix("repo:") {
            if rest.is_empty() {
                return Err(ScopeParseError::EmptyAfterPrefix("repo"));
            }
            return Ok(Self::Repo(rest.to_string()));
        }
        if let Some(rest) = trimmed.strip_prefix("external:") {
            if rest.is_empty() {
                return Err(ScopeParseError::EmptyAfterPrefix("external"));
            }
            return Ok(Self::External(rest.to_string()));
        }
        // Ergonomic fall-through: bare names parse as `Repo`. This keeps
        // existing call sites that pass `"rosary"` (vs `"repo:rosary"`)
        // working without churn.
        Ok(Self::Repo(trimmed.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── FromStr — canonical forms ─────────────────────────────────────

    #[test]
    fn parses_canonical_repo_form() {
        let s: ScopeId = "repo:rosary".parse().unwrap();
        assert_eq!(s, ScopeId::Repo("rosary".to_string()));
    }

    #[test]
    fn parses_canonical_external_form() {
        let s: ScopeId = "external:https://github.com/foo/bar#42".parse().unwrap();
        assert_eq!(
            s,
            ScopeId::External("https://github.com/foo/bar#42".to_string())
        );
    }

    #[test]
    fn parses_canonical_global() {
        let s: ScopeId = "global".parse().unwrap();
        assert_eq!(s, ScopeId::Global);
    }

    // ── FromStr — ergonomic fall-through ──────────────────────────────

    #[test]
    fn parses_bare_name_as_repo() {
        // No `repo:` prefix — fall through to Repo("..."). Lets existing
        // callers that pass bare repo names keep working.
        let s: ScopeId = "rosary".parse().unwrap();
        assert_eq!(s, ScopeId::Repo("rosary".to_string()));
    }

    #[test]
    fn parses_bare_hyphenated_repo() {
        // Hyphens in repo names (ley-line, ley-line-open) must survive
        // the bare-name fall-through; the lack of `:` is what
        // disambiguates from the prefixed forms.
        let s: ScopeId = "ley-line-open".parse().unwrap();
        assert_eq!(s, ScopeId::Repo("ley-line-open".to_string()));
    }

    #[test]
    fn parses_strips_surrounding_whitespace() {
        // Defensive: caller copies + pastes with trailing whitespace.
        let s: ScopeId = "  rosary\n".parse().unwrap();
        assert_eq!(s, ScopeId::Repo("rosary".to_string()));
    }

    // ── FromStr — error cases ─────────────────────────────────────────

    #[test]
    fn rejects_empty_string() {
        assert_eq!("".parse::<ScopeId>().unwrap_err(), ScopeParseError::Empty);
    }

    #[test]
    fn rejects_whitespace_only() {
        assert_eq!(
            "   \t\n".parse::<ScopeId>().unwrap_err(),
            ScopeParseError::Empty
        );
    }

    #[test]
    fn rejects_repo_prefix_with_no_value() {
        assert_eq!(
            "repo:".parse::<ScopeId>().unwrap_err(),
            ScopeParseError::EmptyAfterPrefix("repo")
        );
    }

    #[test]
    fn rejects_external_prefix_with_no_value() {
        assert_eq!(
            "external:".parse::<ScopeId>().unwrap_err(),
            ScopeParseError::EmptyAfterPrefix("external")
        );
    }

    // ── Display — canonical forms ─────────────────────────────────────

    #[test]
    fn display_repo_uses_canonical_prefix() {
        assert_eq!(ScopeId::Repo("rosary".into()).to_string(), "repo:rosary");
    }

    #[test]
    fn display_external_uses_canonical_prefix() {
        assert_eq!(
            ScopeId::External("https://example.com/q".into()).to_string(),
            "external:https://example.com/q"
        );
    }

    #[test]
    fn display_global_has_no_prefix() {
        assert_eq!(ScopeId::Global.to_string(), "global");
    }

    // ── Display ↔ FromStr round-trip ─────────────────────────────────

    #[test]
    fn roundtrip_repo() {
        let original = ScopeId::Repo("rosary".into());
        let parsed: ScopeId = original.to_string().parse().unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn roundtrip_external() {
        let original = ScopeId::External("zen://inbox/pr/42".into());
        let parsed: ScopeId = original.to_string().parse().unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn roundtrip_global() {
        let original = ScopeId::Global;
        let parsed: ScopeId = original.to_string().parse().unwrap();
        assert_eq!(parsed, original);
    }

    // ── from_repo_path back-compat helper ─────────────────────────────

    #[test]
    fn from_repo_path_extracts_basename() {
        let s = ScopeId::from_repo_path("/Users/jamesgardner/remotes/art/rosary");
        assert_eq!(s, ScopeId::Repo("rosary".into()));
    }

    #[test]
    fn from_repo_path_strips_trailing_slash() {
        // Many config + shell paths end with `/`. The basename of `foo/`
        // is `foo`, not `""` — without the trim, this would return
        // Repo("unknown").
        let s = ScopeId::from_repo_path("/Users/jamesgardner/remotes/art/rosary/");
        assert_eq!(s, ScopeId::Repo("rosary".into()));
    }

    #[test]
    fn from_repo_path_handles_relative() {
        let s = ScopeId::from_repo_path("art/rosary");
        assert_eq!(s, ScopeId::Repo("rosary".into()));
    }

    #[test]
    fn from_repo_path_handles_basename_only() {
        let s = ScopeId::from_repo_path("rosary");
        assert_eq!(s, ScopeId::Repo("rosary".into()));
    }

    #[test]
    fn from_repo_path_trims_whitespace() {
        let s = ScopeId::from_repo_path("  /tmp/foo  ");
        assert_eq!(s, ScopeId::Repo("foo".into()));
    }

    #[test]
    fn from_repo_path_with_no_basename_returns_unknown() {
        // Edge case: `/` (root) has no basename. `Path::file_name` returns
        // None. We surface "unknown" rather than panic so the call site
        // stays infallible.
        let s = ScopeId::from_repo_path("/");
        assert_eq!(s, ScopeId::Repo("unknown".into()));
    }

    #[test]
    fn from_repo_path_empty_returns_unknown() {
        let s = ScopeId::from_repo_path("");
        assert_eq!(s, ScopeId::Repo("unknown".into()));
    }

    // ── as_repo_name ──────────────────────────────────────────────────

    #[test]
    fn as_repo_name_some_for_repo() {
        assert_eq!(
            ScopeId::Repo("rosary".into()).as_repo_name(),
            Some("rosary")
        );
    }

    #[test]
    fn as_repo_name_none_for_external() {
        assert_eq!(ScopeId::External("x".into()).as_repo_name(), None);
    }

    #[test]
    fn as_repo_name_none_for_global() {
        assert_eq!(ScopeId::Global.as_repo_name(), None);
    }

    // ── ScopeParseError display + Error trait ─────────────────────────

    #[test]
    fn parse_error_empty_display() {
        assert_eq!(ScopeParseError::Empty.to_string(), "scope id is empty");
    }

    #[test]
    fn parse_error_empty_after_prefix_display() {
        assert_eq!(
            ScopeParseError::EmptyAfterPrefix("repo").to_string(),
            "scope id `repo:` has no value after the prefix"
        );
    }

    #[test]
    fn parse_error_is_std_error() {
        // Compiles iff `ScopeParseError: std::error::Error` — pins the
        // trait bound so anyhow / thiserror integration stays clean.
        fn assert_error<E: std::error::Error>(_: &E) {}
        assert_error(&ScopeParseError::Empty);
    }
}
