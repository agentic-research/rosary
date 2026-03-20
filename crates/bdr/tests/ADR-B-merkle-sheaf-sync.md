# ADR: Merkle Sheaf Sync — Structural Arena Synchronization

**Status:** Proposed\
**Author:** James Gardner\
**Date:** 2026-03-19\
**Repo:** leyline (crates: leyline-net, leyline-fs)\
**Depends on:** ADR-A (Sheaf Cache)\
**Relates to:** leyline-net manifest (Level 2), fountain codes (Level 3), mache community detection

______________________________________________________________________

## Context

Ley-line's current sync model is whole-arena. When a generation changes, the sender transmits the entire inactive buffer to the receiver via fountain codes. The Merkle manifest (Level 2) verifies integrity after transfer but doesn't inform *what* to transfer.

For small arenas (\<100MB) this is fine. For large monorepo arenas or multi-tenant hosted scenarios, transmitting the full arena on every generation bump is wasteful when most communities didn't change.

Ley-line already has:

- Manifest with Merkle root (Level 2) — whole-arena integrity
- RaptorQ fountain codes (Level 3) — loss-tolerant transport
- Content-defined chunking (CDC) — byte-level dedup
- sqlite3_deserialize zero-copy — arena as SQLite database
- Community structure via mache — structural regions within the arena

What's missing: the ability to sync at the *structural region* level, not the byte level.

## Decision

Extend the ley-line manifest to include per-community Merkle stalks. Two ley-line instances compare stalks to determine which communities diverged, then sync only those communities via fountain codes.

### Sync Protocol Extension

```
Current:
  Sender                          Receiver
    |--- TCP: full manifest -------->|
    |--- UDP: full arena (FEC) ----->|
    |<-- TCP: bitmap reconcile ------|
    |--- UDP: missing blocks ------->|

Proposed:
  Sender                          Receiver
    |--- TCP: sheaf manifest ------->|    (per-community stalks)
    |<-- TCP: stale communities -----|    (receiver compares stalks)
    |--- UDP: community deltas ----->|    (FEC only for stale regions)
    |<-- TCP: bitmap reconcile ------|    (per-community)
    |--- UDP: missing blocks ------->|
```

### Sheaf Manifest

```rust
struct SheafManifest {
    /// Whole-arena Merkle root (backward compatible with Level 2)
    arena_root: [u8; 32],

    /// Per-community stalks
    community_stalks: Vec<CommunityStalk>,

    /// Restriction map edges (for receiver to validate consistency)
    restrictions: Vec<RestrictionEntry>,
}

struct CommunityStalk {
    community_id: u32,
    merkle_root: [u8; 32],
    /// Byte range in arena buffer for this community's nodes
    byte_offset: u64,
    byte_length: u64,
    /// Node count (for progress estimation)
    node_count: u32,
}
```

### Bandwidth Reduction

A 50-community arena where one commit touched one community:

- Current: transmit 100% of arena
- Proposed: transmit ~2% of arena (1 community + boundary nodes)

The fountain codes still handle loss tolerance on the reduced payload. The pacing math from `net` crate applies unchanged — just to a smaller transfer.

### Consistency Check on Receive

After receiving community deltas, the receiver:

1. Updates received communities in the inactive buffer
1. Recomputes per-community Merkle stalks
1. Runs d⁰ across restriction map edges to verify boundary consistency
1. If consistent (H⁰ check passes): atomic buffer swap
1. If inconsistent: request full sync for inconsistent communities

This is the same coboundary operator from ADR-A, applied to arena data rather than cache entries.

## Consequences

**Positive:**

- Sync bandwidth proportional to structural change, not arena size
- Fountain codes still provide loss tolerance on the reduced payload
- Backward compatible — fall back to full sync if receiver doesn't support sheaf manifest
- Consistency guaranteed by H⁰ check before buffer swap

**Negative:**

- Manifest size increases (community stalks + restrictions vs single Merkle root)
- Requires mache community structure to be available at the ley-line layer
- Community byte ranges must be contiguous or tracked (may require arena layout changes)

**Risks:**

- Community boundaries may not align with SQLite page boundaries — partial community sync may require extra pages
- Schema changes between sender/receiver could cause community ID mismatch
