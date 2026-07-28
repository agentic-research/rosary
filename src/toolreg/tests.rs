use super::*;
use serde_json::{Value, json};

/// Pull a tool's `inputSchema` out of the ASSEMBLED tool list — the same one an
/// MCP client receives, not a scrape of `tools.rs`.
fn hand_written(tool: &str) -> Value {
    let listed = crate::serve::tools::tool_definitions();
    listed["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .find(|t| t.get("name").and_then(Value::as_str) == Some(tool))
        .unwrap_or_else(|| panic!("{tool} not in the assembled tool list"))
        .get("inputSchema")
        .cloned()
        .expect("inputSchema")
}

/// Compare only what MCP consumes, and report DIFFERENCES rather than a wall of
/// JSON — a failure has to be readable to be actionable.
fn diff_properties(made: &Value, hand: &Value) -> Vec<String> {
    let g = made.pointer("/properties").and_then(Value::as_object);
    let h = hand.pointer("/properties").and_then(Value::as_object);
    let (Some(g), Some(h)) = (g, h) else {
        return vec!["one side has no properties object".into()];
    };
    let mut out = Vec::new();
    for (k, hv) in h {
        match g.get(k) {
            None => out.push(format!("  MISSING from generated: {k}")),
            Some(gv) => {
                for key in [
                    "type",
                    "minimum",
                    "maximum",
                    "default",
                    "additionalProperties",
                ] {
                    let (a, b) = (gv.get(key), hv.get(key));
                    if a != b {
                        out.push(format!(
                            "  {k}.{key}: generated={} hand-written={}",
                            a.map(ToString::to_string).unwrap_or("—".into()),
                            b.map(ToString::to_string).unwrap_or("—".into())
                        ));
                    }
                }
            }
        }
    }
    for k in g.keys() {
        if !h.contains_key(k) {
            out.push(format!("  EXTRA in generated: {k}"));
        }
    }
    out
}

/// THE EQUIVALENCE CHECK. Until this passes for a tool, its hand-written schema
/// stays authoritative and nothing is swapped.
///
/// Deliberately compares against the LIVE tool list rather than a fixture, so it
/// keeps holding as the hand-written schema is edited — the drift it guards runs
/// in both directions.
#[test]
fn generated_schema_matches_the_hand_written_one() {
    let hand = hand_written("rsry_bead_search");
    let made = input_schema::<BeadSearchArgs>();
    let diffs = diff_properties(&made, &hand);
    assert!(
        diffs.is_empty(),
        "BeadSearchArgs does not yet reproduce rsry_bead_search's schema:\n{}\n\n\
         generated = {}\n",
        diffs.join("\n"),
        serde_json::to_string_pretty(&made).unwrap_or_default()
    );
}

/// `required` must agree too — a field that is optional on one side and
/// mandatory on the other is a behaviour change, not a formatting one.
#[test]
fn required_fields_agree() {
    let hand = hand_written("rsry_bead_search");
    let made = input_schema::<BeadSearchArgs>();
    let as_set = |v: &Value| -> std::collections::BTreeSet<String> {
        v.get("required")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    assert_eq!(
        as_set(&made),
        as_set(&hand),
        "required-field sets differ for rsry_bead_search"
    );
}

/// The snake_case field names survive — the single thing capnp could not do,
/// and the reason this registry exists at all.
#[test]
fn snake_case_field_names_survive() {
    let made = input_schema::<BeadSearchArgs>();
    let props = made
        .pointer("/properties")
        .and_then(Value::as_object)
        .unwrap();
    assert!(
        props.contains_key("repo_path"),
        "repo_path must appear verbatim — capnp rejects the underscore, which is \
         why 36 of 42 tools cannot be generated from the capnp registry. Got: {:?}",
        props.keys().collect::<Vec<_>>()
    );
}

/// The free-form helper says `additionalProperties: true` explicitly rather than
/// relying on a bare `{}`, which accepts anything but asserts nothing.
#[test]
fn free_form_helper_is_explicit() {
    let mut made = schemars::r#gen::SchemaGenerator::default();
    let s = serde_json::to_value(free_form_object(&mut made)).unwrap();
    assert_eq!(s, json!({"type": "object", "additionalProperties": true}));
}
