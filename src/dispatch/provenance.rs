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
    /// The seat's usage/session limit was hit — claude reports this as a
    /// *successful* result ("You've hit your session limit · resets 3:40pm"),
    /// so it is detected from the output, not the exit code. A failure of the
    /// seat, not of the work: nothing to verify, retry after the reset.
    SessionLimit,
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
            Self::SessionLimit => "session-limit",
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

    // 2. Seat limit — claude reports it as a *successful* result, so this is
    //    detected from the text alone and outranks the exit code (rosary-1780ea).
    if t.contains("session limit") || t.contains("usage limit") {
        return FailureClass::SessionLimit;
    }

    // 3. Auth — credential missing or rejected.
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

/// How long until the seat's limit resets, parsed from the limit notice
/// ("… resets 3:40pm", "… will reset at 4pm"). `None` when the text carries no
/// wall-clock reset time — the caller falls back to exponential backoff.
pub fn session_limit_reset(
    text: &str,
    now: chrono::DateTime<chrono::Local>,
) -> Option<std::time::Duration> {
    use chrono::TimeZone;
    let t = text.to_ascii_lowercase();
    let tail = &t[t.find("reset")?..];
    let b = tail.as_bytes();
    let mut i = 0;
    while i < b.len() && !b[i].is_ascii_digit() {
        i += 1;
    }
    let mut j = i;
    while j < b.len() && b[j].is_ascii_digit() {
        j += 1;
    }
    let mut hour: u32 = tail[i..j].parse().ok()?;
    let mut minute: u32 = 0;
    let mut k = j;
    if b.get(k) == Some(&b':') {
        let m0 = k + 1;
        let mut m1 = m0;
        while m1 < b.len() && b[m1].is_ascii_digit() {
            m1 += 1;
        }
        minute = tail[m0..m1].parse().ok()?;
        k = m1;
    }
    while b.get(k) == Some(&b' ') {
        k += 1;
    }
    match &tail[k..] {
        r if r.starts_with("pm") => {
            if hour != 12 {
                hour += 12;
            }
        }
        r if r.starts_with("am") => {
            if hour == 12 {
                hour = 0;
            }
        }
        _ => return None,
    }
    let naive = now.date_naive().and_hms_opt(hour, minute, 0)?;
    let mut target = now.timezone().from_local_datetime(&naive).single()?;
    if target <= now {
        target += chrono::Duration::days(1);
    }
    (target - now).to_std().ok()
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

    /// rosary-1780ea: claude reports a session-limit hit as a *successful*
    /// result ("You've hit your session limit · resets 3:40pm", exit 0). It is
    /// a failure of the seat, not of the work, and must be classified as such
    /// regardless of the exit code so the record is the diagnosis.
    #[test]
    fn session_limit_is_classified_even_on_exit_zero() {
        assert_eq!(
            classify(true, "You've hit your session limit · resets 3:40pm"),
            FailureClass::SessionLimit
        );
        assert_eq!(
            classify(
                false,
                "Claude usage limit reached. Your limit will reset at 4pm"
            ),
            FailureClass::SessionLimit
        );
        assert_eq!(
            FailureClass::SessionLimit.qualify("failure"),
            "failure:session-limit"
        );
    }

    #[test]
    fn session_limit_reset_time_parses_to_a_wait() {
        use chrono::TimeZone;
        let now = chrono::Local
            .with_ymd_and_hms(2026, 9, 12, 15, 22, 0)
            .unwrap();
        // Same day, later: wait until then.
        let wait = session_limit_reset("You've hit your session limit · resets 3:40pm", now)
            .expect("reset time present");
        assert_eq!(wait, std::time::Duration::from_secs(18 * 60));
        // Minutes optional, "at" optional.
        let wait = session_limit_reset("limit will reset at 4pm", now).unwrap();
        assert_eq!(wait, std::time::Duration::from_secs(38 * 60));
        // Already past today: the next occurrence is tomorrow.
        let wait = session_limit_reset("resets 3:00pm", now).unwrap();
        assert_eq!(wait, std::time::Duration::from_secs(23 * 3600 + 38 * 60));
        // No reset time in the message: nothing to key to.
        assert_eq!(
            session_limit_reset("You've hit your session limit", now),
            None
        );
        assert_eq!(session_limit_reset("not logged in", now), None);
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
