//! Priority work queue with triage scoring and exponential backoff.
//!
//! Modeled after driftlessaf's workqueue: priority ordering, NotBefore scheduling,
//! dedup by bead ID, and bounded retry with exponential backoff.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::bead::Bead;

/// Base backoff period (doubles per retry).
const BACKOFF_BASE: Duration = Duration::from_secs(30);
/// Maximum backoff period.
const BACKOFF_MAX: Duration = Duration::from_secs(30 * 60);
/// Default max retries before deadletter.
const MAX_RETRIES: u32 = 5;

/// An entry in the priority queue.
#[derive(Debug, Clone)]
pub struct QueueEntry {
    pub bead_id: String,
    pub repo: String,
    pub score: f64,
    pub enqueued_at: Instant,
    pub retries: u32,
    pub generation: u64,
}

// BinaryHeap is a max-heap; higher score = dequeued first.
// Ties broken by earlier enqueue time (older first = fairness).
impl PartialEq for QueueEntry {
    fn eq(&self, other: &Self) -> bool {
        self.bead_id == other.bead_id
    }
}

impl Eq for QueueEntry {}

impl PartialOrd for QueueEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for QueueEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.score
            .partial_cmp(&other.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| other.enqueued_at.cmp(&self.enqueued_at))
    }
}

/// Backoff state for a bead that has been retried.
#[derive(Debug, Clone)]
pub struct BackoffState {
    pub retries: u32,
    pub next_eligible: Instant,
}

impl BackoffState {
    pub fn new(retries: u32, now: Instant) -> Self {
        let delay = std::cmp::min(
            BACKOFF_BASE.saturating_mul(1 << retries.min(10)),
            BACKOFF_MAX,
        );
        BackoffState {
            retries,
            next_eligible: now + delay,
        }
    }

    pub fn is_eligible(&self, now: Instant) -> bool {
        now >= self.next_eligible
    }

    pub fn exceeded_max(&self) -> bool {
        self.retries >= MAX_RETRIES
    }
}

/// Priority work queue with dedup and backoff tracking.
pub struct WorkQueue {
    heap: BinaryHeap<QueueEntry>,
    in_queue: HashSet<String>,
    backoff: HashMap<String, BackoffState>,
}

impl WorkQueue {
    pub fn new() -> Self {
        WorkQueue {
            heap: BinaryHeap::new(),
            in_queue: HashSet::new(),
            backoff: HashMap::new(),
        }
    }

    /// Enqueue a bead if not already in the queue.
    /// Returns true if the bead was added.
    pub fn enqueue(&mut self, entry: QueueEntry) -> bool {
        if self.in_queue.contains(&entry.bead_id) {
            return false;
        }
        self.in_queue.insert(entry.bead_id.clone());
        self.heap.push(entry);
        true
    }

    /// Dequeue the highest-priority bead that is eligible (past backoff).
    /// Skips entries still in backoff, leaving them in the queue.
    pub fn dequeue(&mut self, now: Instant) -> Option<QueueEntry> {
        let mut deferred = Vec::new();

        let result = loop {
            match self.heap.pop() {
                None => break None,
                Some(entry) => {
                    if let Some(state) = self.backoff.get(&entry.bead_id) {
                        if !state.is_eligible(now) {
                            deferred.push(entry);
                            continue;
                        }
                    }
                    self.in_queue.remove(&entry.bead_id);
                    break Some(entry);
                }
            }
        };

        // Put deferred entries back
        for entry in deferred {
            self.heap.push(entry);
        }

        result
    }

    /// Record a backoff for a failed bead.
    pub fn record_backoff(&mut self, bead_id: &str, retries: u32, now: Instant) {
        self.backoff
            .insert(bead_id.to_string(), BackoffState::new(retries, now));
    }

    /// Check if a bead has exceeded max retries.
    pub fn is_deadlettered(&self, bead_id: &str) -> bool {
        self.backoff
            .get(bead_id)
            .is_some_and(|s| s.exceeded_max())
    }

