//! Staleness-invalidated, content-addressed context cache (warm-resume Phase B,
//! rosary-a9f5dc). Aligned with LLO's CacheEntry{value,generation,valid}: `get`
//! returns a value only while `valid`; `on_change` is the sole invalidator.

use std::collections::HashMap;

/// What a cached render was derived from — intersected against a `ChangeSet` by
/// `on_change` to decide staleness.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Provenance {
    pub bead: String,
    pub commit_sha: String,
    pub source_refs: Vec<String>,
}

/// A described change to the world. `on_change` invalidates every entry whose
/// `provenance` intersects it. This is the self-contained generation floor —
/// correct with zero leyline present; a leyline cascade refiner (B3) narrows it.
#[derive(Clone, Debug, Default)]
pub struct ChangeSet {
    /// The bead whose state moved (paired with `commit_sha`).
    pub bead: Option<String>,
    /// The new commit sha; entries for `bead` cached at a different sha are stale.
    pub commit_sha: Option<String>,
    /// Source regions (content-hashes) that changed.
    pub source_refs: Vec<String>,
}

impl Provenance {
    /// True if this entry was derived from something the change touched.
    fn intersects(&self, change: &ChangeSet) -> bool {
        // The bead's state moved. With a new sha, only entries at an older sha
        // are stale (an advance); without one, conservatively treat every entry
        // for this bead as stale — a bead-only change must never silently no-op.
        if let Some(bead) = &change.bead {
            if &self.bead == bead {
                match &change.commit_sha {
                    Some(sha) => {
                        if &self.commit_sha != sha {
                            return true;
                        }
                    }
                    None => return true,
                }
            }
        }
        // Any shared source region changed.
        change
            .source_refs
            .iter()
            .any(|r| self.source_refs.contains(r))
    }
}

struct CacheEntry {
    value: String,
    generation: u64,
    valid: bool,
    provenance: Provenance,
}

/// Content-addressed cache with generation-tracked validity.
#[derive(Default)]
pub struct ContextCache {
    generation: u64,
    entries: HashMap<String, CacheEntry>,
}

