# fixtures.capnp — leyline-net/v1 conformance vector value definitions.
#
# Bead ley-line-open-083344. These consts define the exact values whose
# encodings are committed under `reference/` and `canonical/`. They are
# a value-for-value mirror of cloister's `wire/cross-check-fixtures.capnp`
# (@0xc1c0c0c0c0c0c1c0) — the vector set cloister's hand-rolled TS codec
# and Go bindings already validate against — re-rooted on LLO's canonical
# `net.capnp`.
#
# Reference-encoder check (mirrors cloister's `capnp eval -b` fixture
# generation; run from `rs/ll-core/schema-spec/leyline-net/v1/test-vectors/`):
#
#   capnp eval -I ../../../../schema-capnp/schemas --no-standard-import \
#     fixtures.capnp <constName> -b
#
# must be byte-equal to `reference/<const-name>.bin`. The test
#   rs/ll-core/schema-capnp/tests/leyline_net_vectors.rs
# mechanizes this, plus the strict-canonical form and the digest pins.

@0x9ff483f71f533d11;

using Net = import "/net.capnp";

# ── Manifest fixtures ─────────────────────────────────────────────────────

const manifestCanonical :Net.Manifest = (
  sequence    = 42,
  publicKey   = 0x"1111111111111111111111111111111111111111111111111111111111111111",
  signature   = 0x"22222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222",
  contentHash = 0x"3333333333333333333333333333333333333333333333333333333333333333",
);

const manifestZeroSequence :Net.Manifest = (
  sequence    = 0,
  publicKey   = 0x"0000000000000000000000000000000000000000000000000000000000000000",
  signature   = 0x"00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
  contentHash = 0x"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
);

# ── ToolCall fixtures ─────────────────────────────────────────────────────

const toolCallBasic :Net.ToolCall = (
  upstreamId    = "rosary",
  toolName      = "rsry_status",
  argumentsJson = 0x"7b7d",  # "{}"
);

const toolCallEmpty :Net.ToolCall = (
  upstreamId    = "",
  toolName      = "",
  # argumentsJson omitted intentionally. Acceptance of literal-empty Data
  # forms (`0x""`, `[]`) varies between capnp compiler versions and isn't
  # mandated by the spec. The portable form is to omit the field;
  # defaulted Data is the empty list, which is what we want here.
);

const toolCallWithArgs :Net.ToolCall = (
  upstreamId    = "leyline",
  toolName      = "lsp_hover",
  # canonical JSON: {"col":5,"file":"/x/foo.rs","line":10}
  argumentsJson = 0x"7b22636f6c223a352c2266696c65223a222f782f666f6f2e7273222c226c696e65223a31307d",
);

# ── ToolResult fixtures ──────────────────────────────────────────────────

const toolResultEmpty :Net.ToolResult = (
  content = [],
  isError = false,
);

const toolResultErrorEmpty :Net.ToolResult = (
  content = [],
  isError = true,
);

const toolResultText :Net.ToolResult = (
  content = [
    (body = (text = "hello world")),
  ],
  isError = false,
);

const toolResultResource :Net.ToolResult = (
  content = [
    # raw bytes "opaque"
    (body = (resource = 0x"6f7061717565")),
  ],
  isError = false,
);

const toolResultBinary :Net.ToolResult = (
  content = [
    (body = (binary = (
      data     = 0x"89504e47",  # PNG signature first 4 bytes
      mimeType = "image/png",
    ))),
  ],
  isError = false,
);

const toolResultMixed :Net.ToolResult = (
  content = [
    (body = (text = "first")),
    (body = (binary = (data = 0x"010203", mimeType = "application/octet-stream"))),
    (body = (resource = 0x"6f706171756532")),
    (body = (text = "last")),
  ],
  isError = false,
);

const toolResultErrorWithText :Net.ToolResult = (
  content = [
    (body = (text = "tool failed: missing 'file' argument")),
  ],
  isError = true,
);
