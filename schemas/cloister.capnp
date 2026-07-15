# schemas/cloister.capnp — leyline-net wire frames, vendored from LLO.
#
# Source of truth: ley-line-open `rs/ll-core/schema-capnp/schemas/net.capnp`
# (fileId @0xa25bb2a310446125), vendored from LLO commit 78638f3
# (PR #225, bead ley-line-open-083344). This is a verbatim copy of that
# canonical file, with exactly two rosary-specific deltas — see below.
# Re-vendor FROM net.capnp when it evolves; do NOT hand-edit the frame
# structs here (the leyline-net/v1 conformance vectors under
# `schemas/leyline-net/v1/test-vectors/` are the drift gate — bead
# rosary-086973, test `src/leyline_net_vectors.rs`).
#
# ── rosary-specific deltas from net.capnp (the ONLY differences) ─────────
#
#   1. FILENAME kept as `cloister.capnp` (not `net.capnp`). rosary's
#      `build.rs` compiles this file to the generated module
#      `cloister_capnp` (capnpc-rust derives the module name from the
#      source filename), which `src/main.rs` and `src/serve/ipc.rs`
#      reference. Renaming the file would rename that module across the
#      IPC transport for no wire benefit, so the filename stays.
#   2. Go codegen annotations REMOVED. net.capnp carries
#      `using Go = import "/go.capnp"; $Go.package(...); $Go.import(...)`
#      for LLO's capnpc-go clients. rosary generates only Rust and has no
#      `/go.capnp` on its capnpc import path (build.rs compiles this one
#      file, src_prefix `schemas`), so the `import "/go.capnp"` would fail
#      to resolve at `capnp compile` time. rosary does no Go codegen, so
#      dropping the annotations is inert for its build.
#
# Neither delta touches a frame struct, field, ordinal, or type — the
# wire bytes are byte-identical to net.capnp's, mechanized by the
# byte-equality gate in `src/leyline_net_vectors.rs` (rosary's capnp
# 0.21 codegen reproduces LLO's =0.25.0 pinned vectors, both byte-forms).
#
# ── Everything below this line is net.capnp verbatim (sans Go block) ─────
#
# These structs are the leyline-net frame vocabulary: the per-message
# authentication header (`Manifest`) and the tool-call request/response
# payloads (`ToolCall` / `ToolResult`). They were first spec'd in
# cloister's `wire/cloister.capnp` (@0xa1c0157e2a1e0001, cloister
# ADR-0005) and vendored by rosary as `schemas/cloister.capnp`. Per the
# naming rule that leyline-* protocols live in LLO, net.capnp is now the
# canonical copy; downstream repos import or re-vendor FROM it:
#
#   - cloister `wire/cloister.capnp` — repoints to these definitions.
#   - rosary `schemas/cloister.capnp` — re-vendors from net.capnp
#     instead of from cloister (this file, bead rosary-086973).
#
# The fileId is NEW relative to cloister's (this file and cloister's
# cannot share an ID — capnp forbids two files with one ID in an import
# graph). A fileId is file identity, not wire content: struct encodings
# carry no type IDs, so the bytes of every frame are IDENTICAL to those
# produced from the cloister schema. That claim is mechanized, not
# asserted — see `schemas/leyline-net/v1/test-vectors/` for the
# digest-pinned vectors and `src/leyline_net_vectors.rs` for the
# byte-equality gates.
#
# ── Frame layout (full leyline-net envelope) ─────────────────────────────
#
# Where the transport crosses a real network boundary, the message body
# is a leyline-net frame:
#
#     [manifest length :2 bytes BE]
#     [manifest bytes  :variable]    -- a serialized `Manifest` struct
#     [aead nonce      :12 bytes]    -- ChaCha20-Poly1305 nonce
#     [aead ciphertext :variable]    -- AEAD(payload) where payload is a
#                                       serialized `ToolCall` (or the
#                                       response carries a `ToolResult`)
#
# AEAD authenticated-data binds the manifest bytes so a man-in-the-middle
# can't swap a manifest onto a stale ciphertext. The manifest's
# `contentHash` is SHA-256 of the AEAD plaintext (i.e. the un-encrypted
# capnp-encoded ToolCall/ToolResult); receivers verify it after decryption
# as a defense-in-depth check.
#
# Intra-cluster transports (cloister↔companion loopback HTTP, rosary's
# `rsry mcp --ipc-socket` UDS) exchange plain serialized `ToolCall` /
# `ToolResult` messages with NO Manifest envelope and NO AEAD — the
# trust boundary there is the host/filesystem, not the network. See
# cloister ADR-0005 (intra-cluster amendment) and rosary's
# `src/serve/ipc.rs` header.
#
# ── Canonical encoding contract ──────────────────────────────────────────
#
# Consumers require **canonical** Cap'n Proto encoding per
# capnproto.org/encoding.html#canonicalization:
#
#   - **Single segment** per message. Multi-segment input is rejected by
#     cloister's hand-rolled TS decoder.
#   - **Unpacked** binary encoding (canonical form forbids packed
#     zero-elision).
#   - **Composite-list size code 7** for List(struct).
#   - **Stream framing** is the transport's responsibility, not capnp's.
#
# Two byte-forms of these values circulate in practice and BOTH are
# pinned in `schemas/leyline-net/v1/test-vectors/`:
#
#   - `reference/` — `capnp eval -b` reference-encoder output (declared
#     section sizes, no trailing-zero truncation). This is what
#     cloister's committed fixtures pin and what freshly-built encoder
#     output (Rust Builder, TS codec) produces.
#   - `canonical/` — strict canonical form (`set_root_canonical`), which
#     additionally truncates trailing zero words. Decoders MUST accept
#     both; they are value-equal.
#
# ── Schema-evolution discipline ──────────────────────────────────────────
#
# Cap'n Proto wire-compat rules, quoted from capnproto.org/language.html
# § "Evolving Your Protocol":
#
#   - "New fields, enumerants, and methods may be added… as long as each
#     new member's number is larger than all previous members." — adding
#     fields and union variants at higher ordinals is safe.
#   - "You cannot change a field, method, or enumerant's number." —
#     renumbering @N tags is NEVER safe; reassigning a retired ordinal is
#     equivalent to renumbering. Retire a field by leaving its ordinal in
#     place and stopping population.
#   - "Any symbolic name can be changed, as long as the type ID / ordinal
#     numbers stay the same." — renaming a field is safe; names live in
#     codegen, never on the wire.
#
# When in doubt: add new fields, never remove or renumber. This file is
# load-bearing for cross-host wire compatibility across LLO, cloister,
# cloister-companion, and rosary; old consumers must keep parsing new
# frames and vice-versa. Any edit here MUST be a re-vendor from net.capnp
# paired with a deliberate regen of the leyline-net/v1 vectors (which the
# digest-pinned tests force — they fail loudly on drift).

