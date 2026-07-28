# Property-test map

**Status:** working checklist. Measured 2026-07-28.
**Epic:** `rosary-c1f669` (close the re-declaration class) — this map is its sibling: that epic removes duplicate *declarations*, this one removes untested *invariants*.

## Why this exists

`#434` added one property test (`import(export(b)) == b`) and it found a second
bug on its first run. That is not luck: every field-loss defect in this repo was
written by someone who had example-based tests, because an example only covers
the cases its author thought of — the same enumeration that went wrong.

This map lists where that technique pays next, ordered by **value to core bead
and task management first**, with integrations deliberately last.

## What makes a good target

A property is worth writing when the module claims a *law* rather than a
behaviour. Five shapes cover almost everything here:

| shape | law | where |
|---|---|---|
| round-trip | `decode(encode(x)) == x` | export/import, migrate, backup, diff |
| idempotence | `f(f(x)) == f(x)` | import, close-merged, init, publish, backfill |
| order-independence | `fold(shuffle(xs)) == fold(xs)` | the observation lattice |
| algebraic laws | associativity, commutativity, idempotence | the four CRDT algebras |
| invariant preservation | no operation sequence reaches a bad state | state machine, dependency graph |

**The rule that generalises** (from `src/parity`): derive the check from the
*authority*, never from a copy. A property that enumerates its own cases is an
example test wearing a costume.

## Substitute, don't accumulate

The default is **replacement**, not addition. Where a property subsumes a set of
examples, the examples go — keeping both is duplication, and duplicated
assertions rot in the usual way: one gets updated, the other quietly stops
meaning anything.

### Substitute when the example samples a law

If a test name generalises — `idempotent`, `reorder_invariant`, `associative`,
`add_order_invariance` — it is one sampled point of a universally-quantified
claim. One property covers every point, including the ones nobody wrote.

### Keep when it is not a law

Four cases, and they are not rare:

