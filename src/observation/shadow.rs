//! R4b (rosary-a66b3a) steps 2–3: read the persisted `observation` events back,
//! rebuild the in-memory G-set, fold it through the `FieldAlgebra` registry, and
//! derive the pipeline verdict — a **shadow** of `persist_status`. No read-path
//! flip: this lets us observe (and later corpus-compare) what the lattice would
//! report, while the mutable cell stays authoritative until the fold is proven.

use super::log::ObservationLog;
use super::{FieldName, FieldValue, Observation, PipelineVerdictValue};
use crate::store::WorkRef;

/// Parse the JSON envelopes `append_observation` writes
/// (`{"observation": <Observation>, "detail": ...}`) back into `Observation`s.
/// Malformed or legacy (flat-string) events are skipped, not errored.
pub fn parse_observation_events(events: &[String]) -> Vec<Observation> {
    events
        .iter()
        .filter_map(|e| {
            let v: serde_json::Value = serde_json::from_str(e).ok()?;
            serde_json::from_value(v.get("observation")?.clone()).ok()
        })
        .collect()
}

/// Fold the observations and return the lattice's chain-max `PipelineVerdict`
/// for `work` — the derived pipeline state the lattice would report. `None` when
/// no pipeline-verdict observation exists for the work item.
pub fn folded_pipeline_verdict(
    observations: &[Observation],
    work: &WorkRef,
) -> Option<PipelineVerdictValue> {
    let mut log = ObservationLog::new();
    for o in observations {
        log.insert(o.clone());
    }
    let views = super::fold::fold(&log).ok()?;
    let per_source = views
        .get(work)?
        .per_source
        .get(&FieldName::PipelineVerdict)?;
    // Chain-max across sources: the furthest-along verdict any source folded to.
    per_source
        .values()
        .filter_map(|fv| match fv {
            FieldValue::PipelineVerdict(v) => Some(*v),
            _ => None,
        })
        .max_by_key(|v| v.rank().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::super::Source;
    use super::*;
    use chrono::Utc;

    fn work() -> WorkRef {
        WorkRef {
            repo: "rosary".into(),
            scope: String::new(),
            bead_id: "rosary-abc".into(),
        }
    }

    fn event(verdict: PipelineVerdictValue, phase: u32) -> String {
        let obs = Observation::pipeline_verdict(
            work(),
            Source::new("rosary"),
            format!("phase{phase}:dev-agent"),
            verdict,
            Utc::now(),
        );
        serde_json::to_string(&serde_json::json!({ "observation": obs, "detail": "x" })).unwrap()
    }

    #[test]
    fn parses_envelopes_back_into_observations() {
        let events = vec![event(PipelineVerdictValue::Pass, 2), "not json".into()];
        let obs = parse_observation_events(&events);
        assert_eq!(obs.len(), 1, "legacy/garbled events are skipped");
        assert_eq!(obs[0].field, FieldName::PipelineVerdict);
    }

    #[test]
    fn folds_pipeline_lifecycle_to_chain_max() {
        // Dispatched → Verifying → Pass folds to Pass (chain-max).
        let events = vec![
            event(PipelineVerdictValue::Dispatched, 0),
            event(PipelineVerdictValue::Verifying, 1),
            event(PipelineVerdictValue::Pass, 2),
        ];
        let obs = parse_observation_events(&events);
        assert_eq!(
            folded_pipeline_verdict(&obs, &work()),
            Some(PipelineVerdictValue::Pass)
        );
    }

    #[test]
    fn fail_does_not_advance_the_chain() {
        // Fail carries no rank (ADR-0010) — the chain-max stays at Verifying.
        let events = vec![
            event(PipelineVerdictValue::Verifying, 1),
            event(PipelineVerdictValue::Fail, 2),
        ];
        let obs = parse_observation_events(&events);
        assert_eq!(
            folded_pipeline_verdict(&obs, &work()),
            Some(PipelineVerdictValue::Verifying)
        );
    }

    #[test]
    fn no_observations_yields_none() {
        assert_eq!(folded_pipeline_verdict(&[], &work()), None);
    }
}