@0xa25bb2a310446125;

# ── Manifest: the unforgeable per-message header ─────────────────────────

# Every enveloped wire frame carries one of these. The signature binds the
# message's content + sequence number to a public key, so a receiver can
# authenticate every frame independently — no per-session secret, no replay
# window exposure beyond what the sequence counter enforces.
struct Manifest {
  # Monotonic per-(publicKey) counter. Receivers maintain a per-pubkey
  # last-seen value and reject any frame whose sequence is ≤ last-seen.
  # The window for legitimate retransmits is the sender's responsibility
  # (don't reuse a sequence on retry — issue a new one).
  sequence    @0 :UInt64;

  # Ed25519 public key, 32 bytes. Pinned by configuration on the receiver:
  # the receiver knows which pubkey its peer was provisioned with, and
  # rejects any other.
  publicKey   @1 :Data;

  # Ed25519 signature, 64 bytes, over the canonical concatenation:
  #     sequence (LE 8 bytes) ‖ contentHash (32 bytes)
  # NOT over the AEAD ciphertext — the contentHash binding is what guarantees
  # the signed plaintext matches what's in the AEAD payload.
  signature   @2 :Data;

  # SHA-256 of the AEAD plaintext (the serialized ToolCall or ToolResult,
  # before encryption). 32 bytes.
  contentHash @3 :Data;
}

# ── ToolCall: the request payload ────────────────────────────────────────

# A gateway sends one of these when a client calls MCP `tools/call`. The
# receiver routes to the configured upstream by `upstreamId`, decodes the
# result, and returns a ToolResult.
struct ToolCall {
  # Logical upstream identifier — names which backend the receiver forwards
  # to (e.g. "rosary", "mache", "leyline"). Maps to receiver-side config,
  # not user-controlled.
  upstreamId @0 :Text;

  # MCP tool name (e.g. "rsry_decompose", "lsp_hover"). The receiver may
  # validate that this tool is actually advertised by the upstream; the
  # sending gateway has typically already done that check.
  toolName   @1 :Text;

  # Tool arguments encoded as canonical JSON bytes (the sender
  # canonicalizes incoming args. Encoding as Data here preserves the
  # exact bytes the sender-side digest was computed over without
  # re-canonicalizing on the receiver).
  #
  # Future evolution: an `args :ArgsUnion` field with one variant per known
  # tool would give end-to-end type safety, but requires a tool-schema
  # registry shared between sender and receiver. JSON bytes is the
  # simplest correct first cut.
  argumentsJson @2 :Data;
}

# ── ToolResult: the response payload ─────────────────────────────────────

# What the upstream side sends back. Mirrors the MCP `tools/call` result
# shape — content array + isError flag — so a gateway can re-emit it as
# JSON-RPC at the public face with no semantic translation.
struct ToolResult {
  content @0 :List(Content);
  isError @1 :Bool;
}

# Per-MCP-spec, content items have a discriminated `type`. We encode that
# as a capnp union so each variant carries exactly the right shape.
struct Content {
  body :union {
    text     @0 :Text;            # type:"text"     — JSON-stringified or prose
    binary   @1 :BinaryContent;   # type:"image"    — bytes + MIME
    resource @2 :Data;            # type:"resource" — opaque to the gateway;
                                  # the client decodes it. Forwarded verbatim.
  }
}

struct BinaryContent {
  data     @0 :Data;
  mimeType @1 :Text;
}
