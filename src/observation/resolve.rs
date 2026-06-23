//! Authenticated-authority resolution over a detected conflict (ADR-0011).
//!
//! `algebra_flat` *detects* a conflict (`FlatLattice::Top { witnesses }`); it
//! does not pick a winner, because claims with no natural lattice order
//! ("Postgres" vs "DynamoDB") have no max. This module resolves such a conflict
//! by **authenticated authority** — the one mechanism no prior system uses
//! (TMS resolves by structure, AGM by entrenchment, Graphiti by recency,
//! venturi by domain-order, sieve by majority).
//!
//! The rule (ADR-0011):
//! 1. **Gate** — keep only authenticated claims (signet/quarantine). Excluded,
//!    not down-weighted.
//! 2. **Undercut** — a claim proved-undercut by an eligible claim is dropped,
//!    regardless of its tier (defeasible, not just outranking).
//! 3. **Rank** — highest [`Authority`] tier wins. Timestamp is **not** a
//!    cross-tier tiebreaker: authority beats recency.
//! 4. **Escalate** — a tie at the top tier escalates to a human (venturi's
//!    move); never silently latest-wins.

use super::Observation;
use std::collections::{HashMap, HashSet};

/// Epistemic authority tier — the resolution rank for conflicts with no
/// natural lattice order.
///
/// A discrete **total order** on purpose (gate-not-weight): resolving by tier
/// preserves the lattice's idempotence/associativity, where a `[0,1]` weight
/// folded into the join would break both (ADR-0011).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Authority {
    /// Inbound auto-ingest / webhook re-assertion. Lowest.
    AutoIngest,
    /// An agent's claim.
    AgentAssertion,
    /// A recorded decision.
    Decision,
    /// An explicit human correction. Wins by default.
    HumanCorrection,
}

/// A conflict witness lifted with the metadata resolution needs.
///
/// Phase 1 takes `authority` and `authenticated` as inputs; deriving them from
/// authenticated observation authorship is rosary-197eb0.
pub struct AuthoritativeClaim<'a> {
    pub observation: &'a Observation,
    pub authority: Authority,
    /// True iff this claim passed the signet/quarantine authenticity gate.
    /// Unauthenticated claims are **excluded**, never down-weighted.
    pub authenticated: bool,
    /// `payload_hash` of a claim this one undercuts WITH PROOF. Undercut
    /// removes the target regardless of its tier — defeasible defeat, not mere
    /// outranking (otherwise this is last-writer-wins with a priority key).
    pub undercuts: Option<String>,
}

/// Outcome of resolving a detected conflict.
#[derive(Debug)]
pub enum Resolution<'a> {
    /// A single claim won. Losers are kept (superseded, not deleted).
    Resolved {
        winner: &'a Observation,
        superseded: Vec<&'a Observation>,
    },
    /// No unambiguous winner — escalate to a human. **Never** silently
    /// latest-wins.
    Escalate {
        reason: String,
        candidates: Vec<&'a Observation>,
    },
}

