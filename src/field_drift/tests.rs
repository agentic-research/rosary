use super::*;
use std::collections::BTreeSet;

/// The canonical field set, from `Bead` itself.
fn canonical() -> BTreeSet<String> {
    let bead = crate::bead::Bead {
        id: "rosary-aa0003".into(),
        title: "t".into(),
        description: "d".into(),
        status: "open".into(),
        priority: 1,
        issue_type: "bug".into(),
        owner: Some("o".into()),
        repo: "rosary".into(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        dependency_count: 0,
        dependent_count: 0,
        comment_count: 0,
        branch: Some("b".into()),
        pr_url: Some("p".into()),
        jj_change_id: Some("j".into()),
        external_ref: Some("X-1".into()),
        files: vec!["src/a.rs".into()],
        test_files: vec!["tests/a.rs".into()],
        created_by: Some("me".into()),
        scope: "s".into(),
        derived_from: vec![bdr::provenance::ProvenanceRef::Adr { id: "0021".into() }],
        acceptance_criteria: "cargo test".into(),
    };
    match serde_json::to_value(&bead).expect("Bead serializes") {
        serde_json::Value::Object(m) => m.keys().cloned().collect(),
        other => panic!("Bead must serialize to an object, got {other}"),
    }
}

/// What a surface actually carries — asked of the surface, never described.
fn covered(surface: Surface) -> BTreeSet<String> {
    match surface {
        Surface::Export => {
            let bead = sample();
            match crate::import::bead_to_contract_value(&bead, &[], &[]) {
                serde_json::Value::Object(m) => m.keys().cloned().collect(),
                other => panic!("contract must be an object, got {other}"),
            }
        }
        Surface::StoreWrite => {
            match serde_json::to_value(crate::store::NewBead::default())
                .expect("NewBead serializes")
            {
                serde_json::Value::Object(m) => m.keys().cloned().collect(),
                other => panic!("NewBead must serialize to an object, got {other}"),
            }
        }
        Surface::McpCreate => mcp_args("rsry_bead_create"),
        Surface::McpUpdate => mcp_args("rsry_bead_update"),
    }
}

fn sample() -> crate::bead::Bead {
    crate::bead::Bead {
        id: "rosary-aa0004".into(),
        title: "t".into(),
        description: "d".into(),
        status: "open".into(),
        priority: 1,
        issue_type: "bug".into(),
        owner: Some("o".into()),
        repo: "rosary".into(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        dependency_count: 0,
        dependent_count: 0,
        comment_count: 0,
        branch: Some("b".into()),
        pr_url: Some("p".into()),
        jj_change_id: Some("j".into()),
        external_ref: Some("X-1".into()),
        files: vec!["src/a.rs".into()],
        test_files: vec!["tests/a.rs".into()],
        created_by: Some("me".into()),
        scope: "s".into(),
        derived_from: vec![bdr::provenance::ProvenanceRef::Adr { id: "0021".into() }],
        acceptance_criteria: "cargo test".into(),
    }
}

/// Read a tool's advertised argument names out of the ASSEMBLED tool list —
/// the same list an MCP client receives, not a scrape of `tools.rs`.
fn mcp_args(tool: &str) -> BTreeSet<String> {
    let listed = crate::serve::tools::tool_definitions();
    let tools = listed["tools"].as_array().expect("tools array");
    let def = tools
        .iter()
        .find(|t| t.get("name").and_then(|n| n.as_str()) == Some(tool))
        .unwrap_or_else(|| panic!("{tool} is not in the assembled tool list"));
    def.pointer("/inputSchema/properties")
        .and_then(|p| p.as_object())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

const SURFACES: &[Surface] = &[
    Surface::Export,
    Surface::StoreWrite,
    Surface::McpCreate,
    Surface::McpUpdate,
];

/// Sanity: the interrogators see real surfaces. A gate validated against empty
/// sets would pass vacuously — the failure mode this session found twice, in
/// the permission rail and the coverage ratchet.
#[test]
fn surfaces_are_non_empty() {
    assert!(canonical().len() >= 20, "canonical set looks wrong");
    for s in SURFACES {
        assert!(
            covered(*s).len() >= 5,
            "surface {} looks empty: {:?}",
            s.name(),
            covered(*s)
        );
    }
}

/// THE GATE. Every canonical field is on every surface, or exempt with a reason.
#[test]
fn every_canonical_field_is_carried_or_exempt() {
    let canon = canonical();
    let mut undeclared: Vec<String> = Vec::new();
    for surface in SURFACES {
        let have = covered(*surface);
        for field in &canon {
            if have.contains(field) {
                continue;
            }
            let exempt = EXEMPT
                .iter()
                .any(|e| e.field == field && e.surface == *surface);
            if !exempt {
                undeclared.push(format!("  {} :: {}", surface.name(), field));
            }
        }
    }
    assert!(
        undeclared.is_empty(),
        "canonical fields missing from a surface with no recorded decision:\n{}\n\n\
         Either carry the field on that surface, or add an `ex(..)` entry to \
         field_drift::EXEMPT saying why it is absent. Silence is what let \
         acceptance_criteria (rosary-4887d0) and derived_from (rosary-79393f) \
         disappear.",
        undeclared.join("\n")
    );
}

/// An exemption must name a real field and a surface that really lacks it —
/// otherwise the table rots into fiction as fields are added or renamed.
#[test]
fn no_stale_exemptions() {
    let canon = canonical();
    let mut stale = Vec::new();
    for e in EXEMPT {
        if !canon.contains(e.field) {
            stale.push(format!(
                "  {} :: {} — not a canonical field",
                e.surface.name(),
                e.field
            ));
            continue;
        }
        if covered(e.surface).contains(e.field) {
            stale.push(format!(
                "  {} :: {} — exempted, but the surface DOES carry it now; delete the exemption",
                e.surface.name(),
                e.field
            ));
        }
    }
    assert!(stale.is_empty(), "stale exemptions:\n{}", stale.join("\n"));
}

/// Every exemption carries a non-trivial reason. A blank one is silence with
/// extra steps.
#[test]
fn exemptions_carry_reasons() {
    for e in EXEMPT {
        assert!(
            e.reason.len() > 15,
            "{} :: {} has no real reason: {:?}",
            e.surface.name(),
            e.field,
            e.reason
        );
    }
}

/// The gate must be able to FAIL. A check whose only reachable outcome is
/// "pass" is the coverage ratchet all over again (rosary-f78208).
#[test]
fn gate_detects_a_missing_field() {
    let have = covered(Surface::McpCreate);
    assert!(
        !have.contains("a_field_that_does_not_exist"),
        "sanity: fabricated field must not appear"
    );
    // And a field the surface really does carry is seen, so the membership test
    // discriminates rather than always returning false.
    assert!(
        have.contains("title"),
        "MCP create must advertise `title`; got {have:?}"
    );
}
