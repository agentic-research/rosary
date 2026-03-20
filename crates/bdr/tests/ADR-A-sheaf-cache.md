# ADR: Sheaf Cache — Structurally-Aware Cache Invalidation via Čech Cohomology

**Status:** Proposed\
**Author:** James Gardner\
**Date:** 2026-03-19\
**Repo:** leyline-sheaf (new crate in leyline workspace, extract to standalone when API stabilizes)\
**Relates to:** BREAD (2025), cairn_sheaf.go (x-ray)

______________________________________________________________________

## Context

Cache invalidation is one of two hard problems in computer science. Current approaches are structurally blind:

- **Time-based (TTL):** Serves stale data until expiry. Wastes valid cache after expiry.
- **Key-based (LRU/LFU):** Evicts by access pattern. No understanding of relationships between entries.
- **Full invalidation:** Correctness guaranteed but performance destroyed. One change flushes everything.

None of these understand that cached entries have *structural relationships*. A function's caller graph is related to its callees' caller graphs. A community's analysis is related to its neighboring communities' analyses. When one entry changes, only structurally connected entries *might* need invalidation — and most won't.

We need a cache primitive that invalidates based on structural consistency, not time or access patterns.

## Decision

Introduce `sheaf_cache`: a generic cache whose entries are organized by topological regions with invalidation driven by the Čech coboundary operator d⁰.

### Core Abstractions

**Stalk:** A content hash (Merkle root) summarizing a topological region's current state. Any data structure that can be hashed can produce a stalk.

**Restriction map:** An edge between two regions recording the hash of their shared boundary (shared symbols, shared interfaces, shared types). Weights on each edge encode how tightly coupled the regions are.

**Coboundary operator (d⁰):** Given a change to one region's stalk, d⁰ propagates through restriction map edges to identify which neighbors are now inconsistent.

**H⁰ (cache validity):** ker(d⁰) = the set of cache entries that remain globally consistent after a change. Everything in H⁰ stays cached. Everything outside H⁰ is invalidated.

**Defect:** ||d⁰(stalks)||² measures total structural inconsistency. Zero = entire cache valid. Nonzero = localized to specific boundaries. The defect vector pinpoints exactly which entries are stale.

### Algorithm

```
on_change(changed_regions):
    // 1. Recompute stalks for changed regions (incremental Merkle rehash)
    for region in changed_regions:
        recompute_stalk(region)

    // 2. Walk restriction map edges from changed regions
    for region in changed_regions:
        for (neighbor, restriction) in restriction_edges(region):
            new_boundary = restriction.compute_boundary_hash()
            if new_boundary != restriction.previous_boundary:
                invalidate(neighbor)
                restriction.previous_boundary = new_boundary

    // 3. Bounded cascade (typically 2-3 hops)
    cascade_invalidation(max_depth=3)
```

### Restriction Map Learning

Weights are derived, not configured:

- **From history:** Cross-region variance of co-change frequency. High co-change → loose restriction. Low co-change → tight restriction. Reverted co-changes → very tight.
- **From feedback:** Successful operations reinforce weights. Failed operations tighten them. The restriction map evolves from observed patterns.

This is `computeRestrictionWeights` from cairn_sheaf.go generalized: instead of visual feature variance across DOM zones, compute structural co-change variance across any topological regions.

### Data Structures

```rust
/// Domain-independent sheaf cache.
/// S: Stalk type (must be hashable to [u8; 32])
/// V: Cached value type
pub struct SheafCache<S: StalkHash, V> {
    stalks: HashMap<RegionId, S>,
    restrictions: HashMap<(RegionId, RegionId), RestrictionEdge>,
    entries: HashMap<RegionId, CacheEntry<V>>,
    generation: u64,
}

pub struct RestrictionEdge {
    weights: Vec<f64>,           // learned per-dimension weights
    boundary_hash: [u8; 32],     // hash of shared boundary
    co_change_rate: f64,         // from history
    revert_rate: f64,            // from history
}

pub trait StalkHash {
    fn merkle_root(&self) -> [u8; 32];
}
```

### Complexity

- **Invalidation per change:** O(|affected_regions| × |edges_per_region|). Typically O(1 × 3) = O(3) hash comparisons.
- **Stalk recompute:** O(|changed_leaves|) for incremental Merkle.
- **Memory:** O(|regions| × (32 bytes + sizeof(V))).

### Relation to BREAD (2025)

BREAD proposed SheafCell with hyperbolic embeddings. The embedding step was intractable. This ADR eliminates it: the structural graph IS the topology. Merkle roots ARE the stalks. History variance IS the restriction map. No hyperbolic space. No training. Same coboundary operator. Same H⁰. Same guarantees.

### Relation to cairn_sheaf.go (x-ray)

`BuildCechCoboundary`, `ComputeH0`, `computeRestrictionWeights` are directly reusable with different stalk types. The core coboundary logic should be extracted from x-ray into this crate as the shared implementation.

## Consequences

**Positive:**

- Generic primitive usable by any system with topological structure
- Invalidation is provably minimal — only structurally affected entries evicted
- Defect metric provides quantitative staleness measure
- Restriction maps are self-tuning from observed history

**Negative:**

- Requires topology (community/region structure) to be defined by consumer
- Overhead of Merkle stalk maintenance per mutation
- Cascading invalidation must be depth-bounded to prevent pathological flush

**Risks:**

- Incorrect invalidation (serving stale data) is worse than over-invalidation
- Must be formally verified before production use
