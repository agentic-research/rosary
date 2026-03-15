# ADR-001: Harmony Lattice Decomposition

**Status:** Proposed
**Date:** 2026-03-14
**Author:** James Gardner + Claude
**Repo:** agentic-research/bdr (rosary workspace member)

## Context

Architecture Decision Records (ADRs) exist across 60+ files in two GitHub orgs (jamestexas, agentic-research). They capture decisions but don't connect to actionable work. Meanwhile, rosary's bead system tracks atomic work items but lacks the "why" — the narrative that ties beads into coherent initiatives.

Every agentic orchestration system (Symphony, Codex, Claude Code worktrees) is repo-scoped. Rosary is unique in crossing repo boundaries via threads. But there's no top-level organizing primitive above threads — nothing that says "these 15 beads across 4 repos all serve this one architectural decision."

Session boundaries create another gap: agents produce "next steps" lists that are essentially beads being born, but the temporal context (what was tried, what failed, what was learned) is lost.

## Decision

**BDR (Bead Decision Record) is a Rust crate in the rosary workspace that uses OpenAI's Harmony token format (`openai-harmony = "0.0.8"`) to define a 3-tier lattice for decomposing decisions into actionable work.**

### The Lattice

Harmony's 3-channel assistant architecture maps directly:

| Harmony Channel | BDR Tier | Purpose | Visibility |
|----------------|----------|---------|------------|
| `analysis` | **Decade** | ADR-level reasoning, design rationale, alternatives | Internal — architect/agent |
| `commentary` | **Thread** | Implementation routing, cross-repo refs, tool interactions | Team — developers |
| `final` | **Bead** | Atomic deliverable: PR, commit, closed issue | External — stakeholders |

### Why Harmony

1. **Rust + Apache 2.0** — fits the rosary workspace natively
2. **Serialization for free** — `HarmonyEncoding` renders the lattice to/from token streams, loss-free
3. **Streaming** — `StreamableParser` parses BDR structures incrementally as agents produce output
4. **Interop** — gpt-oss models understand the token format natively; any Harmony-aware system can read BDR
5. **Constraint enforcement** — `<|constrain|>` tokens enforce output format at the token level
6. **Role hierarchy** — `system > developer > user > assistant > tool` provides conflict resolution (decade overrides bead on conflicts)
7. **Routing** — `recipient` field is cross-repo thread routing, already parsed

### Three Flows (Bidirectional)

```
1. Top-down:  ADR markdown → parse atoms → emit beads with channel annotations
2. Bottom-up: Bead completions → accrete via <|return|> → update thread/decade state
3. Temporal:  Session "next steps" → new beads with provenance (which session, what failed)
```

### ADR Atoms → Bead Types

| ADR Atom | Bead issue_type | Channel |
|----------|----------------|---------|
| Friction Point | bug / task | decade |
| Decision | task | thread |
| Constraint | task (validation gate) | decade |
| Consequence | task (follow-up) | thread |
| Alternative (rejected) | — (metadata/comment) | decade |
| Open Question | task / feature | thread |
| Implementation Phase | epic | thread |
| Validation Point | review | bead |
| Technical Spec | task | bead |

### Harmony Token Mapping

```rust
// BDR channel config (replaces default analysis/commentary/final)
ChannelConfig::require_channels(["decade", "thread", "bead"])

// Routing: cross-repo thread references
Message { recipient: Some("mache:bead-85t".into()), channel: Some("thread".into()), .. }

// Constraint: bead type enforcement
// <|constrain|> → issue_type: task|bug|feature|epic|review

// Completion: flows up the lattice
// <|return|> on bead channel → thread summary update → decade status update

// Dispatch: flows down the lattice
// <|call|> on thread channel → spawn bead in target repo
```

### Relationship to Existing Systems

