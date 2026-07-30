//! JSONL wire format — the one place that decides how records become a file.
//!
//! Both bead exporters ([`crate::import::export_beads_contract_jsonl`] and
//! [`crate::jsonl_sync`]) render a `Vec<String>` of serialized records and then
//! have to turn it into file bytes. That last step is the whole of this module,
//! because getting it wrong is silent — the file still parses either way — and
//! it was wrong in both of them independently.

/// Join JSONL records so every line is newline-**terminated**, including the
/// last one.
///
/// `lines.join("\n")` separates rather than terminates, which quietly broke
/// both properties the bead export exists to have:
///
/// - **Diff-stability.** Appending a record also rewrites the previously-final
///   line, because it gains the `\n` it was missing. "Adding a bead inserts
///   exactly one line" — the guarantee `export_beads_contract_jsonl` documents,
///   and the reason it sorts by immutable id (rosary-4ebf52) — was therefore
///   false at the end of the file, which is exactly where appends land.
/// - **Being a text file.** `wc -l` undercounts by one, which the rosary-599778
///   magnitude guard reads; and any repo running pre-commit's
///   `end-of-file-fixer` cannot commit at all. The rsry pre-commit hook
///   re-exports and stages the export, the fixer appends the newline, git
///   reports a hook-modified file and aborts — and the retry re-exports it
///   unterminated again. No attempt count escapes that loop. Observed blocking
///   every commit in mache.
///
/// Zero records is an empty file, not a blank line: a blank line would parse as
/// a record and fail.
pub(crate) fn join(lines: Vec<String>) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminates_the_last_line_rather_than_separating() {
        assert_eq!(join(vec!["a".into()]), "a\n");
        assert_eq!(join(vec!["a".into(), "b".into()]), "a\nb\n");
    }

    /// The prefix property is what makes a one-line git diff a true statement
    /// about a one-record append.
    #[test]
    fn appending_leaves_earlier_lines_byte_identical() {
        let one = join(vec!["a".into()]);
        let two = join(vec!["a".into(), "b".into()]);
        assert!(two.starts_with(&one));
    }

    #[test]
    fn no_records_is_an_empty_file_not_a_blank_line() {
        assert_eq!(join(Vec::new()), "");
    }
}
