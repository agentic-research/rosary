// ADR markdown parser — extract atoms from markdown structure

use crate::atom::{Atom, AtomKind};

/// Known ADR section types, mapped from heading text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SectionKind {
    Context,
    Decision,
    Consequences,
    Alternatives,
    OpenQuestions,
    Implementation,
    Validation,
    References,
    Status,
    Other,
}

/// Parse an ADR markdown string into atoms.
pub fn parse_adr(markdown: &str) -> Vec<Atom> {
    let mut atoms = Vec::new();
    let lines: Vec<&str> = markdown.lines().collect();
    let sections = extract_sections(&lines);

    for section in &sections {
        let kind = classify_section(section.heading);
        let section_atoms = extract_atoms_from_section(section, kind);
        atoms.extend(section_atoms);
    }

    atoms
}

/// A parsed section from the markdown.
struct Section<'a> {
    heading: &'a str,
    #[allow(dead_code)]
    heading_level: usize,
    start_line: usize,
    body_lines: Vec<&'a str>,
}

/// Extract ## sections from markdown lines. ### subsections are included in their parent's body.
fn extract_sections<'a>(lines: &[&'a str]) -> Vec<Section<'a>> {
    let mut sections = Vec::new();
    let mut i = 0;

    // Find all ## headings (the main ADR sections)
    while i < lines.len() {
        let line = lines[i];
        if let Some((level, heading)) = parse_heading(line)
            && level == 2
        {
            let start_line = i + 1;
            let mut body_lines = Vec::new();
            let mut j = i + 1;
            while j < lines.len() {
                if let Some((next_level, _)) = parse_heading(lines[j])
                    && next_level <= 2
                {
                    break;
                }
                body_lines.push(lines[j]);
                j += 1;
            }
            sections.push(Section {
                heading,
                heading_level: level,
                start_line,
                body_lines,
            });
            i = j;
            continue;
        }
        i += 1;
    }

    sections
}

/// Parse a markdown heading line, returning (level, text).
fn parse_heading(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim();
    if !trimmed.starts_with('#') {
        return None;
    }
    let level = trimmed.chars().take_while(|&c| c == '#').count();
    if level > 6 {
        return None;
    }
    let text = trimmed[level..].trim();
    if text.is_empty() {
        return None;
    }
    Some((level, text))
}

/// Classify a section heading into a known kind.
fn classify_section(heading: &str) -> SectionKind {
    let lower = heading.to_lowercase();

    if lower.contains("context") || lower.contains("problem") || lower.contains("motivation") {
        SectionKind::Context
    } else if lower.contains("decision") && !lower.contains("driver") {
        SectionKind::Decision
    } else if lower.contains("consequence") || lower.contains("impact") {
        SectionKind::Consequences
    } else if lower.contains("alternative") || lower.contains("option") {
        SectionKind::Alternatives
    } else if lower.contains("open question") || lower.contains("unknown") {
        SectionKind::OpenQuestions
    } else if lower.contains("implementation") || lower.contains("phase") || lower.contains("plan")
    {
        SectionKind::Implementation
    } else if lower.contains("validation") || lower.contains("success") || lower.contains("metric")
    {
        SectionKind::Validation
    } else if lower.contains("reference") || lower.contains("link") {
        SectionKind::References
    } else if lower.contains("status") || lower.contains("date") || lower.contains("author") {
        SectionKind::Status
    } else {
        SectionKind::Other
    }
}

/// Extract atoms from a classified section.
fn extract_atoms_from_section(section: &Section, kind: SectionKind) -> Vec<Atom> {
    match kind {
        // These sections don't produce actionable atoms
        SectionKind::References | SectionKind::Status | SectionKind::Other => Vec::new(),

        SectionKind::Context => extract_block_atoms(section, AtomKind::FrictionPoint),
        SectionKind::Decision => extract_block_atoms(section, AtomKind::Decision),
        SectionKind::Consequences => extract_subsection_atoms(section, AtomKind::Consequence),
        SectionKind::Alternatives => extract_subsection_atoms(section, AtomKind::Alternative),
        SectionKind::OpenQuestions => extract_list_atoms(section, AtomKind::OpenQuestion),
        SectionKind::Implementation => extract_subsection_atoms(section, AtomKind::Phase),
        SectionKind::Validation => extract_list_atoms(section, AtomKind::ValidationPoint),
    }
}

/// Extract a single atom from the entire section body.
fn extract_block_atoms(section: &Section, kind: AtomKind) -> Vec<Atom> {
    let body = section.body_lines.join("\n").trim().to_string();
    if body.is_empty() {
        return Vec::new();
    }

    let title = first_sentence(&body).unwrap_or_else(|| section.heading.to_string());
    let references = extract_references(&body);

    vec![Atom {
        kind,
        title,
        body,
        source_line: section.start_line,
        source_section: section.heading.to_string(),
        references,
    }]
}