/// Resolve a detected conflict by authenticated authority. See module docs.
pub fn resolve_by_authority<'a>(claims: &'a [AuthoritativeClaim<'a>]) -> Resolution<'a> {
    // 1. GATE — only authenticated claims are eligible. Excluded, not weighted.
    let eligible: Vec<&AuthoritativeClaim> = claims.iter().filter(|c| c.authenticated).collect();

    // 2. UNDERCUT — drop any eligible claim that another eligible claim
    //    undercuts with proof, regardless of its tier (defeasible defeat).
    let undercut: HashSet<&str> = eligible
        .iter()
        .filter_map(|c| c.undercuts.as_deref())
        .collect();
    // 3. DEDUP by payload_hash — a re-delivered identical observation (same
    //    G-set dedup key) is ONE claim, not two; keep the highest-authority
    //    instance per hash. This makes resolution order-independent AND
    //    idempotent: f({a}) == f({a, a}). Without it, a duplicate flips
    //    Resolved into Escalate at the tie count. (math-friend review 2026-06-22)
    let mut by_hash: HashMap<&str, &AuthoritativeClaim> = HashMap::new();
    for c in eligible
        .into_iter()
        .filter(|c| !undercut.contains(c.observation.payload_hash.as_str()))
    {
        by_hash
            .entry(c.observation.payload_hash.as_str())
            .and_modify(|kept| {
                if c.authority > kept.authority {
                    *kept = c;
                }
            })
            .or_insert(c);
    }
    let survivors: Vec<&AuthoritativeClaim> = by_hash.values().copied().collect();

    if survivors.is_empty() {
        return Resolution::Escalate {
            reason: "no authenticated, un-undercut claim".to_string(),
            candidates: claims.iter().map(|c| c.observation).collect(),
        };
    }

    // 4. RANK — the highest authority tier wins. Timestamp is NOT a cross-tier
    //    tiebreaker: a lower-tier claim never wins by being more recent.
    let top_tier = survivors
        .iter()
        .map(|c| c.authority)
        .max()
        .expect("non-empty");
    let top: Vec<&AuthoritativeClaim> = survivors
        .iter()
        .copied()
        .filter(|c| c.authority == top_tier)
        .collect();

    // 5. Resolve, or ESCALATE on a genuine same-tier tie — never latest-wins.
    //    Output vecs are sorted by payload_hash so the Resolution value is
    //    deterministic regardless of input order / hash-map iteration.
    if top.len() == 1 {
        let winner = top[0].observation;
        let mut superseded: Vec<&Observation> = survivors
            .iter()
            .map(|c| c.observation)
            .filter(|o| o.payload_hash != winner.payload_hash)
            .collect();
        superseded.sort_by(|a, b| a.payload_hash.cmp(&b.payload_hash));
        Resolution::Resolved { winner, superseded }
    } else {
        let mut candidates: Vec<&Observation> = top.iter().map(|c| c.observation).collect();
        candidates.sort_by(|a, b| a.payload_hash.cmp(&b.payload_hash));
        Resolution::Escalate {
            reason: format!(
                "{} claims tie at top authority tier {top_tier:?}",
                top.len()
            ),
            candidates,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::{FieldName, FieldValue, Observation, Source, WorkRef};
    use chrono::{TimeZone, Utc};

    fn obs(value: &str, source: &str, secs: i64, payload: &str) -> Observation {
        Observation {
            work_item: WorkRef {
                repo: "rosary".to_string(),
                scope: String::new(),
                bead_id: "rosary-test".to_string(),
            },
            source: Source::new(source),
            source_event_id: payload.to_string(),
            field: FieldName::Status,
            value: FieldValue::String(value.to_string()),
            observed_at: Utc.timestamp_opt(secs, 0).unwrap(),
            cert: None,
            payload_hash: payload.to_string(),
        }
    }

    fn claim(o: &Observation, a: Authority) -> AuthoritativeClaim<'_> {
        AuthoritativeClaim {
            observation: o,
            authority: a,
            authenticated: true,
            undercuts: None,
        }
    }

    #[test]
    fn authority_beats_recency() {
        // Human correction observed EARLIER; contradicting auto-ingest LATER.
        let human = obs("postgres", "user", 100, "h1");
        let auto = obs("dynamo", "github", 200, "a1");
        let claims = vec![
            claim(&human, Authority::HumanCorrection),
            claim(&auto, Authority::AutoIngest),
        ];
        match resolve_by_authority(&claims) {
            Resolution::Resolved { winner, .. } => assert_eq!(
                winner.payload_hash, "h1",
                "human correction must win over a later auto-ingest"
            ),
            Resolution::Escalate { reason, .. } => {
                panic!("expected Resolved(human), got Escalate: {reason}")
            }
        }

        // Flip ONLY the timestamps — the winner must not change.
        let human_late = obs("postgres", "user", 300, "h1");
        let auto_early = obs("dynamo", "github", 50, "a1");
        let flipped = vec![
            claim(&human_late, Authority::HumanCorrection),
            claim(&auto_early, Authority::AutoIngest),
        ];
        match resolve_by_authority(&flipped) {
            Resolution::Resolved { winner, .. } => assert_eq!(
                winner.payload_hash, "h1",
                "flipping only the timestamp must not flip the winner — authority, not recency"
            ),
            Resolution::Escalate { reason, .. } => {
                panic!("expected Resolved, got Escalate: {reason}")
            }
        }
    }

    #[test]
    fn unauthenticated_claim_excluded() {
        // A HumanCorrection-tier claim that is NOT authenticated is excluded
        // (gate, not weight); the authenticated lower-tier claim wins.
        let fake_human = obs("dynamo", "spoof", 200, "f1");
        let real_agent = obs("postgres", "agent", 100, "r1");
        let claims = vec![
            AuthoritativeClaim {
                observation: &fake_human,
                authority: Authority::HumanCorrection,
                authenticated: false,
                undercuts: None,
            },
            claim(&real_agent, Authority::AgentAssertion),
        ];
        match resolve_by_authority(&claims) {
            Resolution::Resolved { winner, .. } => assert_eq!(
                winner.payload_hash, "r1",
                "an unauthenticated high-tier claim must be excluded, not down-weighted"
            ),
            Resolution::Escalate { reason, .. } => {
                panic!("expected Resolved(agent), got Escalate: {reason}")
            }
        }
    }

    #[test]
    fn undercut_overrides_rank() {
        // An auto-ingest that UNDERCUTS the human correction (with proof)
        // removes it — undercut defeats rank, leaving the auto-ingest.
        let human = obs("postgres", "user", 100, "h1");
        let auto = obs("dynamo", "github", 200, "a1");
        let claims = vec![
            claim(&human, Authority::HumanCorrection),
            AuthoritativeClaim {
                observation: &auto,
                authority: Authority::AutoIngest,
                authenticated: true,
                undercuts: Some("h1".to_string()),
            },
        ];
        match resolve_by_authority(&claims) {
            Resolution::Resolved { winner, .. } => assert_eq!(
                winner.payload_hash, "a1",
                "undercut must remove the higher-tier claim, not merely lose to it"
            ),
            Resolution::Escalate { reason, .. } => {
                panic!("expected Resolved(auto after undercut), got Escalate: {reason}")
            }
        }
    }

    #[test]
    fn same_tier_escalates_not_latest_wins() {
        // Two human corrections that conflict → escalate to a human, never
        // silently pick the later one.
        let h1 = obs("postgres", "user_a", 100, "h1");
        let h2 = obs("dynamo", "user_b", 200, "h2");
        let claims = vec![
            claim(&h1, Authority::HumanCorrection),
            claim(&h2, Authority::HumanCorrection),
        ];
        match resolve_by_authority(&claims) {
            Resolution::Escalate { candidates, .. } => assert_eq!(
                candidates.len(),
                2,
                "two top-tier claims must escalate, not latest-wins"
            ),
            Resolution::Resolved { winner, .. } => {
                panic!("expected Escalate, got Resolved({})", winner.payload_hash)
            }
        }
    }

    #[test]
    fn duplicate_claims_are_idempotent() {
        // A re-delivered identical claim (same payload_hash = same observation,
        // per the G-set dedup key) must NOT flip Resolved into Escalate.
        // f({a}) == f({a, a}). (math-friend review 2026-06-22)
        let human = obs("postgres", "user", 100, "h1");
        let single = vec![claim(&human, Authority::HumanCorrection)];
        let doubled = vec![
            claim(&human, Authority::HumanCorrection),
            claim(&human, Authority::HumanCorrection),
        ];
        match (
            resolve_by_authority(&single),
            resolve_by_authority(&doubled),
        ) {
            (Resolution::Resolved { winner: w1, .. }, Resolution::Resolved { winner: w2, .. }) => {
                assert_eq!(w1.payload_hash, "h1");
                assert_eq!(
                    w2.payload_hash, "h1",
                    "a duplicate identical claim must not escalate — resolution must be idempotent"
                );
            }
            (a, b) => panic!("expected both Resolved(h1); got {a:?} and {b:?}"),
        }
    }
}
