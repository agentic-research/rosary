//! R4b corpus audit (rosary-a66b3a): fold every bead's persisted observation
//! history and compare the lattice-derived status against `persist_status` (the
//! mutable cell). This is the evidence instrument that gates the read-path flip
//! — run it across a real store; when it reads clean (divergences all explained
//! or zero), the fold is proven equivalent and `persist_status` can be deleted.
//!
//! It reads only ALREADY-persisted data (the observations written since R4b
//! step 1), so no live dispatch is needed to measure — the corpus fills as real
//! runs accumulate.

use super::PipelineVerdictValue;
use super::shadow::{folded_pipeline_verdict, parse_events_for};
use crate::store::{BeadStore, WorkRef};
use anyhow::Result;

/// One bead's audit result.
#[derive(Debug, Clone)]
pub struct AuditRow {
    pub bead_id: String,
    /// The bead's current `persist_status` value.
    pub status: String,
    /// Lattice chain-max verdict folded from the bead's observations.
    pub folded: Option<PipelineVerdictValue>,
    /// Status the folded verdict maps to.
    pub expected: Option<String>,
}

/// Aggregate audit over a store.
#[derive(Debug, Default)]
pub struct AuditReport {
    /// Total beads scanned.
    pub total: usize,
    /// Beads with foldable observations (excludes legacy / no-observation beads).
    pub comparable: usize,
    /// Comparable beads whose folded status matches `persist_status`.
    pub agree: usize,
    /// Comparable beads where the two disagree — for inspection.
    pub divergences: Vec<AuditRow>,
}

/// Normalize equivalent status spellings so the comparison isn't fooled by
/// vocabulary (`closed` and `done` are the same terminal state).
fn normalize(s: &str) -> &str {
    match s {
        "closed" => "done",
        other => other,
    }
}

/// Fold every bead's observation history and diff the derived status against
/// its stored status.
pub async fn audit_store(store: &dyn BeadStore, repo_name: &str) -> Result<AuditReport> {
    let beads = store.list_all_beads(repo_name).await?;
    let mut report = AuditReport {
        total: beads.len(),
        ..Default::default()
    };

    for b in beads {
        let events = store.list_event_details(&b.id, "observation").await?;
        let work = WorkRef {
            repo: repo_name.to_string(),
            scope: String::new(),
            bead_id: b.id.clone(),
        };
        let observations = parse_events_for(&events, &work);
        if observations.is_empty() {
            continue; // never dispatched — no observations to fold
        }
        report.comparable += 1;

        let folded = folded_pipeline_verdict(&observations, &work);
        let expected = folded.map(|v| v.expected_bead_status().to_string());

        let agrees = matches!(&expected, Some(e) if normalize(e) == normalize(&b.status));
        if agrees {
            report.agree += 1;
        } else {
            report.divergences.push(AuditRow {
                bead_id: b.id,
                status: b.status,
                folded,
                expected,
            });
        }
    }
    Ok(report)
}

impl AuditReport {
    /// Human-readable summary for the CLI.
    pub fn render(&self, repo_name: &str) -> String {
        let mut out = format!(
            "lattice audit [{repo_name}]: beads={} comparable={} agree={} diverge={}\n",
            self.total,
            self.comparable,
            self.agree,
            self.divergences.len()
        );
        for d in &self.divergences {
            out.push_str(&format!(
                "  DIVERGE {} : persisted={} folded={:?} (expected={})\n",
                d.bead_id,
                d.status,
                d.folded,
                d.expected.as_deref().unwrap_or("?"),
            ));
        }
        if self.comparable == 0 {
            out.push_str(
                "  (no foldable observations yet — the corpus fills as dispatches run \
                 post-R4b-step-1)\n",
            );
        } else if self.divergences.is_empty() {
            out.push_str("  ✓ lattice agrees with persist_status across the corpus\n");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Observation, PipelineVerdictValue, Source};
    use super::*;
    use crate::bead_sqlite::connect_bead_store;

    async fn seed(
        store: &dyn BeadStore,
        id: &str,
        transitions: &[&str],
        verdicts: &[PipelineVerdictValue],
    ) {
        store
            .create_bead_full(crate::store::NewBead {
                id: id.into(),
                title: id.into(),
                ..Default::default()
            })
            .await
            .unwrap();
        // Walk a valid transition path (update_status validates transitions).
        for s in transitions {
            store.update_status(id, s).await.unwrap();
        }
        for (phase, v) in verdicts.iter().enumerate() {
            let obs = Observation::pipeline_verdict(
                WorkRef {
                    repo: "myrepo".into(),
                    scope: String::new(),
                    bead_id: id.into(),
                },
                Source::new("rosary"),
                format!("phase{phase}"),
                *v,
                chrono::Utc::now(),
            );
            let detail = serde_json::to_string(&serde_json::json!({ "observation": obs })).unwrap();
            store.log_event(id, "observation", &detail).await;
        }
    }

    #[tokio::test]
    async fn audit_flags_only_real_divergences() {
        let repo = crate::testutil::TestRepo::new();
        let store = connect_bead_store(&repo.path().join(".beads"))
            .await
            .unwrap();

        // Agrees: walk to "done"; folded chain-max Done → "done".
        seed(
            &*store,
            "myrepo-ok",
            &["dispatched", "verifying", "done"],
            &[PipelineVerdictValue::Verifying, PipelineVerdictValue::Done],
        )
        .await;
        // Diverges: observations fold to Done but the mutable cell stays "open".
        seed(&*store, "myrepo-bad", &[], &[PipelineVerdictValue::Done]).await;
        // Not comparable: no observations.
        store
            .create_bead_full(crate::store::NewBead {
                id: "myrepo-none".into(),
                title: "n".into(),
                ..Default::default()
            })
            .await
            .unwrap();

        let report = audit_store(&*store, "myrepo").await.unwrap();
        assert_eq!(report.total, 3);
        assert_eq!(report.comparable, 2);
        assert_eq!(report.agree, 1);
        assert_eq!(report.divergences.len(), 1);
        assert_eq!(report.divergences[0].bead_id, "myrepo-bad");
        assert_eq!(report.divergences[0].expected.as_deref(), Some("done"));
    }
}
