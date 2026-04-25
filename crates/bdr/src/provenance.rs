// Polymorphic source reference for a BeadSpec.
//
// Replaces the flat `source_adr: String` field with a typed enum that
// captures where a work item originated — ADR, Slack thread, meeting note,
// or manual entry. Future variants (GitHub Issue, Linear issue, RFD) follow
// the same pattern: add a variant, extend the `bdr decompose` resolver.
//
// Serde: tagged with "kind" so JSON stays self-describing across variants.

use serde::{Deserialize, Serialize};

/// Where a BeadSpec originated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProvenanceRef {
    /// Sourced from an Architecture Decision Record.
    Adr {
        /// The ADR identifier, e.g. "ADR-001" or "0007-bdr-enrichment-pipeline".
        id: String,
    },
    /// Sourced from a Slack thread permalink.
    SlackThread {
        /// Permalink URL of the originating Slack thread.
        url: String,
        /// Optional human-readable description of the thread.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    /// Sourced from a meeting note.
    Meeting {
        /// Meeting title or document name.
        title: String,
        /// ISO 8601 date string, e.g. "2026-04-12".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        date: Option<String>,
    },
    /// Manually created without a structured upstream source.
    Manual {
        /// Free-form note describing the origin of the work item.
        note: String,
    },
}

impl ProvenanceRef {
    /// Return a compact human-readable label for display and logging.
    pub fn label(&self) -> String {
        match self {
            ProvenanceRef::Adr { id } => format!("adr:{id}"),
            ProvenanceRef::SlackThread { url, .. } => format!("slack:{url}"),
            ProvenanceRef::Meeting { title, date } => match date {
                Some(d) => format!("meeting:{title} ({d})"),
                None => format!("meeting:{title}"),
            },
            ProvenanceRef::Manual { note } => {
                if note.is_empty() {
                    "manual".to_string()
                } else {
                    format!("manual:{}", truncate(note, 40))
                }
            }
        }
    }

    /// If this is an ADR provenance, return the ADR id.
    pub fn adr_id(&self) -> Option<&str> {
        match self {
            ProvenanceRef::Adr { id } => Some(id.as_str()),
            _ => None,
        }
    }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        let end = s
            .char_indices()
            .take(max)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        &s[..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adr_label() {
        let p = ProvenanceRef::Adr {
            id: "ADR-001".into(),
        };
        assert_eq!(p.label(), "adr:ADR-001");
    }

    #[test]
    fn manual_empty_label() {
        let p = ProvenanceRef::Manual {
            note: String::new(),
        };
        assert_eq!(p.label(), "manual");
    }

    #[test]
    fn adr_id_accessor() {
        let p = ProvenanceRef::Adr { id: "0007".into() };
        assert_eq!(p.adr_id(), Some("0007"));
        let m = ProvenanceRef::Manual { note: "x".into() };
        assert!(m.adr_id().is_none());
    }

    #[test]
    fn serde_roundtrip_adr() {
        let p = ProvenanceRef::Adr {
            id: "ADR-001".into(),
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: ProvenanceRef = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn serde_roundtrip_slack() {
        let p = ProvenanceRef::SlackThread {
            url: "https://slack/archives/C123/p456".into(),
            summary: Some("discussed caching approach".into()),
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: ProvenanceRef = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn serde_roundtrip_manual_no_note() {
        let p = ProvenanceRef::Manual {
            note: String::new(),
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: ProvenanceRef = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn tagged_json_shape() {
        let p = ProvenanceRef::Adr {
            id: "ADR-007".into(),
        };
        let v: serde_json::Value = serde_json::to_value(&p).unwrap();
        assert_eq!(v["kind"], "adr");
        assert_eq!(v["id"], "ADR-007");
    }
}