    /// Get retry count for a bead.
    pub fn retries(&self, bead_id: &str) -> u32 {
        self.backoff.get(bead_id).map_or(0, |s| s.retries)
    }

    /// Number of entries currently in the queue.
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    /// Check if a bead is already enqueued.
    pub fn contains(&self, bead_id: &str) -> bool {
        self.in_queue.contains(bead_id)
    }

    /// Clear backoff state for a bead (e.g., on successful completion).
    pub fn clear_backoff(&mut self, bead_id: &str) {
        self.backoff.remove(bead_id);
    }
}

/// Compute a triage score for a bead. Higher = more urgent.
///
/// Composite scoring (all factors 0.0–1.0):
/// - priority_score (0.4): lower priority number = higher score
/// - dependency_score (0.3): 1.0 if no deps, 0.0 if blocked
/// - age_score (0.2): ages up over 1 week
/// - retry_penalty (0.1): diminishing returns on retries
pub fn triage_score(bead: &Bead, retries: u32, now: chrono::DateTime<chrono::Utc>) -> f64 {
    let priority_score = 1.0 - (bead.priority as f64 / 5.0);

    let dependency_score = if bead.dependency_count == 0 {
        1.0
    } else {
        0.0
    };

    let age_hours = (now - bead.created_at).num_hours().max(0) as f64;
    let age_score = (age_hours / 168.0).min(1.0); // 168h = 1 week

    let retry_penalty = 1.0 / (1.0 + retries as f64);

    0.4 * priority_score + 0.3 * dependency_score + 0.2 * age_score + 0.1 * retry_penalty
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn make_bead(id: &str, priority: u8, deps: u32, created_at: &str) -> Bead {
        Bead {
            id: id.to_string(),
            title: format!("bead {id}"),
            description: String::new(),
            status: "open".to_string(),
            priority,
            issue_type: "task".to_string(),
            owner: None,
            repo: "test".to_string(),
            created_at: created_at.parse().unwrap(),
            updated_at: created_at.parse().unwrap(),
            dependency_count: deps,
            dependent_count: 0,
            comment_count: 0,
        }
    }

    #[test]
    fn higher_priority_dequeued_first() {
        let mut q = WorkQueue::new();
        let now = Instant::now();

        q.enqueue(QueueEntry {
            bead_id: "low".into(),
            repo: "r".into(),
            score: 0.3,
            enqueued_at: now,
            retries: 0,
            generation: 0,
        });
        q.enqueue(QueueEntry {
            bead_id: "high".into(),
            repo: "r".into(),
            score: 0.9,
            enqueued_at: now,
            retries: 0,
            generation: 0,
        });

        let first = q.dequeue(now).unwrap();
        assert_eq!(first.bead_id, "high");
        let second = q.dequeue(now).unwrap();
        assert_eq!(second.bead_id, "low");
    }

    #[test]
    fn dedup_prevents_double_enqueue() {
        let mut q = WorkQueue::new();
        let now = Instant::now();

        let entry = QueueEntry {
            bead_id: "x".into(),
            repo: "r".into(),
            score: 0.5,
            enqueued_at: now,
            retries: 0,
            generation: 0,
        };

        assert!(q.enqueue(entry.clone()));
        assert!(!q.enqueue(entry));
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn backoff_skips_ineligible() {
        let mut q = WorkQueue::new();
        let now = Instant::now();

        q.enqueue(QueueEntry {
            bead_id: "backed-off".into(),
            repo: "r".into(),
            score: 0.9,
            enqueued_at: now,
            retries: 1,
            generation: 0,
        });
        // Record backoff with future eligibility
        q.record_backoff("backed-off", 1, now);

        // Should skip — still in backoff
        assert!(q.dequeue(now).is_none());
        // Entry should still be in queue
        assert_eq!(q.len(), 1);

        // After backoff expires, should dequeue
        let future = now + Duration::from_secs(120);
        let entry = q.dequeue(future).unwrap();
        assert_eq!(entry.bead_id, "backed-off");
    }

    #[test]
    fn backoff_exponential_growth() {
        let now = Instant::now();

        let b0 = BackoffState::new(0, now);
        let b1 = BackoffState::new(1, now);
        let b2 = BackoffState::new(2, now);
        let b5 = BackoffState::new(5, now);

        // Each retry doubles the delay
        let d0 = b0.next_eligible.duration_since(now);
        let d1 = b1.next_eligible.duration_since(now);
        let d2 = b2.next_eligible.duration_since(now);

        assert_eq!(d0, Duration::from_secs(30));
        assert_eq!(d1, Duration::from_secs(60));
        assert_eq!(d2, Duration::from_secs(120));

        // Should cap at BACKOFF_MAX
        let d5 = b5.next_eligible.duration_since(now);
        assert!(d5 <= BACKOFF_MAX);
    }

    #[test]
    fn max_retries_deadletters() {
        let mut q = WorkQueue::new();
        let now = Instant::now();

        q.record_backoff("doomed", MAX_RETRIES, now);
        assert!(q.is_deadlettered("doomed"));

        q.record_backoff("ok", 1, now);
        assert!(!q.is_deadlettered("ok"));
    }

    #[test]
    fn clear_backoff_resets() {
        let mut q = WorkQueue::new();
        let now = Instant::now();

        q.record_backoff("x", 3, now);
        assert_eq!(q.retries("x"), 3);

        q.clear_backoff("x");
        assert_eq!(q.retries("x"), 0);
        assert!(!q.is_deadlettered("x"));
    }

    #[test]
    fn triage_score_priority_dominates() {
        let now = chrono::Utc::now();
        let high = make_bead("h", 0, 0, "2026-03-12T00:00:00Z");
        let low = make_bead("l", 4, 0, "2026-03-12T00:00:00Z");

        let sh = triage_score(&high, 0, now);
        let sl = triage_score(&low, 0, now);
        assert!(sh > sl, "P0 should score higher than P4: {sh} vs {sl}");
    }

    #[test]
    fn triage_score_blocked_beads_low() {
        let now = chrono::Utc::now();
        let ready = make_bead("r", 2, 0, "2026-03-12T00:00:00Z");
        let blocked = make_bead("b", 2, 3, "2026-03-12T00:00:00Z");

        let sr = triage_score(&ready, 0, now);
        let sb = triage_score(&blocked, 0, now);
        assert!(
            sr > sb,
            "ready should score higher than blocked: {sr} vs {sb}"
        );
    }

    #[test]
    fn triage_score_age_increases() {
        let now = chrono::Utc.with_ymd_and_hms(2026, 3, 19, 0, 0, 0).unwrap();
        let old = make_bead("old", 2, 0, "2026-03-05T00:00:00Z"); // 14 days ago
        let new = make_bead("new", 2, 0, "2026-03-18T00:00:00Z"); // 1 day ago

        let so = triage_score(&old, 0, now);
        let sn = triage_score(&new, 0, now);
        assert!(so > sn, "older bead should score higher: {so} vs {sn}");
    }

    #[test]
    fn triage_score_retries_penalize() {
        let now = chrono::Utc::now();
        let bead = make_bead("x", 2, 0, "2026-03-12T00:00:00Z");

        let s0 = triage_score(&bead, 0, now);
        let s3 = triage_score(&bead, 3, now);
        assert!(s0 > s3, "0 retries should score higher than 3: {s0} vs {s3}");
    }

    #[test]
    fn older_entry_wins_tiebreak() {
        let mut q = WorkQueue::new();
        let now = Instant::now();
        let later = now + Duration::from_secs(1);

        q.enqueue(QueueEntry {
            bead_id: "newer".into(),
            repo: "r".into(),
            score: 0.5,
            enqueued_at: later,
            retries: 0,
            generation: 0,
        });
        q.enqueue(QueueEntry {
            bead_id: "older".into(),
            repo: "r".into(),
            score: 0.5,
            enqueued_at: now,
            retries: 0,
            generation: 0,
        });

        let first = q.dequeue(now).unwrap();
        assert_eq!(first.bead_id, "older");
    }
}
