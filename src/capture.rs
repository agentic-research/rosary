//! Session and code-provenance capture commands.
//!
//! `rsry capture --from-session <path>` reads a transcript file, runs BDR
//! atom extraction via the LLM, and proposes BeadSpecs to stdout (dry-run)
//! or writes them to `.beads/` (with `--commit`).

use anyhow::Result;
use bdr::decompose::{BeadSpec, decompose_with_meta};
use bdr::parse::DocMeta;
use bdr::provenance::ProvenanceRef;
use std::path::Path;

// ── Session capture ───────────────────────────────────────────────────────────

/// Options for `rsry capture --from-session`.
pub struct SessionCaptureOpts<'a> {
    /// Transcript file path (use `-` for stdin).
    pub transcript_path: &'a str,
    /// LLM model shorthand: "haiku" (default), "sonnet", or a full model ID.
    pub model: &'a str,
}

/// Read the transcript, extract BDR atoms via LLM, and return BeadSpecs.
///
/// The provenance of every returned spec is `ProvenanceRef::Session`.
pub async fn capture_from_session(opts: &SessionCaptureOpts<'_>) -> Result<Vec<BeadSpec>> {
    let content = read_transcript(opts.transcript_path)?;
    let atoms = crate::bdr_enrich::extract_atoms_with_llm(&content, opts.model).await?;

    let provenance = ProvenanceRef::Session {
        transcript_path: opts.transcript_path.to_string(),
        summary: None,
    };

    let meta = DocMeta {
        provenance: Some(provenance),
        ..DocMeta::default()
    };

    let doc_id = Path::new(opts.transcript_path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "session".to_string());

    Ok(decompose_with_meta(&atoms, &doc_id, &meta))
}

fn read_transcript(path: &str) -> Result<String> {
    if path == "-" {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| anyhow::anyhow!("reading stdin: {e}"))?;
        return Ok(buf);
    }
    std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("reading transcript '{}': {e}", path))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod session {
    use super::*;
    use bdr::provenance::ProvenanceRef;

    #[test]
    fn read_transcript_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.md");
        std::fs::write(&path, "## Discussion\n\nWe decided to use Dolt.").unwrap();
        let content = read_transcript(path.to_str().unwrap()).unwrap();
        assert!(content.contains("Dolt"));
    }

    #[test]
    fn read_transcript_missing_file_errors() {
        let result = read_transcript("/nonexistent/path/transcript.md");
        assert!(result.is_err());
    }

    #[test]
    fn session_provenance_set_on_specs() {
        use bdr::atom::{Atom, AtomKind};

        let atoms = vec![Atom {
            kind: AtomKind::TechnicalSpec,
            title: "Use Dolt for bead storage".to_string(),
            body: "Dolt provides version-controlled SQL.".to_string(),
            source_line: 1,
            source_section: "Storage".to_string(),
            references: vec![],
        }];

        let provenance = ProvenanceRef::Session {
            transcript_path: "sessions/2026-04-29.md".to_string(),
            summary: None,
        };

        let meta = DocMeta {
            provenance: Some(provenance),
            ..DocMeta::default()
        };

        let specs = decompose_with_meta(&atoms, "session", &meta);
        assert_eq!(specs.len(), 1);

        let primary = specs[0].primary_source().unwrap();
        assert!(
            matches!(
                primary,
                ProvenanceRef::Session {
                    transcript_path,
                    ..
                } if transcript_path == "sessions/2026-04-29.md"
            ),
            "expected Session provenance, got {primary:?}"
        );
    }

    #[test]
    fn session_provenance_label() {
        let p = ProvenanceRef::Session {
            transcript_path: "sessions/2026-04-29.md".to_string(),
            summary: Some("Discussed Dolt integration".to_string()),
        };
        assert_eq!(p.label(), "session:sessions/2026-04-29.md");
    }

    #[test]
    fn session_provenance_roundtrip() {
        let p = ProvenanceRef::Session {
            transcript_path: "sessions/foo.md".to_string(),
            summary: Some("design review".to_string()),
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: ProvenanceRef = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }
}
