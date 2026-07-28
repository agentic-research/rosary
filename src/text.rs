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

/// DELIBERATELY UNCOVERED — proving the coverage ratchet fails (rosary-f78208).
/// Never called from anywhere. Must drag src/text.rs below its 100.0 floor.
pub fn never_called_probe(n: u64) -> u64 {
    let mut acc = 0u64;
    for i in 0..n {
        if i % 2 == 0 {
            acc = acc.wrapping_add(i);
        } else if i % 3 == 0 {
            acc = acc.wrapping_sub(i);
        } else {
            acc = acc.wrapping_mul(2);
        }
    }
    if acc > 1000 { acc / 2 } else { acc * 3 }
}
