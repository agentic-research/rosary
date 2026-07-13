//! Dispatch provenance: turn a raw dispatch failure into a *classified*,
//! self-describing outcome — so a failure record is the diagnosis, not a prompt
//! to go re-run and inspect.
//!
//! The root gap behind three dispatch bugs this substrate keeps hitting is that
//! a dispatch never recorded *why* it ended:
//! - auth ("Not logged in", `rosary-b1495c`) — the fork landed outside the
//!   credential context and died, leaving only an absence;
//! - silent death (`rosary-82caac`) — a corpse is an outcome nobody wrote;
//! - ACP adapter version-skew — `claude-agent-acp` rejecting a *valid current*
//!   Claude Code permission mode (`permissions.defaultMode: auto`), which reads
//!   as "bad config" but is actually the adapter lagging the harness.
//!
//! Each is the same defect: the failure carried no provenance, so diagnosis
//! meant re-running. This module reads the agent's stderr tail and maps known
//! failure signatures to a [`FailureClass`], recorded on the dispatch outcome
//! (e.g. `failure:auth`). The classifier is intentionally pure — the wiring
//! (which stderr file, which record) lives at the call site.

use std::fmt;

/// A classified dispatch outcome. [`Ok`](FailureClass::Ok) is success; the rest
/// name *why* a dispatch failed, so the recorded outcome diagnoses itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// The agent ran and the action succeeded.
    Ok,
    /// Credential missing or rejected — the harness never authenticated.
    /// e.g. "Not logged in", "Invalid API key", "401 Unauthorized".
    Auth,
    /// Harness/adapter version-skew — a *valid current* input rejected by a
    /// lagging adapter. e.g. `claude-agent-acp` rejecting
    /// `permissions.defaultMode: auto`. Distinct from [`Config`](Self::Config):
    /// the input is correct; the harness is behind.
    Skew,
    /// A genuine bad-configuration error (the input itself is wrong).
    Config,
    /// The harness binary was not found or not executable — nothing ran.
    MissingBinary,
    /// The dispatch exceeded its deadline.
    Timeout,
    /// The agent ran and failed, but the reason isn't classifiable from output.
    Unknown,
}

impl FailureClass {
    /// A stable, lowercase slug for the outcome record (e.g. `missing-binary`).
    pub fn slug(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Auth => "auth",
            Self::Skew => "skew",
            Self::Config => "config",
            Self::MissingBinary => "missing-binary",
            Self::Timeout => "timeout",
            Self::Unknown => "unknown",
        }
    }

    /// Qualify a base outcome (`failure`, `deadletter`) with this class, e.g.
    /// `failure:auth`. [`Ok`](Self::Ok) and [`Unknown`](Self::Unknown) add no
    /// suffix — the former is not a failure, the latter carries no signal worth
    /// appending — so existing `success`/`failure` records are unchanged when
    /// there is nothing to say.
    pub fn qualify(self, base: &str) -> String {
        match self {
            Self::Ok | Self::Unknown => base.to_string(),
            other => format!("{base}:{}", other.slug()),
        }
    }
}

impl fmt::Display for FailureClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.slug())
    }
}

