# Bead aggregation + skill design — working findings (2026-07-27)

**Status:** working notes, deliberately uncommitted. Not a spec, not an ADR.
**Purpose:** capture what a live session found so the mechanism can be designed
from measurements rather than intuition.

Every number here was measured, not estimated. Commands are included so each
claim is re-runnable and falsifiable.

---

## 1. Measurements

### 1.1 Aggregation coverage (rosary)

```
open beads                                460
  with a thread assignment                167  (36%)
  ORPHANED (no thread, no decade)         293  (64%)
```

```bash
# reproduce
rsry bead export --jsonl -o /tmp/open.jsonl
sqlite3 ~/.rsry/backend.db "select bead_id from thread_members where repo='rosary'"
```

Threads for rosary: **94** across **21** decades. Of those, **20 threads share the
single name `SharedScope cluster`**, all under `decade_id='backlog'`, with
generated ids of the form `backlog/{beadA}-{beadB}`.

### 1.2 The backlog inbox is half-built, by design

`src/reconcile/threading.rs:40-42` states the intent verbatim:

```rust
// The "backlog" name signals these are auto-clustered beads
// awaiting triage (by a triage-agent or a human), rather than
// ignore-me dead-letters.
```

The decade row agrees: `Backlog: auto-clustered beads awaiting triage`.

- **Built:** `auto_thread` clusters open beads (Sequential | SharedScope) and
  files them into `backlog/*` threads.
- **Not built:** any triage-OUT path. Nothing promotes a bead from `backlog`
  into a real decade. `rsry thread-reparent` exists but nothing calls it
  systematically.

Result: an inbox with no exit accretes forever. The 20 duplicate threads are
the symptom, not the disease.

### 1.3 Two `backlog` concepts that want to be one

| Concept | Where | Population |
|---|---|---|
| `BeadState::Backlog` | `src/bead.rs:22`, legal `Backlog → Open` transition | **0 beads** |
| `decade_id='backlog'` | `src/reconcile/threading.rs:43` | 20 threads |

The status-level inbox exists in the type system and is entirely unused. The
decade-level inbox is used and has no exit. Neither works.

### 1.4 "Done" beads whose condition still holds — a close-gate defect

Three found in one session. This is a *class*, not a coincidence:

| Bead | Marked | Reality |
|---|---|---|
| `rosary-4ebf52` | done | `docs/git-hooks/post-push` unchanged; still a silent no-op for SQLite stores |
| `rosary-560953` | done | 18 orphaned beads still sitting in `~/.beads/beads.db` |
| `rsry-118421` | done | `bead_ops` exports 4 symbols; `move`/`import`/`update` never unified |

**Mechanical implication:** a close condition that is a *prose assertion* is not
checkable. `has_close_condition()` accepts any non-empty `acceptance_criteria`
string, so "Resolved by X" closes a bead without anything verifying X.

### 1.5 Read-lossy search hides dispatchable work

`search_beads` / `search_beads_fts` (`src/bead_sqlite/mod.rs:1008`, `:1073`)
hand-roll SELECT lists that omit `acceptance_criteria`. `bead_from_row` reads by
name with `.unwrap_or_default()`, so the missing column yields `""` — silently.

`is_dispatchable()` → `has_close_condition()` gates on that field.

```
beads whose close condition lives ONLY in acceptance_criteria:  71
```

Those 71 report non-dispatchable when reached via search, dispatchable via list.

### 1.6 Identity is path-derived at ~30 sites with no chokepoint

- `resolve_beads_dir` (`src/main.rs:1215`) returns early whenever `.beads`
  exists, so its worktree-redirect branch is **unreachable in any repo that
  git-tracks `.beads/`** — which rosary does.
- The same function returns a **relative** path in a main worktree
  (`git rev-parse --git-common-dir` → `.git`, `.parent()` → `""`), resolved
  against process CWD. `standard_hooks_dir` (`:3045`) handles the identical git
  output correctly — the correct implementation already exists in-tree.
- ~30 call sites derive repo identity from `Path::file_name()` with no
  canonicalization. `canonicalize_repo_path` exists and is called from exactly
  two places.

