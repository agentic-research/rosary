//! Guard against tool-call markup absorbed into a bead field (rosary-4d5d70).
//!
//! Split out of `bead_ops` (Golden Rule 2); re-exported there, so every surface
//! still reaches it through the shared op core.

/// Reject a field value that absorbed its neighbouring tool parameter
/// (rosary-4d5d70). A client that mis-serialises adjacent parameters leaves one
/// field holding `…</description>\n<parameter name="acceptance_criteria">…` and
/// the neighbour empty — and the close gate then reads the empty one. The
/// signature is a closing tag immediately followed by a `<parameter name=`
/// opener; prose that merely *mentions* `<parameter name=...>` (as bug reports
/// about this very leak do) does not match, so it is still accepted.
pub fn reject_absorbed_parameter(field: &str, value: &str) -> anyhow::Result<()> {
    const OPENER: &str = "<parameter name=";
    for (at, _) in value.match_indices(OPENER) {
        // Walk back over only `</name>` + whitespace — never the whole prefix —
        // so a value packed with openers stays linear (Copilot, #510). Each
        // backward scan stops at the first byte outside its class, and the
        // whitespace/name runs before one opener can't overlap another's.
        let before = value[..at].trim_end();
        let Some(inner) = before.strip_suffix('>') else {
            continue;
        };
        let name_len = inner
            .bytes()
            .rev()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
            .count();
        let tag_start = inner.len() - name_len;
        let is_closing_tag = name_len > 0 && inner[..tag_start].ends_with("</");
        if is_closing_tag {
            let tag_start = tag_start - 2;
            anyhow::bail!(
                "{field} contains tool-call markup ({}{OPENER}…) — an adjacent parameter \
                 was absorbed into it (rosary-4d5d70); resend each field separately",
                &before[tag_start..]
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A value packed with openers (and no closing tag to anchor on) must stay
    /// linear: a backward scan over the whole prefix per opener is ~10^11 byte
    /// reads here — minutes — versus milliseconds for the bounded walk.
    #[test]
    fn absorbed_parameter_scan_is_linear_in_the_value() {
        let value = "<parameter name=".repeat(200_000);
        let started = std::time::Instant::now();
        reject_absorbed_parameter("acceptance_criteria", &value).unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "scan took {:?} over {} bytes",
            started.elapsed(),
            value.len()
        );
    }

    #[test]
    fn absorbed_parameter_is_caught_after_a_long_prose_prefix() {
        let value = format!(
            "{}</description>\n<parameter name=\"x\">y",
            "word ".repeat(10_000)
        );
        let err = reject_absorbed_parameter("description", &value)
            .unwrap_err()
            .to_string();
        assert!(err.contains("(</description><parameter name=…)"), "{err}");
    }
}
