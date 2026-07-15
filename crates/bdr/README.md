# bdr — Bead Decision Records

Harmony-lattice decomposition for ADRs: parse an architecture decision record
(markdown + frontmatter) into typed atoms (`TechnicalSpec`, `Constraint`, …),
map atoms to dispatchable bead specs with dependency and target-repo routing,
and group them into the Decade ⊃ Thread ⊃ Bead hierarchy. The reverse direction
(`accrete`) folds bead completions back up into decade state transitions.

Every atom carries a `ProvenanceRef` (`Doc`, `Session`, `Code`, `Meeting`,
`SlackThread`) so generated work items trace back to the sentence that caused
them.

Part of the [rosary](https://github.com/agentic-research/rosary) workspace —
this is the crate behind `rsry decompose` and `rsry capture`. Not published to
crates.io. License: Apache-2.0 (deliberately more permissive than the
workspace's AGPL — bdr is the liberally-licensed decomposition core).

```bash
cargo test -p bdr
```