Live phantom stores at time of writing:

```
~/.beads/beads.db                             18 beads (11 open)
~/remotes/art/rosary-f345f6/.beads/beads.db    0 rows   (git worktree)
~/remotes/art/rosary/.beads/.beads/beads.db    0 rows   (nested double-join)
~/remotes/art/rosary/.beads.db                 0 bytes
```

### 1.7 Bead ids are load-bearing in source code

| Bead id | Referenced in |
|---|---|
| `cloister-5183bc` | **17 files** — `wire/cloister.capnp`, `src/wire/codec.ts`, `Taskfile.yml`, tests, fixtures |
| `cloister-df79a5` | 5 — incl. `test/storage/falsifiability.test.ts` |
| `cloister-dfbe92` | 2 — incl. `src/storage/typed-cid.ts` |

**Therefore `rsry bead move` — which regenerates the id — is unsafe for any bead
whose id has escaped into code.** There is no warning; the references simply rot.

---

## 2. The mechanism these measurements imply

### 2.1 Renaming and signing are the same problem

Today the id **is** the identity. So:

- `bead move` regenerating an id = an identity change = every signature,
  attestation, or content-address over that bead is void.
- A rename therefore *cannot* survive a substrate change.

Under ADR-0020, `{repo}-{6hex}` demotes to a **human alias** and identity becomes
the digest of an immutable genesis blob. Rename becomes an alias edit; BeadId is
unchanged; signatures survive **by construction**.

Two independent lines of evidence force the same conclusion:
1. the phantom-store failure (location-keyed identity loses beads), and
2. the signing requirement (name-keyed identity voids attestations on move).

Related: `rosary-f0af8f` (content-hash bead IDs), `ley-line-open-9d30ac`
(Σ Merkle-CAS substrate decade), `ley-line-open-d274a4` (co-attested Head).

### 2.2 Aggregation must be extractable, not inferred

The reason 293 beads are orphaned is that routing information exists only as
prose in `description`. Nothing can route mechanically from prose.

**Proposal: a structured title contract**, conventional-commits shaped, so
routing is parsed rather than guessed:

```
<type>(<scope>)[!]: <summary>            ; core, required
```

with routing metadata in declared fields rather than prose:

| Field | Source today | Should be |
|---|---|---|
| decade / thread | inferred by clustering | declared, validated against existing decades |
| cross-repo dep | prose mention of an id | typed edge (`depends_on`), surfaced as a GitHub URI |
| close condition | free text | typed: `command:` / `pr-merge` / `criteria:` |

**Design rule: enrich on read, gate on write only for *typed* fields.**
Rejecting a malformed human title is hostile; failing a bead that declares a
*nonexistent decade* is mechanical and correct. Gate what is checkable, parse
what is not.

### 2.3 Invariants a substrate can check without judgment

These are the mechanical gates the findings above justify. Each is
CI-checkable and needs no reviewer opinion:

| # | Invariant | Catches |
|---|---|---|
| I1 | Every read of a bead uses the single canonical column list | §1.5 read-lossy search |
| I2 | `{write cols} == {read cols} == {export keys} == {create args}` modulo declared exemptions | ADR-0021 field drift |
| I3 | Every store-location resolution flows through one seam; no raw `join(".beads")` | §1.6 phantom stores |
| I4 | Every repo-identity derivation flows through one seam; no bare `file_name()` | §1.6 basename identity |
| I5 | A bead may not be closed while its close condition is machine-checkable and failing | §1.4 false-done |
| I6 | No bead may reference a decade/thread that does not exist | orphan accretion |
| I7 | Orphan rate (beads with no thread) must not increase vs baseline | §1.1, ratchet-style |
| I8 | Every CLI bead verb has an MCP counterpart and vice versa | §1.7 `move` is CLI-only |

I7 deliberately mirrors the mache smell-gate ratchet: a committed baseline that
may improve but never regress, rather than a threshold argued about per-PR.

### 2.4 Skill design: make the loop mechanical

The triage-out path (§1.2) is the missing half. Shape it so an agent does the
*proposing* and a human does the *deciding* — neither does the other's job.

