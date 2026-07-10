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

    #[test]
    fn put_then_get_returns_value_absent_is_none() {
        let mut c = ContextCache::new();
        c.put("k1", "render-A", prov("rosary-1", "sha1"));
        assert_eq!(c.get("k1"), Some("render-A"));
        assert_eq!(c.get("missing"), None);
        assert_eq!(c.len_valid(), 1);
    }
}