1. **It pins an incident.** A test naming a bead preserves *why we care*, which
   a property cannot express. `derived_from_is_lost_on_round_trip` (#434) is the
   pattern: the property proves the law, the example carries the history.
2. **It specifies a chosen answer at a boundary.** `chain_max_empty_returns_dispatched`
   asserts a *decision* — that empty means `Dispatched` — not a law. A property
   would have to encode the same constant, so the example IS the specification.
3. **It is an error or negative path.** `*_type_mismatch_errors` checks that
   invalid input is rejected. Generating "arbitrary invalid" is awkward and
   usually degenerate; the example is clearer and stronger.
4. **The generator will not reliably reach it.** Rare shapes deserve a named
   example even when a property nominally covers them.

### Worked classification: the four algebras

**Corrected 2026-07-28 after doing it.** The first version of this table claimed
13 law-sampling tests (the table actually listed 12 — an off-by-one) and was
wrong in a way that matters: it lumped *semantic* assertions in with *laws*.

Only **6** are genuinely subsumed, all of them order-invariance:

| substituted | subsumed by |
|---|---|
| `chain_max_associative_under_reorder` | `fold_is_invariant_under_permutation` |
| `reorder_invariant` (flat) | ″ |
| `lww_reorder_invariant` | ″ |
| `lww_tiebreak_total` | ″ (the generator collapses timestamps onto shared instants, so ties are exercised — which *is* the totality claim) |
| `lww_tiebreak_same_source_same_ts_is_total` | ″ |
| `or_set_add_order_invariance` | ″ |

Everything else stays, for two reasons the first pass missed:

- **Semantics are not laws.** `chain_max_monotone`, `lww_picks_latest`,
  `top_absorbs_under_more_distinct_values` and `or_set_unique_tags` assert
  *which value wins*. The properties assert only that order and repetition do
  not matter — a fold that always returned the same wrong answer would satisfy
  every property and fail every one of these. `or_set_unique_tags` in particular
  encodes ADR-0010 invariant 7 (identity is `(source, event_id)`, not value
  text); deleting it would have silently dropped an ADR invariant.
- **Algebra-level idempotence moved layers.** `chain_max_idempotent` and
  `idempotent_under_dedup` are not covered by the new properties, because
  duplicate-suppression turned out to be `ObservationLog`'s contract
  (invariant 8), not the algebra's — see `src/observation/laws.rs`. They stay.

The lesson generalises, and is the reason the "keep" list above exists: it is
much easier to over-apply substitution than to under-apply it, because a
property *looks* like it covers a test whose name sounds similar. Read what the
example asserts, not what it is called.

### The guard

Run `task coverage` **before and after** each substitution and put both numbers
in the commit message. It ratchets per-file line coverage against
`docs/coverage-baseline.json`, and the Taskfile describes it as *"Local-first:
run before/after a decomposition to prove no regression"* — this is that.

**Correction (2026-07-28), because the first version of this section was wrong
in a way that mattered.** It said coverage was "not CI-enforced". It *is* wired
into CI on every PR — and it has been **green while enforcing nothing** since the
day it was built.

The mechanism, verified against the workflow logs: `coverage.yml` regenerates a
CI-native baseline on push-to-main and `git push`es it at main; the `main` branch
ruleset blocks that push; the failure was swallowed by `git push || echo
"::warning::…"` so the job reported success; the committed baseline therefore
remains the dev-machine one from #345, with no `env` key; and
`scripts/coverage-ratchet.py` skips enforcement whenever `CI` is set and the
baseline is not `env: ci`. Every "coverage ratchet ✓" on #430–#435 was that skip.

This is the same shape as the permission rail mutation testing exposed earlier:
a gate whose only reachable outcome is "pass". A green tick that means nothing is
worse than no tick, because it is trusted.

**Substitution work must not start until this is armed** (`rosary-f78208`) —
deleting example tests is safe only if something actually proves coverage did not
drop. `task coverage` also still exits 0 when `cargo-llvm-cov` is absent locally.

Even armed, line coverage is a floor, not a proof.

A property can execute the same lines while asserting strictly more, so coverage
catches the accident — a deleted example whose branch nothing else reaches — not
the subtlety.

## Current coverage, measured

```
src/bead.rs              1492 loc   43 tests
src/epic.rs              1063 loc   36 tests
src/pipeline.rs           568 loc   24 tests
src/bead_migrate.rs       706 loc   13 tests
src/bead_ops.rs           434 loc   10 tests
src/bead_diff.rs          425 loc    9 tests
src/import.rs             493 loc    6 tests
src/restore/merge.rs      252 loc   14 tests (in merge/tests.rs)
src/bead_sqlite/mod.rs   1422 loc    0 tests
```

Density is not the problem. **Kind** is: all of the above are example-based.

---

## Tier 1 — core bead + task correctness

Do these first. Each guards state that, if wrong, silently corrupts work
tracking rather than failing loudly.

### 1.1 CRDT algebra laws — the four per-field algebras ⬅ **start here**

`src/observation/algebra_{chain,flat,lww,orset}.rs`

The lattice's convergence guarantee rests on each per-field algebra being
associative, commutative and idempotent. Measured law coverage today:

| algebra | idempotent | commutative | associative |
|---|---|---|---|
| chain-max | 1 | **0** | 1 |
| flat-lattice | 1 | **0** | 2 |
| LWW-register | **0** | **0** | 1 |
| OR-set | **0** | **0** | 1 |

**Zero commutativity tests exist for any algebra.** Yet
`src/observation/integration_tests.rs:650` justifies convergence-under-partition
with the comment *"Per-field algebras are commutative + idempotent"* — a claim
asserted in prose and tested nowhere.

This matters now, not eventually: ADR-0010 R4b (`rosary-a66b3a`) promotes the
lattice toward being the **source of truth for bead status**. Status is the core
of task management. A non-commutative merge means two agents observing the same
bead in different orders disagree about whether it is done — and `rosary-e0e19f`
already showed what a wrong terminal status costs when it cannot be undone.

Property: for arbitrary observation sets, `merge` is ACI, and the fold is
invariant under permutation.

### 1.2 Fold order-independence

`src/observation/fold.rs`, `src/observation/tree_fold.rs`

`convergence_under_partition` exists as one example. The property is that ANY
partition and ANY interleaving of the same observation set folds to the same
result — including the cross-source flat-lattice step for `Status`, and the
Decade ⊃ Thread ⊃ Bead rollup, where a child ordering must not change the
parent's derived state.

### 1.3 3-way JSONL merge driver

`src/restore/merge.rs` (14 example tests)

This is a **git merge driver** (`rosary-f9516f`) — the class of code where a bug
silently eats committed work rather than erroring. Laws it should satisfy for
arbitrary bead sets:

- `merge(O, A, A) == A` and `merge(O, O, B) == B`
- no record present in any of O/A/B is ever absent from the output
- output is id-sorted (the diff-stability contract in `import.rs`)
- conflict is raised **iff** both sides changed the same bead id differently
- never silently picks a winner

### 1.4 Bead state machine

`src/bead.rs` (`BeadState`, `valid_transitions`, `can_transition_to`)

`BeadState::Done => &[]` makes Done absolutely terminal — which is precisely how
`rosary-e0e19f` became unrecoverable, needing a raw `UPDATE` on `beads.db`.

Property: over arbitrary transition sequences, no state is reachable that has no
path to a terminal state, and every state a *correction* might need to reach is
reachable by *some* legitimate operation. This is as much a design audit as a
test — it converts "Done is a trap" from an incident into a stated, checked fact.

### 1.5 Create-path field fidelity

`src/bead_ops.rs`, `src/bead_sqlite/mod.rs` — tracked as `rosary-c7126b`

`#434` deliberately measures only the export/import boundary, comparing what the
*source store holds* against what came back. The other half — does
`create → read` preserve everything handed in? — is untested, and
`rosary-4887d0` (acceptance_criteria dropped on create) is the proof it can
fail. Note `src/bead_sqlite/mod.rs` has **0 in-file tests** at 1422 loc.

### 1.6 Migration fidelity

`src/bead_migrate.rs` — relates to `rosary-3a0e19` (closed)

`verify_migration` compares field-by-field via a hand-written `same!` list —
the sixth copy of the canonical field set. Property: for an arbitrary bead,
Dolt→SQLite migration preserves every field. Becomes near-free once the
extension registry (`rosary-c47ca6`) lands, since the comparison derives.

---

## Tier 2 — cross-repo semantics

The second thing that "sucks to deal with normally", per the goal.

### 2.1 Dependency-graph acyclicity

`src/store.rs:804` rejects an edge that would create a **same-repo** cycle;
`same_repo_cycle_rejected` tests one example. Property: for any sequence of
`add_dependency` calls, the accepted subset contains no same-repo cycle — and
ADR-0009's stratified rule for *cross*-repo edges holds too, which no test
currently exercises at all.

### 2.2 Semantic dedup + file-overlap detection

`src/epic.rs` — `is_dominated_by:370`, `has_file_overlap:419`

These gate dispatch: a false negative on `has_file_overlap` means two agents
edit the same file concurrently, which is the failure mode behind the
worktree data-loss incident. Properties: overlap detection is symmetric and
never misses a genuine shared path; domination is not vacuously true for
distinct beads.

### 2.3 Diff completeness

`src/bead_diff.rs` — property: `diff(a, a)` is empty, and a change to ANY single
canonical field is reported. Field-generic already, so this mainly guards
against the report silently narrowing.

---

## Tier 3 — integrations (deliberately later)

`src/linear.rs`, `src/linear_tracker.rs`, `src/github_mirror.rs`,
`src/serve/github_webhook.rs`, `src/sync.rs`.

These matter, but they are *mirrors* of the core: if a bead's state is right and
the sync mapping is wrong, the bead is still right. If the core is wrong,
everything downstream is confidently wrong. Revisit after Tier 1.

---

## Rank by measured strength, not by test kind

**Added 2026-07-28 after doing 1.3.** This map ranked the merge driver #2 on the
reasoning that "14 examples and no laws" meant weak coverage. Mutation testing
said otherwise: four mutations injected into `merge_contract` (silent winner,
dropped additions, false conflict on identical adds, asymmetric resurrection) —
every real one was caught by the *existing examples*, and no mutation was found
that the new properties catch and the examples miss. One mutation turned out to
be inert (an earlier match arm short-circuited it), which is itself a reminder
that an uncaught mutation is not automatically a coverage gap.

So the properties there were worth landing — they quantify over shapes nobody
enumerated, and the symmetry law has no example equivalent — but they did not
find a defect, and **zero examples were substituted**, because mutation testing
showed all 14 pull their weight.

The correction: **test kind is not a proxy for test strength.** Ranking a module
as under-tested because its tests are example-based sends effort at whatever is
merely old-fashioned rather than whatever is actually weak. Mutation testing is
the cheap discriminator and should run *before* a module is ranked, not after.
Contrast the algebras (1.1), where the properties found a real gap on the first
run, and the round-trip (#434), which found a second bug immediately.

## Sequencing

1. **1.1 algebra laws** — highest value, smallest surface, and the lattice is on
   the path to owning status
2. **1.3 merge driver** — a git merge driver with no laws tested is the biggest
   silent-corruption risk on the list
3. **1.4 state machine** — cheap, and it retires a live incident class
4. **1.2 fold order-independence** — follows naturally from 1.1
5. **1.5 / 1.6** — largely fall out of `rosary-c47ca6` (extension registry)
6. Tier 2, then Tier 3
