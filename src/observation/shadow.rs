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

/// The phase index this observation belongs to, parsed from its
/// `source_event_id` (`"phaseN:agent"`, uniform across live + legacy events).
/// Defaults to 0 when absent/unparseable.
fn phase_of(obs: &Observation) -> u32 {
    obs.source_event_id
        .strip_prefix("phase")
        .and_then(|rest| rest.split(':').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// Status of a single phase from its own verdict set — the innermost fold of the
/// `Bead ⊃ Phase ⊃ verdict` catamorphism. This is NOT the global chain-max: a
/// `Fail` here means *this phase's verify failed → retrying*, so it must rank
/// ABOVE `Verifying`/`Dispatched` (it's a strictly later state), yet below a
/// `Pass` (which means the phase ultimately advanced, possibly after retries).
/// Precedence: PrOpen > Pass > Fail > Verifying > Dispatched. Order-independent
/// (set-based), so it holds for the timestamp-less legacy corpus too.
fn phase_local_status(verdicts: &[PipelineVerdictValue]) -> Option<&'static str> {
    use PipelineVerdictValue as V;
    let has = |t: V| verdicts.iter().any(|v| *v == t);
    if has(V::PrOpen) {
        Some("pr_open")
    } else if has(V::Pass) {
        Some("verifying") // phase passed verify, advancing to the next phase
    } else if has(V::Fail) {
        Some("open") // verify failed, back in the queue for retry
    } else if has(V::Verifying) {
        Some("verifying")
    } else if has(V::Dispatched) {
        Some("dispatched")
    } else {
        None
    }
}

/// Derive the pipeline **status** from a work item's full observation set —
/// what `persist_status` should hold.
///
/// The pipeline is recursive state: `Bead ⊃ Phase ⊃ verdict-lifecycle`. A flat
/// chain-max over every phase's verdicts is wrong — it ranks by verdict alone,
/// so an early phase's `Pass` (rank 3) masks a *later* phase's `Fail`/retry
/// (no rank), reporting `verifying` while the bead is really re-queued `open`
/// (rosary-7f7eff). Instead we fold hierarchically:
///   - `Done` present        → `done`    (terminal success, absorbs all phases)
///   - else `Deadletter`     → `blocked` (terminal failure, absorbs all phases)
///   - else the **highest phase reached** governs — its [`phase_local_status`].
///
/// `None` when the work item has no pipeline-verdict observation.
pub fn derived_status(observations: &[Observation], work: &WorkRef) -> Option<String> {
    let by_phase: Vec<(u32, PipelineVerdictValue)> = observations
        .iter()
        .filter(|o| &o.work_item == work)
        .filter_map(|o| match &o.value {
            FieldValue::PipelineVerdict(v) => Some((phase_of(o), *v)),
            _ => None,
        })
        .collect();
    if by_phase.is_empty() {
        return None;
    }
    // Terminal verdicts absorb across ALL phases, order-independently (rosary-818ed4).
    if by_phase
        .iter()
        .any(|(_, v)| *v == PipelineVerdictValue::Done)
    {
        return Some("done".to_string());
    }
    if by_phase
        .iter()
        .any(|(_, v)| *v == PipelineVerdictValue::Deadletter)
    {
        return Some("blocked".to_string());
    }
    // Otherwise the current (highest) phase governs: a later phase's Fail/retry
    // is never masked by an earlier phase's Pass.
    let max_phase = by_phase.iter().map(|(p, _)| *p).max()?;
    let current: Vec<PipelineVerdictValue> = by_phase
        .iter()
        .filter(|(p, _)| *p == max_phase)
        .map(|(_, v)| *v)
        .collect();
    phase_local_status(&current).map(String::from)
}

/// Fold the observations and return the lattice's chain-max `PipelineVerdict`
/// for `work` — the derived pipeline state the lattice would report. `None` when
/// no pipeline-verdict observation exists for the work item.
///
/// Note: this is the raw chain-max verdict for display; [`derived_status`] is
/// the terminal-aware STATUS used for the persist_status comparison.
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
            "raw chain-max ignores Deadletter (display value)"
        );
    }

    // ── rosary-818ed4 regression: terminal-aware derived_status ──────────────

    fn legacy(verdict: &str, phase: u32) -> String {
        format!("phase={phase} agent=a verdict={verdict} detail=x")
    }

    #[test]
    fn derived_status_deadletter_is_terminal_blocked() {
        // THE FIX: a deadlettered bead's derived STATUS is `blocked`, even though
        // the raw chain-max verdict is `PrOpen` (rosary-818ed4 / rosary-11214e).
        let obs = parse_events_for(
            &[
                legacy("Dispatched", 0),
                legacy("PrOpen", 1),
                legacy("Deadletter", 77),
            ],
            &work(),
        );
        assert_eq!(derived_status(&obs, &work()).as_deref(), Some("blocked"));
    }

    #[test]
    fn derived_status_done_dominates_a_prior_fail() {
        // Terminal success absorbs: a Fail that was retried past → still `done`.
        let obs = parse_events_for(
            &[legacy("Verifying", 1), legacy("Fail", 2), legacy("Done", 3)],
            &work(),
        );
        assert_eq!(derived_status(&obs, &work()).as_deref(), Some("done"));
    }

    #[test]
    fn derived_status_chain_max_when_no_terminal() {
        let obs = parse_events_for(&[legacy("Dispatched", 0), legacy("Verifying", 1)], &work());
        assert_eq!(derived_status(&obs, &work()).as_deref(), Some("verifying"));
        let obs = parse_events_for(&[legacy("PrOpen", 4)], &work());
        assert_eq!(derived_status(&obs, &work()).as_deref(), Some("pr_open"));
    }

    #[test]
    fn derived_status_none_without_observations() {
        assert_eq!(derived_status(&[], &work()), None);
    }

    #[test]
    fn later_phase_fail_not_masked_by_earlier_phase_pass() {
        // rosary-7f7eff (dogfood rosary-765f42): phase 1 PASSED, phase 2 is
        // failing/retrying. Flat chain-max would report Pass → `verifying`; the
        // phase-aware fold reports the CURRENT phase's Fail → `open`, matching
        // what persist_status writes for a re-queued bead.
        let obs = parse_events_for(
            &[
                legacy("Dispatched", 1),
                legacy("Verifying", 1),
                legacy("Pass", 1),
                legacy("Dispatched", 2),
                legacy("Verifying", 2),
                legacy("Fail", 2),
            ],
            &work(),
        );
        assert_eq!(derived_status(&obs, &work()).as_deref(), Some("open"));
        // The raw display fold still shows chain-max Pass (informational only).
        assert_eq!(
            folded_pipeline_verdict(&obs, &work()),
            Some(PipelineVerdictValue::Pass)
        );
    }

    #[test]
    fn highest_phase_governs_when_it_passed_and_advances() {
        // phase 2 reached Pass after phase 1 also passed → `verifying` (advancing),
        // not dragged to a lower phase's state.
        let obs = parse_events_for(&[legacy("Pass", 1), legacy("Pass", 2)], &work());
        assert_eq!(derived_status(&obs, &work()).as_deref(), Some("verifying"));
    }

    #[test]
    fn within_phase_retry_that_finally_passes_is_advancing() {
        // A phase that Failed, retried, then Passed → `verifying` (Pass wins over
        // the earlier Fail within the SAME phase).
        let obs = parse_events_for(
            &[legacy("Verifying", 2), legacy("Fail", 2), legacy("Pass", 2)],
            &work(),
        );
        assert_eq!(derived_status(&obs, &work()).as_deref(), Some("verifying"));
    }
}