impl ContextCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Monotone clock advanced by `on_change`; stamped onto entries at `put`.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Insert (or overwrite) a valid entry stamped at the current generation.
    pub fn put(
        &mut self,
        key: impl Into<String>,
        value: impl Into<String>,
        provenance: Provenance,
    ) {
        self.entries.insert(
            key.into(),
            CacheEntry {
                value: value.into(),
                generation: self.generation,
                valid: true,
                provenance,
            },
        );
    }

    /// Return the cached value ONLY while valid — never recomputes freshness at
    /// read time (LLO semantics; invalidation lives solely in `on_change`).
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .get(key)
            .filter(|e| e.valid)
            .map(|e| e.value.as_str())
    }

    /// Count of currently-valid entries — instrumentation for the warmth gate.
    pub fn len_valid(&self) -> usize {
        self.entries.values().filter(|e| e.valid).count()
    }

    /// Bump the generation, mark `valid = false` on every entry whose provenance
    /// intersects `change`, and return the invalidated keys (the cascade a
    /// downstream consumer would evict on its side). The SOLE invalidator.
    pub fn on_change(&mut self, change: &ChangeSet) -> Vec<String> {
        self.generation += 1;
        let mut invalidated = Vec::new();
        for (key, entry) in self.entries.iter_mut() {
            if entry.valid && entry.provenance.intersects(change) {
                entry.valid = false;
                invalidated.push(key.clone());
            }
        }
        invalidated.sort();
        invalidated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prov(bead: &str, sha: &str) -> Provenance {
        Provenance {
            bead: bead.into(),
            commit_sha: sha.into(),
            source_refs: vec![],
        }
    }

    fn prov_refs(bead: &str, sha: &str, refs: &[&str]) -> Provenance {
        Provenance {
            bead: bead.into(),
            commit_sha: sha.into(),
            source_refs: refs.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn put_then_get_returns_value_absent_is_none() {
        let mut c = ContextCache::new();
        c.put("k1", "render-A", prov("rosary-1", "sha1"));
        assert_eq!(c.get("k1"), Some("render-A"));
        assert_eq!(c.get("missing"), None);
        assert_eq!(c.len_valid(), 1);
    }

    #[test]
    fn eviction_completeness_sha_change() {
        // Gate 2: after on_change, the affected entry is gone from get().
        let mut c = ContextCache::new();
        c.put("k", "old-render", prov("rosary-1", "sha1"));
        let evicted = c.on_change(&ChangeSet {
            bead: Some("rosary-1".into()),
            commit_sha: Some("sha2".into()),
            source_refs: vec![],
        });
        assert_eq!(evicted, vec!["k".to_string()]);
        assert_eq!(c.get("k"), None, "stale entry must never be served");
        assert_eq!(c.generation(), 1, "on_change bumps generation");
    }

    #[test]
    fn on_change_spares_unrelated_beads() {
        let mut c = ContextCache::new();
        c.put("k", "render", prov("rosary-1", "sha1"));
        let evicted = c.on_change(&ChangeSet {
            bead: Some("rosary-2".into()),
            commit_sha: Some("shaX".into()),
            source_refs: vec![],
        });
        assert!(evicted.is_empty());
        assert_eq!(
            c.get("k"),
            Some("render"),
            "unrelated change must not evict"
        );
    }

    #[test]
    fn eviction_on_shared_source_ref() {
        let mut c = ContextCache::new();
        c.put(
            "k",
            "render",
            prov_refs("rosary-1", "sha1", &["blobhash-r1"]),
        );
        c.on_change(&ChangeSet {
            bead: None,
            commit_sha: None,
            source_refs: vec!["blobhash-r1".into()],
        });
        assert_eq!(
            c.get("k"),
            None,
            "a changed source ref must evict its derivations"
        );
    }

    #[test]
    fn freshness_soundness_valid_never_lies() {
        // Gate 1: after a scripted sequence, every valid entry's provenance does
        // NOT intersect any applied change.
        let mut c = ContextCache::new();
        c.put("a", "ra", prov("rosary-1", "sha1"));
        c.put("b", "rb", prov("rosary-2", "sha1"));
        c.put("d", "rd", prov_refs("rosary-3", "sha1", &["r-shared"]));
        let changes = [
            ChangeSet {
                bead: Some("rosary-1".into()),
                commit_sha: Some("sha2".into()),
                source_refs: vec![],
            },
            ChangeSet {
                bead: None,
                commit_sha: None,
                source_refs: vec!["r-shared".into()],
            },
        ];
        for ch in &changes {
            c.on_change(ch);
        }
        // "a" (rosary-1 moved) and "d" (r-shared changed) must be gone; "b" survives.
        assert_eq!(c.get("a"), None);
        assert_eq!(c.get("d"), None);
        assert_eq!(c.get("b"), Some("rb"));
        // Invariant (Gate 1): no still-valid entry's provenance intersects any
        // applied change — checked against the actual surviving entries.
        for entry in c.entries.values().filter(|e| e.valid) {
            for ch in &changes {
                assert!(
                    !entry.provenance.intersects(ch),
                    "a valid entry's provenance intersects an applied change"
                );
            }
        }
    }

    #[test]
    fn on_change_bead_only_invalidates_all_entries_for_that_bead() {
        let mut c = ContextCache::new();
        c.put("k", "render", prov("rosary-1", "sha1"));
        c.put("other", "r2", prov("rosary-2", "sha1"));
        let evicted = c.on_change(&ChangeSet {
            bead: Some("rosary-1".into()),
            commit_sha: None,
            source_refs: vec![],
        });
        assert_eq!(
            evicted,
            vec!["k".to_string()],
            "bead-only change must invalidate that bead's entries"
        );
        assert_eq!(c.get("k"), None);
        assert_eq!(c.get("other"), Some("r2"), "other beads unaffected");
    }
}