| System | Role in BDR |
|--------|-------------|
| **rosary** | Parent workspace. Orchestrates bead dispatch, reconciliation, verification. BDR is a workspace member crate. |
| **mache** | View layer. Projects BDR lattice as navigable filesystem via schema. Existing bead `mache-85t` (beads as browsable filesystem with write-back). |
| **ley-line** | Storage substrate. Arena + SQLite for structured data access. Staging area for atomic multi-node edits. |
| **tropo** | Significance function. Measures whether a decision changed H1 rank (dependency cycles) or created new communities. Decade-level metadata. |
| **assay** | Coverage verification. Extends 4-layer matching cascade to verify: do beads cover all ADR atoms? Are there stale beads with no corresponding decision? Coverage = \|ADR atoms ∩ Beads\| / \|ADR atoms\|, Staleness = \|Beads \ ADR atoms\| / \|Beads\|. |
| **openai-harmony** | Token format. Provides Message, ChannelConfig, HarmonyEncoding, StreamableParser. BDR extends with custom channels. |
| **symphony** | Comparative reference. Similar orchestration model (poll→reconcile→dispatch) but repo-scoped and Linear-only. BDR is user-scoped and multi-repo. |

### Extension to READMEs and General Docs

The same lattice applies beyond ADRs:

- **README sections** decompose into beads: "Getting Started" → task beads for setup validation, "API Reference" → review beads for accuracy
- **CHANGELOG entries** are bead completions viewed temporally
- **Investigation logs** (mache-gsi) are the narrative track — decade-level analysis channel content

Assay's matching cascade generalizes: any markdown document with code references can be decomposed into atoms and matched against beads/entities.

## Consequences

### Positive

- ADRs become actionable — every decision atom maps to trackable work
- Cross-repo narrative coherence — decades group threads across repos
- Temporal continuity — session "next steps" are captured with provenance
- Bidirectional — beads accrete back into living decision documents
- Interoperable — Harmony format means gpt-oss agents read/write natively
- Measurable — assay-style coverage shows decision→implementation completeness

### Negative

- New dependency on `openai-harmony` (0.0.8, pre-1.0) — API may change
- Channel semantics overloaded — Harmony channels were designed for safety filtering tiers, not work decomposition. May need custom rendering logic.
- Decade/thread/bead naming may confuse rosary users who know "decade" as verify.rs tiers — need to rename or clarify

### Risks

- Over-engineering: BDR could become a complex intermediate format nobody uses. Mitigation: dogfood immediately — this ADR is BDR-001, decomposed into beads below.
- Harmony format drift: OpenAI may change the token format. Mitigation: pin version, wrap in trait for future swap.
- Scope creep into assay territory: Keep BDR focused on decomposition/accretion, let assay handle coverage metrics.

## Implementation Plan

### Phase 1: Scaffold (this session)
- Create rosary workspace member crate `bdr`
- Add `openai-harmony` dependency
- Define BDR channel config and message types
- Write ADR parser (markdown → atoms)

### Phase 2: Decompose
- Implement atom → bead mapping
- Wire to rosary's Dolt infrastructure for bead creation
- Cross-repo thread routing via `recipient` field

### Phase 3: Accrete
- Bead completion triggers thread/decade state updates
- Mache schema (mache-85t) projects the lattice as filesystem
- Assay coverage: ADR atoms vs. beads

### Phase 4: Temporal
- Session boundary capture via CC history (mache-kv0)
- "Next steps" extraction → bead creation with provenance
- Donut integration (when available) for topological temporal encoding

## Open Questions

1. Should `decade` be renamed to avoid confusion with rosary's verify.rs tiers?
2. Does the Harmony `StreamableParser` work for non-LLM-generated token streams (i.e., can we use it for parsing our own serialized BDR structures)?
3. How does the accretion direction handle conflicts (two beads claim different outcomes for the same decision)?
4. Should BDR support non-Harmony serialization (e.g., plain JSON) for systems that don't speak Harmony?

## References

- [OpenAI Harmony format](https://developers.openai.com/cookbook/articles/openai-harmony)
- [openai-harmony crate](https://crates.io/crates/openai-harmony)
- [OpenAI Symphony SPEC.md](https://github.com/openai/symphony/blob/main/SPEC.md)
- [ChatML vs Harmony comparison](https://huggingface.co/blog/kuotient/chatml-vs-harmony)
- Rosary architecture: `~/remotes/art/rosary/docs/ARCHITECTURE.md`
- Mache beads schema bead: `mache-85t`
- Mache JSON write-back chain: `mache-b1w`, `mache-b1w.1`, `mache-b1w.2`, `mache-b1w.3`
- Assay coverage types: `~/remotes/art/assay/internal/coverage/types.go`
