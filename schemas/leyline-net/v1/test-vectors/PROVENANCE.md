# leyline-net/v1 test vectors — vendored from ley-line-open

These are the pinned conformance vectors for the leyline-net wire frames
(`Manifest` / `ToolCall` / `ToolResult`), vendored **verbatim** from
ley-line-open so rosary's copy of the schema
(`schemas/cloister.capnp`, itself vendored from LLO's `net.capnp`) can be
verified against LLO's byte-pins instead of eyeballed.

- **Source**: `ley-line-open/rs/ll-core/schema-spec/leyline-net/v1/test-vectors/`
- **LLO commit**: 78638f3 (PR #225, bead `ley-line-open-083344`)
- **Bead**: `rosary-086973`

## Contents (all pinned by `VECTORS.sha256`)

- `reference/<name>.bin` — 12 vectors, reference-encoder byte form
  (`capnp eval -b` / plain `write_message`; declared section sizes, no
  trailing-zero truncation).
- `canonical/<name>.bin` — 12 vectors, strict canonical form
  (`set_root_canonical`, trailing zero words truncated).
- `digests.json` — BLAKE3 + SHA-256 + size for every vector, both forms.
- `VECTORS.sha256` — `sha256sum` manifest over every load-bearing file.
- `fixtures.capnp` — the capnp `const` value definitions the vectors
  encode (rooted on LLO's `net.capnp`; carried for provenance and to
  keep the SHA-256 manifest complete).

LLO's errata-editable `README.md` is intentionally **not** vendored (it
is unpinned prose upstream); this `PROVENANCE.md` replaces it for rosary.

## What consumes them

`src/leyline_net_vectors.rs` (compiled under `#[cfg(test)]`) rebuilds
each of the 12 values with rosary's generated `cloister_capnp` code
(capnp 0.21) and asserts byte-equality against both forms here, then
decodes both forms back and asserts field values. A drift in rosary's
`schemas/cloister.capnp` — any field, ordinal, type, or layout change —
changes the produced bytes and fails that test loudly. Re-vendoring is
the deliberate act: copy the schema and this directory afresh from LLO.

Verify the vendored copy is intact at any time:

```
cd schemas/leyline-net/v1/test-vectors && shasum -a 256 -c VECTORS.sha256
```
