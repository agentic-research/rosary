# ADR-0011: Decision-of-Record — authenticated-authority resolution over CRDT conflict detection

- **Status:** Accepted
- **Bead:** rosary-1915e0
- **Supersedes:** the "sheaf-H¹ decision substrate / four-graded-truth stack / causal-intersection-as-gluing" framing (falsified 2026-06-22 by a four-agent adversarial pass — math, empirical, greybeard, prior-art)

## Context

rosary already **detects** multi-source conflict. `src/observation/algebra_flat.rs` joins per-source observations into `FlatLattice::Top { witnesses: Vec<(value, Source)> }` — the conflict element `⊤`, carrying every source's view. This is **Belnap's "Both"** (1977) and **H⁰ / `ker δ⁰`** — *not* H¹. venturi (`agentic-research/venturi`, Go) proves the same CRDT join-semilattice at scale: a commutative/associative/idempotent merge over 14 sources / 392K CVEs, detecting 53K conflicts, surfacing witnesses, and escalating genuine disagreement to human review.

What neither has is **resolution for claims with no natural lattice order**. venturi resolves vuln-state by a *fixed domain order* ("more certainty wins: `reserved < … < fixed`") — which works only because vulnerability state is monotone. General decisions ("use Postgres" vs "use DynamoDB") have **no such order**, so a lattice join cannot pick a winner. They need an **authority**.

### What the adversarial pass established (don't re-litigate)

- The core mechanism (typed contradiction + provenance + supersession) is **prior art**: TMS (Doyle 1979), ATMS (de Kleer 1986), AGM belief revision (1985), Graphiti/Zep (2025) bitemporal-provenance, Belnap (1977). **Do not claim these as novel.**
- The sheaf-H¹ framing is metaphor (no site; wrong cohomology group; the engine's stalks are `f32` code vectors for cache invalidation, never pointed at sources).
- The four-graded-truth stack is a category error (a non-Boolean Heyting algebra admits no probability measure; the right algebra for `[0,1]` confidence is MV/Łukasiewicz).
- sieve's "game-theoretic consensus" resolves by **weighted majority** — three stale auto-ingests outvote one human correction. That is **anti-authority** and must not be reused as-is.

## Decision

Layer **authenticated-authority resolution** on the existing CRDT detection substrate. When the flat lattice returns `⊤`, resolve the witnesses by a discrete **authority rank**, gated by **signet authenticity**.

The single defensible novelty (adversary-confirmed): **bind contradiction resolution to a cryptographically-authenticated asserter.** No surveyed system does this — TMS resolves by structure, AGM by entrenchment, Graphiti by recency, venturi by domain-order, sieve by majority.

### The resolution rule

Given the conflicting observations (witnesses of a `⊤`), each lifted to an `AuthoritativeClaim { observation, authority, authenticated, undercuts }`:

1. **GATE** — keep only `authenticated` claims (signet/quarantine-valid). Unauthenticated claims are **excluded, not down-weighted**. (Gate-not-weight: the CRDT *join* in `algebra_flat` is what must stay idempotent/associative; the gate sits strictly *after* it and never folds into it — whereas a `[0,1]` weight multiplied into the join would break both laws. Resolution itself is a terminal classifier `Set⟨Claim⟩ → Resolution`, not a lattice operation; it is order-independent and idempotent under `payload_hash` dedup — see *Known limitations*.)
1. **UNDERCUT** — drop any claim whose `payload_hash` is undercut (with proof) by an eligible claim. Undercut removes a claim **regardless of its rank** — this is what makes resolution *defeasible*, not last-writer-wins-with-a-priority-key.
1. **RANK** — the highest `Authority` tier among survivors wins. `Authority` is a discrete total order: `AutoIngest < AgentAssertion < Decision < HumanCorrection`. **Timestamp is not a cross-tier tiebreaker** — authority beats recency.
1. **ESCALATE** — if two or more survivors share the top tier, return `Escalate` (venturi's "flag for human"). **Never silently latest-wins.**

### Constraints (load-bearing, from falsification)

- **H⁰ not H¹** — reuse `algebra_flat`; do not add a sheaf.
- **Gate, not weight** — authenticity admits/excludes; it never multiplies into the join.
- **Undercut, not just outrank** — proofs can defeat a higher tier.
- **MV-algebra, not Heyting**, for any graded confidence layer (or use venturi's entropy signal).
- **Cite, don't claim** — bitemporality, speech-acts, supersession, store-both are TMS/ATMS/Graphiti/Belnap. Claim novelty only in *authenticity-as-arbiter* + the content-addressed cross-runtime substrate.

## Structural framing

venturi (detect, CRDT) + sieve (resolve-by-majority) already form a **detect→resolve pair for vulnerabilities**. Decision-of-record is that pair generalized to claims, **swapping majority for authenticated authority**. Where each authority tier comes from (deriving it from observation authorship) is the follow-on (rosary-197eb0); this ADR specifies the resolution mechanism that consumes it.

## Falsifiable acceptance gate

The thesis earns "decision substrate, not memory tool" only if **authority beats recency** on the adversarial stratum — a human correction wins over a later contradicting auto-ingest, and flipping *only* the timestamp does not flip the winner. Phase 1 proves this as a unit test over `resolve_by_authority`. Phase 2 proves it at corpus scale against a Graphiti-class newest-wins baseline (empirical protocol in the falsification record).

## Known limitations (Phase 1)

The math-friend review (2026-06-22) confirmed the design avoids the H¹ error and is a citeable mechanism, and flagged these — all Phase-1 scope boundaries, not holes in the falsifiable core:

- **Idempotence is in the code, not claimed of a lattice.** `resolve_by_authority` dedups claims by `payload_hash` (the G-set dedup key) before the tie count, so a re-delivered identical claim does not flip `Resolved` into `Escalate` (`duplicate_claims_are_idempotent`). Idempotence/associativity *proper* are properties of the `algebra_flat` join, not of this terminal classifier — the ADR does not claim otherwise.
- **Undercut is single-round and proof-trusted.** `undercuts` carries a bare `payload_hash`; the proof is **not yet verified** (Phase 2 / signet). So an authenticated low-tier claim can currently defeat a higher-tier one by naming its hash — an authority-inversion that Phase 2 closes by verifying the undercut's proof. There is no reinstatement (a defeated defeater does not revive its target), and mutual undercut removes both → escalate. This is real defeasible *defeat*, **not** a full Nute/Dung argumentation semantics — do not cite it as such.
- **Authority tier is an input.** Phase 1 takes `authority` as a caller-supplied field; *deriving* the tier from authenticated authorship — the hard, falsifiable part ("is this observation actually a `HumanCorrection`?") — is rosary-197eb0.
- **The total order is adequate only for the four fixed tiers.** Incomparable authority kinds (e.g. `security-owner` vs `module-owner`) would force a *partial* order with escalate-on-incomparable. Deferred.

## Consequences

- New module `src/observation/resolve.rs` — the resolution function over a detected `⊤`. Pure, no new dependencies.
- The authority tier is an *input* in Phase 1 (supplied by the caller); deriving it from authenticated authorship is rosary-197eb0.
- Full signet validation routes through `quarantine.rs` (Phase-1 stub passes all); Phase 2 wires real cert checks.