/// Classify a dispatch outcome from its exit signal and the tail of the agent's
/// stderr. Signatures are matched most-upstream-cause first: a missing binary
/// means nothing else ran, so it wins over an auth string that a shell might
/// also print. Matching is case-insensitive and substring-based, tolerant of
/// the surrounding log noise a real stderr tail carries.
pub fn classify(exit_success: bool, stderr_tail: &str) -> FailureClass {
    let t = stderr_tail.to_ascii_lowercase();

    // 1. Binary/spawn failure — nothing ran, so this outranks everything.
    if t.contains("no such file or directory")
        || t.contains("command not found")
        || t.contains("executable file not found")
        || t.contains("os error 2")
    {
        return FailureClass::MissingBinary;
    }

    // 2. Auth — credential missing or rejected.
    if t.contains("not logged in")
        || t.contains("invalid api key")
        || t.contains("authentication_error")
        || t.contains("unauthorized")
        || t.contains("401")
        || t.contains("oauth token")
        || t.contains("please run `claude login`")
        || t.contains("please log in")
    {
        return FailureClass::Auth;
    }

    // 3. Skew — a valid current input rejected by a lagging adapter. Kept to
    //    specific signatures so it never swallows a genuine `Config` error.
    if t.contains("invalid permissions.defaultmode")
        || t.contains("unknown permission mode")
        || t.contains("unrecognized permission mode")
        || t.contains("unsupported protocol version")
    {
        return FailureClass::Skew;
    }

    // 4. Config — the input itself is wrong.
    if t.contains("failed to parse config")
        || t.contains("invalid config")
        || t.contains("unknown setting")
        || t.contains("invalid argument")
    {
        return FailureClass::Config;
    }

    // 5. Timeout — deadline exceeded.
    if t.contains("timed out") || t.contains("timeout") || t.contains("deadline exceeded") {
        return FailureClass::Timeout;
    }

    if exit_success {
        FailureClass::Ok
    } else {
        FailureClass::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_is_ok() {
        assert_eq!(classify(true, ""), FailureClass::Ok);
        assert_eq!(
            classify(true, "some benign trailing log line"),
            FailureClass::Ok
        );
    }

    #[test]
    fn not_logged_in_is_auth() {
        // The exact b1495c signature.
        assert_eq!(
            classify(false, "Error: Not logged in. Please run `claude login`."),
            FailureClass::Auth
        );
    }

    #[test]
    fn api_key_and_401_are_auth() {
        assert_eq!(
            classify(false, "Invalid API key provided"),
            FailureClass::Auth
        );
        assert_eq!(
            classify(false, "API Error: 401 Unauthorized"),
            FailureClass::Auth
        );
    }

    #[test]
    fn invalid_default_mode_is_skew_not_config() {
        // The ACP adapter rejecting a *valid current* mode (`auto`). This must
        // classify as Skew, not Config — the config is correct; the adapter lags.
        assert_eq!(
            classify(false, "-32603 Invalid permissions.defaultMode: auto."),
            FailureClass::Skew
        );
    }

    #[test]
    fn missing_binary_outranks_other_signatures() {
        assert_eq!(
            classify(false, "codex: command not found"),
            FailureClass::MissingBinary
        );
        assert_eq!(
            classify(
                false,
                "error spawning claude: No such file or directory (os error 2)"
            ),
            FailureClass::MissingBinary
        );
        // Even if an auth-shaped word also appears, a spawn failure wins.
        assert_eq!(
            classify(false, "command not found — later: not logged in"),
            FailureClass::MissingBinary
        );
    }

    #[test]
    fn genuine_config_error_is_config() {
        assert_eq!(
            classify(false, "failed to parse config: unexpected token"),
            FailureClass::Config
        );
    }

    #[test]
    fn timeout_is_timeout() {
        assert_eq!(
            classify(false, "operation timed out after 300s"),
            FailureClass::Timeout
        );
        assert_eq!(
            classify(false, "rpc deadline exceeded"),
            FailureClass::Timeout
        );
    }

    #[test]
    fn unclassified_failure_is_unknown() {
        assert_eq!(
            classify(false, "thread 'main' panicked at src/foo.rs:42"),
            FailureClass::Unknown
        );
    }

    #[test]
    fn qualify_composes_failure_with_class() {
        assert_eq!(FailureClass::Auth.qualify("failure"), "failure:auth");
        assert_eq!(FailureClass::Skew.qualify("deadletter"), "deadletter:skew");
        assert_eq!(
            FailureClass::MissingBinary.qualify("failure"),
            "failure:missing-binary"
        );
    }

    #[test]
    fn qualify_leaves_bare_base_when_nothing_to_say() {
        // Ok is not a failure; Unknown carries no signal — neither adds a suffix,
        // so existing `success`/`failure` records are unchanged.
        assert_eq!(FailureClass::Ok.qualify("success"), "success");
        assert_eq!(FailureClass::Unknown.qualify("failure"), "failure");
    }
}
