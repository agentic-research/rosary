//! The bead contract's dependency-edge codec (rosary-664e25): the ONE place an
//! edge set is encoded for every export path (`export --jsonl`, the tracked
//! projection, commit-time publish, trunk-refresh, the pre-push compare) and
//! decoded by restore.
//!
//! `dependencies` stays a list of target ids, sorted — bd's shape, so a record
//! whose edges are all `blocks` is byte-identical to what rosary always wrote.
//! A non-default edge type rides in `dependency_types` (`{target: type}`),
//! present only when there is one. Before this, export read ids only and
//! restore wrote every edge as `blocks`: a `parent-child` edge did not survive
//! a clone or a migrate, and the close-merged containment gate saw no children.

use serde_json::{Map, Value};

/// The type an edge has when the record says nothing — and what bd assumes.
pub const DEFAULT_DEP_TYPE: &str = "blocks";

/// Encode `(target, type)` edges: the sorted id list for the record's
/// `dependencies` slot, and the non-default types for `dependency_types`.
pub fn encode(deps: &[(String, String)]) -> (Vec<String>, Map<String, Value>) {
    let mut deps: Vec<&(String, String)> = deps.iter().collect();
    deps.sort();
    let ids = deps.iter().map(|(id, _)| id.clone()).collect();
    let types = deps
        .iter()
        .filter(|(_, t)| t != DEFAULT_DEP_TYPE)
        .map(|(id, t)| (id.clone(), Value::String(t.clone())))
        .collect();
    (ids, types)
}

/// Add `dependency_types` to a contract record when there is any.
pub fn attach_types(record: &mut Value, types: Map<String, Value>) {
    if !types.is_empty()
        && let Value::Object(m) = record
    {
        m.insert("dependency_types".to_string(), Value::Object(types));
    }
}

/// Decode a record's edge set. A record written before `dependency_types`
/// existed — or by bd — reads as all-`blocks`, which is what it meant.
pub fn decode(record: &Value) -> Vec<(String, String)> {
    let types = record.get("dependency_types").and_then(Value::as_object);
    record
        .get("dependencies")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|id| {
            let t = types
                .and_then(|m| m.get(id))
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_DEP_TYPE);
            (id.to_string(), t.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(id: &str, t: &str) -> (String, String) {
        (id.to_string(), t.to_string())
    }

    #[test]
    fn an_all_blocks_edge_set_carries_no_types_key() {
        let mut record = serde_json::json!({ "dependencies": [] });
        let (ids, types) = encode(&[edge("b", "blocks"), edge("a", "blocks")]);
        record["dependencies"] = serde_json::json!(ids);
        attach_types(&mut record, types);
        assert_eq!(record, serde_json::json!({ "dependencies": ["a", "b"] }));
    }

    #[test]
    fn typed_edges_round_trip_and_legacy_records_read_as_blocks() {
        let deps = vec![edge("p", "parent-child"), edge("q", "blocks")];
        let (ids, types) = encode(&deps);
        let mut record = serde_json::json!({ "dependencies": ids });
        attach_types(&mut record, types);
        assert_eq!(decode(&record), deps);

        let legacy = serde_json::json!({ "dependencies": ["x"] });
        assert_eq!(decode(&legacy), vec![edge("x", "blocks")]);
    }
}