/// Extract atoms from list items within a section.
fn extract_list_atoms(section: &Section, kind: AtomKind) -> Vec<Atom> {
    let mut atoms = Vec::new();
    let mut current_item: Option<(usize, String)> = None;

    for (offset, line) in section.body_lines.iter().enumerate() {
        let trimmed = line.trim();
        if let Some(text) = strip_list_marker(trimmed) {
            // Flush previous item
            if let Some((line_offset, item_text)) = current_item.take() {
                atoms.push(make_list_atom(
                    kind,
                    &item_text,
                    section.start_line + line_offset,
                    section.heading,
                ));
            }
            current_item = Some((offset, text.to_string()));
        } else if !trimmed.is_empty() {
            // Continuation line
            if let Some((_, ref mut text)) = current_item {
                text.push(' ');
                text.push_str(trimmed);
            }
        }
    }

    // Flush last item
    if let Some((line_offset, item_text)) = current_item {
        atoms.push(make_list_atom(
            kind,
            &item_text,
            section.start_line + line_offset,
            section.heading,
        ));
    }

    atoms
}

/// Extract atoms from subsections (### headings within a section).
fn extract_subsection_atoms(section: &Section, kind: AtomKind) -> Vec<Atom> {
    let mut atoms = Vec::new();
    let mut current_title: Option<String> = None;
    let mut current_body = String::new();
    let mut current_line = section.start_line;

    for (offset, line) in section.body_lines.iter().enumerate() {
        if let Some((_, heading)) = parse_heading(line) {
            // Flush previous subsection
            if let Some(title) = current_title.take() {
                let body = current_body.trim().to_string();
                if !body.is_empty() {
                    let references = extract_references(&body);
                    atoms.push(Atom {
                        kind,
                        title,
                        body,
                        source_line: current_line,
                        source_section: section.heading.to_string(),
                        references,
                    });
                }
            }
            current_title = Some(heading.to_string());
            current_body = String::new();
            current_line = section.start_line + offset;
        } else {
            current_body.push_str(line);
            current_body.push('\n');
        }
    }

    // Flush last subsection
    if let Some(title) = current_title {
        let body = current_body.trim().to_string();
        if !body.is_empty() {
            let references = extract_references(&body);
            atoms.push(Atom {
                kind,
                title,
                body,
                source_line: current_line,
                source_section: section.heading.to_string(),
                references,
            });
        }
    }

    // If no subsections found, treat whole body as one atom
    if atoms.is_empty() {
        return extract_block_atoms(section, kind);
    }

    atoms
}

fn make_list_atom(kind: AtomKind, text: &str, line: usize, section: &str) -> Atom {
    let references = extract_references(text);
    Atom {
        kind,
        title: first_sentence(text).unwrap_or_else(|| text.to_string()),
        body: text.to_string(),
        source_line: line,
        source_section: section.to_string(),
        references,
    }
}

/// Extract the first sentence from text (up to first period followed by space or end).
fn first_sentence(text: &str) -> Option<String> {
    let first_line = text.lines().next()?;
    let trimmed = first_line
        .trim()
        .trim_start_matches("**")
        .trim_end_matches("**");
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

/// Strip list markers (-, *, 1., 1)) from a line, returning the remainder.
fn strip_list_marker(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix("- ") {
        return Some(rest);
    }
    if let Some(rest) = trimmed.strip_prefix("* ") {
        return Some(rest);
    }
    // Numbered lists: "1. " or "1) "
    if trimmed.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        if let Some(pos) = trimmed.find(". ") {
            return Some(&trimmed[pos + 2..]);
        }
        if let Some(pos) = trimmed.find(") ") {
            return Some(&trimmed[pos + 2..]);
        }
    }
    None
}

/// Extract cross-references from markdown text.
/// Finds: [text](url), `backtick-refs`, and bead IDs (xxx-yyy pattern).
pub fn extract_references(text: &str) -> Vec<String> {
    let mut refs = Vec::new();

    // Markdown links: [text](url)
    let mut i = 0;
    let bytes = text.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'['
            && let Some(close_bracket) = text[i + 1..].find(']')
        {
            let after = i + 1 + close_bracket + 1;
            if after < bytes.len()
                && bytes[after] == b'('
                && let Some(close_paren) = text[after + 1..].find(')')
            {
                let url = &text[after + 1..after + 1 + close_paren];
                if !url.is_empty() {
                    refs.push(url.to_string());
                }
                i = after + 1 + close_paren + 1;
                continue;
            }
        }
        i += 1;
    }

    // Backtick references: `something`
    for cap in text.split('`').collect::<Vec<_>>().chunks(2) {
        if cap.len() == 2 && !cap[1].is_empty() && !cap[1].contains('\n') {
            let inner = cap[1].trim();
            if !inner.is_empty() && inner.len() < 100 {
                refs.push(inner.to_string());
            }
        }
    }

    refs.sort();
    refs.dedup();
    refs
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_ADR: &str = r#"# ADR-001: Use Harmony Format

**Status:** Proposed
**Date:** 2026-03-14

