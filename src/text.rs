//! Small shared text helpers for human-facing output.
//!
//! Extracted because `truncate` was independently written in both `graph.rs`
//! and `bead_diff.rs` and mache's `duplicate_definitions` rule caught it. One
//! definition, one set of tests — in particular the multi-byte case, which a
//! byte-slicing implementation gets wrong by panicking.

/// Truncate to at most `max` CHARACTERS, appending `…` when shortened.
///
/// Char-based, not byte-based: `&s[..max]` panics on a multi-byte boundary,
/// and bead titles carry em-dashes and non-ASCII routinely.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_short_strings_alone() {
        assert_eq!(truncate("abc", 10), "abc");
        assert_eq!(truncate("abc", 3), "abc", "exactly at the limit is not cut");
    }

    #[test]
    fn appends_ellipsis_when_shortened() {
        assert_eq!(truncate("abcdef", 4), "abc…");
    }

    /// Byte-slicing here would panic mid-codepoint.
    #[test]
    fn is_char_safe_on_multibyte() {
        assert_eq!(truncate("ααααα", 3), "αα…");
        assert_eq!(truncate("日本語テキスト", 4), "日本語…");
    }

    #[test]
    fn handles_degenerate_limits() {
        assert_eq!(truncate("abc", 0), "…");
        assert_eq!(truncate("", 5), "");
    }
}