```
1. DETECT   (mechanical)  orphan beads; duplicate-named threads; stale inbox entries
2. PROPOSE  (agent)       for each orphan, a routing proposal + evidence + confidence
3. DECIDE   (human)       accept / redirect / defer, in batch
4. APPLY    (mechanical)  thread-reparent + record the decision as provenance
5. RATCHET  (mechanical)  orphan-rate baseline updated; regressions fail CI
```

Constraints this session's evidence imposes on that loop:

- **Step 4 must be available over MCP.** Today `move` is CLI-only (§1.7), so an
  agent literally cannot execute the fix it proposes. `I8` closes this.
- **Step 4 must not rename.** Until identity is content-addressed (§2.1), any
  re-homing that changes an id breaks source references. Either re-home without
  renaming, or land identity-first before automating re-homing.
- **Step 2 must cite evidence, not vibes.** A routing proposal should name the
  files/threads/deps it inferred from, so step 3 is a review rather than a
  guess. Cheap version: reuse `epic::cluster_beads` signals and print them.
- **Step 3 must be batchable.** 293 orphans is not a one-at-a-time interaction.

### 2.5 Sequencing that falls out

Nothing here argues for a big-bang. The dependency order is forced:

```
deterministic export (rosary-afdc19)         ← prerequisite for any CI sync
        ↓
hook repair (rosary-4ebf52 / 25e28d)         ← stop active data loss
        ↓
one reader / one writer (ADR-0021 s1-2)      ← kills read-lossy + field drift
        ↓
identity layer (ADR-0020 P1)                 ← rename survives; signing survives
        ↓
triage-out loop (rosary-171a4d)              ← safe to automate re-homing
```

`cargo-mutants` belongs at the third step and after, scoped to the seams that
fail *silently*: `resolve_beads_dir`, `SqliteBeadStore::connect`,
`workspace::repo_key`, `handlers::repo_name_from_path`, `resolve_bead_prefix`,
`config::discover_repo_root`. Exclude `default_worktree_base` and
`legacy_workspace_root` — both branch on `cfg!(test)`, so their mutants are
unkillable by construction and would pollute the MISSED count.

---

## 3. Open questions this does not answer

1. **home vs residence.** ADR-0020 open Q7. A root store makes *every* bead
   materialized away from its `home` claim, so the edge case becomes the normal
   case. Unresolved.
2. **Query-index freshness budget.** ADR-0020 open Q8. How stale may a
   projection be before a read must force a rebuild? This is the cache-
   invalidation contract and it is undecided.
3. **In-tree vs out-of-tree state.** `docs/prior-art/state-sync-sota.md:284-294`
   names this "a real fork" and leaves it open. The tracked `beads.jsonl` is the
   outlier design among five surveyed systems — chosen deliberately for PR
   reviewability.
4. **Canonicalization scheme naming is NOT ours.** Substrate-owned by LLO
   (`ley-line-open-b67a73`, PartitionSpec). Ship deterministic ordering; take the
   tag from LLO. Do not invent a sixth incommensurable root.
5. **CAS ≠ CDC.** rosary does content *addressing* (proven aligned via
   `rosary-bf6c74` golden vectors) and chunks nothing. Keep the words apart.

---

## 4. Cross-references

- `rosary-171a4d` — backlog as a real triage inbox (filed this session)
- `rosary-92694b` / `927e9f` / `9293e9` — P1 epics grouping 16 orphaned bugs (filed this session)
- `rosary-cb1af4` — BeadQuery consumed by MCP, CLI, REST, mache
- `rosary-d954d6` — unify MCP tools + CLI commands from a single source
- `rosary-1a4c0b` — CLI↔MCP parity conformance harness
- `rosary-f0af8f` — content-hash bead IDs
- `ley-line-open-df6402` — LLO schema-spec ↔ rosary tool-registry ↔ cloister surfaces
- `ley-line-open-b67a73` — PartitionSpec (owns canonicalization naming)
- ADR-0020 — findability by identity
- ADR-0021 — single-source the bead field lifecycle
