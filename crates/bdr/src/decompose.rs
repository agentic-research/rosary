// Atom → bead mapper with channel annotations

use crate::atom::{Atom, AtomKind};
use crate::channels::BdrChannel;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A bead ready to be created, annotated with BDR lattice metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeadSpec {
    pub title: String,
    pub description: String,
    pub issue_type: String,
    pub priority: u8,
    pub channel: BdrChannel,
    pub thread_group: String,
    pub source_atom: AtomKind,
    pub source_adr: String,
    pub source_line: usize,
    pub references: Vec<String>,
}

/// Channel assignment for each atom kind.
pub fn channel_for_atom(kind: AtomKind) -> BdrChannel {
    match kind {
        AtomKind::FrictionPoint => BdrChannel::Decade,
        AtomKind::Constraint => BdrChannel::Decade,
        AtomKind::Alternative => BdrChannel::Decade,
        AtomKind::Decision => BdrChannel::Thread,
        AtomKind::Consequence => BdrChannel::Thread,
        AtomKind::OpenQuestion => BdrChannel::Thread,
        AtomKind::Phase => BdrChannel::Thread,
        AtomKind::ValidationPoint => BdrChannel::Bead,
        AtomKind::TechnicalSpec => BdrChannel::Bead,
    }
}

/// Decompose a list of atoms into bead specs.
pub fn decompose(atoms: &[Atom], adr_id: &str) -> Vec<BeadSpec> {
    atoms
        .iter()
        .map(|atom| {
            let channel = channel_for_atom(atom.kind);
            let thread_group = assign_thread_group(atom);

            BeadSpec {
                title: format!("[{}] {}", adr_id, truncate(&atom.title, 60)),
                description: atom.body.clone(),
                issue_type: atom.kind.suggested_issue_type().to_string(),
                priority: atom.kind.suggested_priority(),
                channel,
                thread_group,
                source_atom: atom.kind,
                source_adr: adr_id.to_string(),
                source_line: atom.source_line,
                references: atom.references.clone(),
            }
        })
        .collect()
}

/// Group bead specs by thread_group.
pub fn group_by_thread(specs: &[BeadSpec]) -> BTreeMap<String, Vec<&BeadSpec>> {
    let mut groups: BTreeMap<String, Vec<&BeadSpec>> = BTreeMap::new();
    for spec in specs {
        groups
            .entry(spec.thread_group.clone())
            .or_default()
            .push(spec);
    }
    groups
}

/// Assign a thread grouping key based on section name and atom kind.
fn assign_thread_group(atom: &Atom) -> String {
    let section = atom.source_section.to_lowercase();

    if section.contains("context") || section.contains("problem") {
        "context".to_string()
    } else if section.contains("decision") {
        "decision".to_string()
    } else if section.contains("consequence") || section.contains("impact") {
        "consequences".to_string()
    } else if section.contains("implementation") || section.contains("phase") {
        "implementation".to_string()
    } else if section.contains("validation") || section.contains("metric") {
        "validation".to_string()
    } else if section.contains("question") || section.contains("unknown") {
        "open-questions".to_string()
    } else if section.contains("alternative") {
        "alternatives".to_string()
    } else {
        "general".to_string()
    }
}

/// Truncate a string to max_len, appending "..." if truncated.
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom::ALL_ATOM_KINDS;

    fn sample_atom(kind: AtomKind) -> Atom {
        Atom {
            kind,
            title: "Test atom".to_string(),
            body: "Test body with `ref-1` details".to_string(),
            source_line: 10,
            source_section: "Context".to_string(),
            references: vec!["ref-1".to_string()],
        }
    }

    #[test]
    fn friction_point_maps_to_decade() {
        assert_eq!(
            channel_for_atom(AtomKind::FrictionPoint),
            BdrChannel::Decade
        );
    }

    #[test]
    fn decision_maps_to_thread() {
        assert_eq!(channel_for_atom(AtomKind::Decision), BdrChannel::Thread);
    }

    #[test]
    fn validation_point_maps_to_bead() {
        assert_eq!(
            channel_for_atom(AtomKind::ValidationPoint),
            BdrChannel::Bead
        );
    }

    #[test]
    fn all_atom_kinds_have_channel_mapping() {
        for kind in ALL_ATOM_KINDS {
            let _ = channel_for_atom(kind); // should not panic
        }
    }

    #[test]
    fn decompose_single_atom() {
        let atoms = vec![sample_atom(AtomKind::FrictionPoint)];
        let specs = decompose(&atoms, "ADR-001");

        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].issue_type, "task");
        assert_eq!(specs[0].channel, BdrChannel::Decade);
        assert_eq!(specs[0].source_adr, "ADR-001");
        assert!(specs[0].title.starts_with("[ADR-001]"));
    }

    #[test]
    fn decompose_preserves_references() {
        let atoms = vec![sample_atom(AtomKind::Decision)];
        let specs = decompose(&atoms, "ADR-001");
        assert_eq!(specs[0].references, vec!["ref-1".to_string()]);
    }

    #[test]
    fn decompose_empty_atoms() {
        let specs = decompose(&[], "ADR-001");
        assert!(specs.is_empty());
    }

    #[test]
    fn phase_maps_to_epic() {
        let atom = Atom {
            kind: AtomKind::Phase,
            title: "Phase 1: Scaffold".into(),
            body: "Create crate".into(),
            source_line: 1,
            source_section: "Implementation Plan".into(),
            references: vec![],
        };
        let specs = decompose(&[atom], "ADR-001");
        assert_eq!(specs[0].issue_type, "epic");
        assert_eq!(specs[0].channel, BdrChannel::Thread);
        assert_eq!(specs[0].thread_group, "implementation");
    }

    #[test]
    fn group_by_thread_clusters() {
        let atoms = vec![
            Atom {
                source_section: "Context".into(),
                ..sample_atom(AtomKind::FrictionPoint)
            },
            Atom {
                source_section: "Implementation Plan".into(),
                kind: AtomKind::Phase,
                ..sample_atom(AtomKind::Phase)
            },
            Atom {
                source_section: "Implementation Plan".into(),
                kind: AtomKind::Phase,
                title: "Phase 2".into(),
                ..sample_atom(AtomKind::Phase)
            },
        ];
        let specs = decompose(&atoms, "ADR-001");
        let groups = group_by_thread(&specs);

        assert!(groups.contains_key("context"));
        assert!(groups.contains_key("implementation"));
        assert_eq!(groups["implementation"].len(), 2);
    }

    #[test]
    fn bead_spec_serde_roundtrip() {
        let spec = BeadSpec {
            title: "[ADR-001] Test".into(),
            description: "body".into(),
            issue_type: "task".into(),
            priority: 1,
            channel: BdrChannel::Thread,
            thread_group: "implementation".into(),
            source_atom: AtomKind::Phase,
            source_adr: "ADR-001".into(),
            source_line: 42,
            references: vec!["mache-85t".into()],
        };
        let json = serde_json::to_string(&spec).unwrap();
        let back: BeadSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, back);
    }
}
