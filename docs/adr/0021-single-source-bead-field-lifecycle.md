# ADR-0021: Single-source the bead field lifecycle — one field set, every surface projects from it

**Status:** Proposed
**Date:** 2026-07-14
**Repo:** rosary

**Relates to:**
- Local: [ADR-0006](0006-declarative-tool-registry.md) (declarative tool registry — unify MCP/CLI/pipeline from a single source; this ADR is the *data-model* half of the same principle), [ADR-0014](0014-decouple-rosary-from-bd.md) (own the store — we own the bead format, so we own the duty to define it once), [ADR-0020](0020-findability-by-identity.md) (identity/storage derive from a genesis blob — orthogonal axis: 0020 single-sources a bead's *identity*, this ADR single-sources a bead's *field set*).
- Evidence beads (all real, not hypothetical): `rosary-4887d0` (`acceptance_criteria` dropped by a create surface — a field the field-set didn't enforce), `rosary-eadfbe` (empty-store family — a *read* returning less than the truth), `rosary-3a0e19` (Dolt→SQLite migration — was about to become the *seventh* hand-rolled field list).

## Context

There is no single definition of "a bead's fields and how they round-trip."
The field set is re-declared, by hand, at every surface that touches a bead —
and the copies have already drifted. Concretely, in the tree today:

1. **Two divergent writers.** `create_bead` and `create_bead_full`
   (`src/bead_sqlite/mod.rs`) are two `INSERT INTO issues` statements with
   *different* column lists (the former omits `created_by`/`scope`).
2. **Three divergent readers.** `list_beads` selects `acceptance_criteria`;
   `get_bead` and `list_beads_scoped` do **not**. So `rsry bead list` shows a
   bead's close condition and `get_bead` silently returns it empty — the
   "read-lossy" half of `rosary-eadfbe`'s family.
3. **Hand-wired surface mapping.** The MCP create handler extracts each field
   with `args.get("acceptance_criteria")` (`src/serve/handlers/mod.rs`) — one
   forgotten line drops a field for *that* surface only. That shape is exactly
   how `rosary-4887d0` happens, for any field.
4. **A partial single-source.** `bead_ops::BeadCreateArgs` correctly unifies
   CLI↔MCP *create* — but only create. Read, export (`src/import.rs`), and any
   future migration each carry their own field list.

`rosary-4887d0` (create drops a field), the read-lossy trait (`get_bead` omits
a field), and `rosary-3a0e19` (a migration that would hand-roll the field list
a seventh time) are **not three bugs. They are one defect surfacing three
ways:** the canonical field set exists only implicitly, so every surface
re-derives it and the derivations rot apart. Adding a migration as written
would not fix this — it would add a seventh copy to drift.

## Decision

Define the bead field set **once**, and make every surface *project* from that
definition rather than re-declare it.

1. **One canonical record.** A single type is the authority for "the fields a
   bead has" (the existing `Bead` is nearly it). Every other representation —
   store rows, MCP/CLI args, export JSON — is a projection of it, not a
   parallel truth.

2. **One reader.** A single column list + a single `bead_from_row` mapper backs
   *every* read. No query hand-writes its own `SELECT` column set, so no read
   can silently omit `acceptance_criteria` (or any field) while another
   includes it.

3. **One writer.** A single full-fidelity write path takes the canonical
   record. The `create_bead` / `create_bead_full` fork is retired; callers that
   want a "basic" create construct a canonical record with defaults.

4. **Derived surface mapping, not hand `.get()`.** MCP and CLI argument
   mapping is derive-based (serde over the JSON/args) against the canonical
   field set, so adding a field cannot silently skip a surface. This is
   [ADR-0006](0006-declarative-tool-registry.md)'s principle applied to the
   bead payload: one declaration, many surfaces.

5. **A mechanical drift gate.** A test asserts the field sets of
   {create args, write columns, read columns, export keys} each **equal** the
   canonical set. Add a field and forget a surface → the test fails in CI.
   This is `project_mechanical_gates_pattern` applied to the schema: the
   substrate catches drift without human vigilance.

6. **Migration falls out for free.** With one field set and one reader/writer,
   a store-to-store migration is `source.read_canonical() →
   target.write_canonical()` — full-fidelity **by construction**, because
   there is exactly one field set and both sides use it. `rosary-3a0e19`
   becomes a thin wrapper (read + write + atomic swap + verify), not a new
   field list.

### The bd-legacy column question (must be decided, not drifted)

rosary's SQLite `issues` schema has 16 columns and **no** `branch` / `pr_url` /
`jj_change_id` — those exist only in the richer bd/Dolt schema. The canonical
field set makes this explicit rather than accidental: either (a) those columns
are added to rosary's native schema and become canonical, or (b) they are
declared **out of scope** for rosary's format (derivable from git/GitHub) and
the mechanical gate treats their absence as intended, not as drift. This ADR
requires the choice to be *recorded in the canonical definition*, so a
Dolt→SQLite migration can't silently lose a field nobody decided to drop.

## Consequences

- **`rosary-4887d0` dies:** one derive-mapped create path can't forget a field
  on one surface.
- **The read-lossy trait dies:** one shared reader means `get_bead` and `list`
  agree by construction.
- **`rosary-3a0e19` becomes trivial and correct:** migration is a projection
  round-trip, full-fidelity for every field in the canonical set.
- **One place to add a field.** Adding a bead field is a single edit to the
  canonical definition; the gate names every surface that still needs wiring.
- **Not free:** this touches the core model, the store read/write paths, the
  MCP/CLI arg mapping, and export. It is deliberately sliced (below) so each
  step is independently reviewable and shippable.

## Alternatives considered

- **Just write the migration.** Rejected — it adds a seventh hand-rolled field
  list and leaves 4887d0 and the read-lossy reads live. Fixes an instance, not
  the class.
- **Extend `bead_ops` to cover read/export too, but keep hand mapping.**
  Rejected — a shared *function* still lets each caller pass a hand-built field
  set; only a single *declaration* + a drift gate stops the rot.
- **Full ADR-0020 identity model first.** Orthogonal. 0020 single-sources a
  bead's *identity/storage*; this ADR single-sources its *field set*. Either
  can land first; this one is smaller and unblocks the migration now.

## Migration plan

Sliced so each PR stands alone and the mechanical gate lands early:

1. **Unify reads** — one column list + `bead_from_row`, used by
   `get_bead`/`list_beads`/`list_beads_scoped`. Kills the read-lossy divergence.
2. **Unify writes** — one full-fidelity writer; retire `create_bead` vs
   `create_bead_full`.
3. **Derive-map the surfaces + land the drift gate** — MCP/CLI args mapped from
   the canonical set; CI test asserting all surfaces cover it. Kills 4887d0.
4. **Migration (`rosary-3a0e19`)** — the thin read/write/swap/verify wrapper,
   now full-fidelity by construction.

## Cross-references

- `bead_ops::BeadCreateArgs` — the existing partial single-source (create only).
- `src/bead_sqlite/mod.rs` — the divergent readers/writers this ADR unifies.
- `src/serve/handlers/mod.rs` — the hand-`.get()` mapping this ADR derives.
- `src/import.rs` — export/import, a projection to fold into the canonical set.
