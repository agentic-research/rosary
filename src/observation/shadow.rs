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
/// Malformed events are skipped, not errored. Only the JSON-envelope form is
/// handled here — for the legacy flat-string history (which carries the verdict
/// but no work_item), use [`parse_events_for`], which takes the `WorkRef`.
pub fn parse_observation_events(events: &[String]) -> Vec<Observation> {
    events
        .iter()
        .filter_map(|e| {
            let v: serde_json::Value = serde_json::from_str(e).ok()?;
            serde_json::from_value(v.get("observation")?.clone()).ok()
        })
        .collect()
}

/// Parse observation events for a known `work` item, handling BOTH the JSON
/// envelope (post R4b step 1) AND the legacy flat string
/// (`"phase=N agent=A verdict=V detail=D"`) that predates it. The legacy form
/// carries the verdict but not the work_item, so it's reconstructed from
/// `work` — this makes the entire persisted history foldable, not just events
/// written after the format change.
pub fn parse_events_for(events: &[String], work: &WorkRef) -> Vec<Observation> {
    events
        .iter()
        .filter_map(|e| {
            // JSON envelope first.
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(e)
                && let Some(o) = v.get("observation")
                && let Ok(obs) = serde_json::from_value::<Observation>(o.clone())
            {
                return Some(obs);
            }
            parse_legacy_flat(e, work)
        })
        .collect()
}

/// Reconstruct an `Observation` from the legacy flat-string event
/// `"phase=N agent=A verdict=V detail=D"`. `verdict` is the Debug spelling of
/// `dolt::observations::Verdict`. Time isn't recoverable from the string, but
/// the `PipelineVerdict` fold is chain-max (rank-based), not time-based, so it
/// doesn't affect the derived status.
fn parse_legacy_flat(s: &str, work: &WorkRef) -> Option<Observation> {
    if !s.starts_with("phase=") {
        return None;
    }
    let (mut phase, mut agent, mut verdict) = ("", "", "");
    for tok in s.split_whitespace() {
        if let Some(v) = tok.strip_prefix("phase=") {
            phase = v;
        } else if let Some(v) = tok.strip_prefix("agent=") {
            agent = v;
        } else if let Some(v) = tok.strip_prefix("verdict=") {
            verdict = v;
        }
    }
    let pv = match verdict {
        "Dispatched" => PipelineVerdictValue::Dispatched,
        "Verifying" => PipelineVerdictValue::Verifying,
        "Pass" => PipelineVerdictValue::Pass,
        "PrOpen" => PipelineVerdictValue::PrOpen,
        "Done" => PipelineVerdictValue::Done,
        "Fail" => PipelineVerdictValue::Fail,
        "Deadletter" => PipelineVerdictValue::Deadletter,
        _ => return None,
    };
    Some(Observation::pipeline_verdict(
        work.clone(),
        super::Source::new("rosary"),
        format!("phase{phase}:{agent}"),
        pv,
        chrono::DateTime::from_timestamp(0, 0).unwrap_or_default(),
    ))
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

    #[test]
    fn parse_events_for_handles_json_envelope_and_legacy_flat() {
        // One JSON envelope + one legacy flat string; both reconstruct.
        let json = event(PipelineVerdictValue::Pass, 2);
        let legacy =
            "phase=0 agent=scoping-agent verdict=Dispatched detail=agent dispatched".to_string();
        let obs = parse_events_for(&[json, legacy, "garbage".into()], &work());
        assert_eq!(obs.len(), 2, "both formats parse; garbage skipped");
        // The legacy one folds in with the JSON one for the same work item.
        assert_eq!(
            folded_pipeline_verdict(&obs, &work()),
            Some(PipelineVerdictValue::Pass)
        );
    }

    #[test]
    fn legacy_deadletter_folds_to_highest_non_fail() {
        // The real-corpus finding: a bead that deadlettered folds to its highest
        // NON-fail verdict (chain-max ignores Deadletter), not a terminal state.
        // rosary-a66b3a step-4 must resolve this before the read-path flip.
        let events = vec![
            "phase=0 agent=a verdict=Dispatched detail=x".to_string(),
            "phase=1 agent=a verdict=PrOpen detail=x".to_string(),
            "phase=77 agent=a verdict=Deadletter detail=max retries".to_string(),
        ];
        let obs = parse_events_for(&events, &work());
        assert_eq!(obs.len(), 3);
        assert_eq!(
            folded_pipeline_verdict(&obs, &work()),
            Some(PipelineVerdictValue::PrOpen),
            "chain-max ignores Deadletter — the divergence the audit surfaces"
        );
    }
}