## Context

ADRs exist across 60+ files but don't connect to actionable work.
Rosary tracks beads but lacks narrative coherence above threads.

## Decision

Use OpenAI's `Harmony` token format for a 3-tier lattice.

## Consequences

### Positive

- ADRs become actionable
- Cross-repo coherence via `mache-85t`

### Negative

- New dependency on `openai-harmony`
- Channel semantics overloaded

## Open Questions

1. Should `decade` be renamed?
2. Does StreamableParser work for non-LLM streams?
3. How to handle accretion conflicts?

## Implementation Plan

### Phase 1: Scaffold
- Create crate skeleton
- Add openai-harmony dependency

### Phase 2: Decompose
- Implement atom mapping
- Wire to Dolt

### Phase 3: Accrete
- Bead completion flows
- Mache schema

## References

- [Harmony format](https://developers.openai.com/cookbook/articles/openai-harmony)
- [openai-harmony crate](https://crates.io/crates/openai-harmony)
"#;

    #[test]
    fn parse_adr_produces_atoms() {
        let atoms = parse_adr(SAMPLE_ADR);
        assert!(!atoms.is_empty(), "should produce atoms from sample ADR");
    }

    #[test]
    fn context_produces_friction_point() {
        let atoms = parse_adr(SAMPLE_ADR);
        let friction: Vec<_> = atoms
            .iter()
            .filter(|a| a.kind == AtomKind::FrictionPoint)
            .collect();
        assert_eq!(friction.len(), 1);
        assert!(friction[0].body.contains("60+ files"));
    }

    #[test]
    fn decision_produces_decision_atom() {
        let atoms = parse_adr(SAMPLE_ADR);
        let decisions: Vec<_> = atoms
            .iter()
            .filter(|a| a.kind == AtomKind::Decision)
            .collect();
        assert_eq!(decisions.len(), 1);
        assert!(decisions[0].body.contains("Harmony"));
    }

    #[test]
    fn consequences_split_positive_negative() {
        let atoms = parse_adr(SAMPLE_ADR);
        let consequences: Vec<_> = atoms
            .iter()
            .filter(|a| a.kind == AtomKind::Consequence)
            .collect();
        assert_eq!(consequences.len(), 2, "should split positive and negative");
    }

    #[test]
    fn open_questions_produce_atoms() {
        let atoms = parse_adr(SAMPLE_ADR);
        let questions: Vec<_> = atoms
            .iter()
            .filter(|a| a.kind == AtomKind::OpenQuestion)
            .collect();
        assert_eq!(questions.len(), 3);
    }

    #[test]
    fn implementation_phases_produce_atoms() {
        let atoms = parse_adr(SAMPLE_ADR);
        let phases: Vec<_> = atoms.iter().filter(|a| a.kind == AtomKind::Phase).collect();
        assert_eq!(phases.len(), 3);
        assert!(phases[0].title.contains("Scaffold"));
    }

    #[test]
    fn references_section_produces_no_atoms() {
        let atoms = parse_adr(SAMPLE_ADR);
        for atom in &atoms {
            assert_ne!(atom.source_section, "References");
        }
    }

    #[test]
    fn empty_markdown_produces_no_atoms() {
        let atoms = parse_adr("");
        assert!(atoms.is_empty());
    }

    #[test]
    fn non_adr_markdown_produces_no_atoms() {
        let atoms = parse_adr("# Hello World\n\nJust a regular doc.\n");
        // "Other" sections produce no atoms
        assert!(atoms.is_empty());
    }

    #[test]
    fn references_extracted_from_links() {
        let refs = extract_references("See [Harmony](https://example.com) for details");
        assert!(refs.contains(&"https://example.com".to_string()));
    }

    #[test]
    fn references_extracted_from_backticks() {
        let refs = extract_references("Uses `openai-harmony` and `mache-85t`");
        assert!(refs.contains(&"openai-harmony".to_string()));
        assert!(refs.contains(&"mache-85t".to_string()));
    }

    #[test]
    fn parse_heading_works() {
        assert_eq!(parse_heading("## Context"), Some((2, "Context")));
        assert_eq!(
            parse_heading("### Phase 1: Scaffold"),
            Some((3, "Phase 1: Scaffold"))
        );
        assert_eq!(parse_heading("not a heading"), None);
        assert_eq!(parse_heading("##"), None);
    }

    #[test]
    fn classify_section_works() {
        assert_eq!(classify_section("Context"), SectionKind::Context);
        assert_eq!(classify_section("Problem Statement"), SectionKind::Context);
        assert_eq!(classify_section("Decision"), SectionKind::Decision);
        assert_eq!(classify_section("Consequences"), SectionKind::Consequences);
        assert_eq!(
            classify_section("Open Questions"),
            SectionKind::OpenQuestions
        );
        assert_eq!(
            classify_section("Implementation Plan"),
            SectionKind::Implementation
        );
        assert_eq!(classify_section("Random Section"), SectionKind::Other);
    }
}
