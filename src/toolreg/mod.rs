//! One Rust declaration per operation, projected onto CLI and MCP (rosary-08a278).
//!
//! ## Why this and not capnp
//!
//! ADR-0006 put the tool registry in `schemas/registry.capnp`. It generates 6 of
//! 42 tools and cannot grow: `capnp compile` rejects underscores in field names,
//! the tooldefs emitter uses the capnp name verbatim, and it does not honour
//! `$Json.name`. Every other rosary tool has `repo_path`, `issue_type`,
//! `test_files`. Nested objects, free-form payloads, array-of-object and numeric
//! bounds are unexpressible too.
//!
//! A spike (2026-07-28) showed clap + schemars on one struct expresses **all
//! five** of those shapes, so the cap is a capnp constraint rather than an
//! inherent one. capnp is not the wrong tool — it is being asked to do the wrong
//! job. It is a cross-language WIRE contract (LLO ↔ mache ↔ rosary agreeing on
//! bytes); it is a poor INTERNAL declaration for a Rust binary's own surfaces.
//!
//! mache reached the same answer independently: `internal/mcpregistry` is a plain
//! Go registry and `task gen:server-json` renders from it (mache-802d2b). An
//! in-language registry, not an IDL.
//!
//! ## One declaration, TWO projections — not one struct used verbatim
//!
//! The surfaces legitimately differ, and pretending otherwise is what would make
//! this fail:
//!
//! - `#[command(flatten)]` gives the CLI flat `--session-id`/`--agent` while
//!   schemars emits a nested `$ref`. Both are right for their surface.
//! - array-of-object and free-form fields carry `#[arg(skip)]` — they are not
//!   CLI flags at all. `bead import` takes a FILE PATH on the CLI and inline
//!   objects over MCP.
//!
//! So an entry is `(struct, cli projection, mcp projection)`. `src/parity`
//! already encodes this intuition by matching on OPERATION rather than name.
//!
//! ## Migration posture: prove equivalence BEFORE replacing anything
//!
//! Nothing here is wired into the live tool list yet. Each struct is checked
//! against the hand-written schema it will eventually replace
//! (`tests::generated_schema_matches_the_hand_written_one`), so the swap is a
//! deletion of the literal rather than a rewrite of behaviour.
//!
//! That ordering is deliberate. `rosary-4887d0` (a field dropped on one create
//! surface) and `rosary-79393f` (provenance dropped from every export) were both
//! silent schema changes. Replacing 42 hand-written schemas without an
//! equivalence check would be the same bet at 42× the size.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Emit an explicit free-form object schema.
///
/// The derive default for `serde_json::Value` is a bare `{}` — which accepts
/// anything but does not SAY `additionalProperties: true`, so it would not match
/// the hand-written schema it replaces. Stated rather than implied.
#[allow(dead_code)]
pub fn free_form_object(_: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
    serde_json::from_value(serde_json::json!({
        "type": "object",
        "additionalProperties": true
    }))
    .expect("static schema literal is valid")
}

/// `rsry_bead_search` / `rsry bead search`.
///
/// Carries two of the five shapes capnp cannot express: snake_case field names
/// and numeric bounds.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BeadSearchArgs {
    /// Canonical scope: 'repo:<name>'. Takes priority over repo_path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Legacy: path to repo with .beads/ directory
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_path: Option<String>,
    /// Search query
    pub query: String,
    /// Max results to return (default 20, max 50)
    #[schemars(range(min = 1, max = 50))]
    #[serde(default = "default_limit")]
    pub limit: u32,
}

fn default_limit() -> u32 {
    20
}

/// Render a tool's input schema in the shape MCP expects.
///
/// Two settings matter, and both are rendering rather than semantics — the
/// derive default is a valid schema, just not the one the hand-written literals
/// use:
///
/// - `option_add_null_type = false`: an `Option<String>` otherwise renders as
///   `["string","null"]`. MCP conveys optionality by ABSENCE from `required`,
///   so the union is noise a client has to special-case.
/// - integer bounds: schemars emits `minimum` as `f64`, so `1` becomes `1.0`.
///   Equivalent to a JSON Schema validator, but not byte-equal to what ships
///   today, and the equivalence gate compares values.
pub fn input_schema<T: JsonSchema>() -> serde_json::Value {
    let settings = schemars::r#gen::SchemaSettings::draft07().with(|s| {
        s.option_add_null_type = false;
    });
    let mut v = serde_json::to_value(settings.into_generator().into_root_schema_for::<T>())
        .expect("schema serializes");
    if let Some(o) = v.as_object_mut() {
        o.remove("$schema");
        o.remove("title");
    }
    normalize_integer_bounds(&mut v);
    v
}

/// Rewrite whole-number `minimum`/`maximum` from float to integer.
fn normalize_integer_bounds(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(map) => {
            for key in ["minimum", "maximum"] {
                if let Some(n) = map.get(key).and_then(serde_json::Value::as_f64)
                    && n.fract() == 0.0
                {
                    map.insert(key.to_string(), serde_json::json!(n as i64));
                }
            }
            for (_, child) in map.iter_mut() {
                normalize_integer_bounds(child);
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(normalize_integer_bounds),
        _ => {}
    }
}

#[cfg(test)]
mod tests;
