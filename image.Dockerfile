# image.Dockerfile — assemble rosary OCI image from a krust-built static musl binary.
#
# Build is a two-step flow:
#   1. krust + cargo-zigbuild produces a static musl binary at
#      target/krust/aarch64-unknown-linux-musl/release/rsry
#      (no docker daemon involved; native cross-compile via zig).
#   2. This Dockerfile drops that binary onto chainguard/static:latest
#      (distroless, nonroot uid 65532) — a single COPY, no Rust toolchain in
#      the container, no virtiofs bottleneck.
#
# Pattern adopted from ley-line-open after its melange/apko path stalled on
# Apple Silicon (ley-line-open-2b255c). cluster.capnp pins `rosary:0.2.0`
# and invokes the binary as `mcp --ipc-socket /run/cloister-uds/rosary.sock`
# — both encoded as ENTRYPOINT + default CMD below.
#
# See `task image` for the wired-up invocation.

ARG BIN_PATH=target/krust/aarch64-unknown-linux-musl/release/rsry

FROM cgr.dev/chainguard/static:latest

ARG BIN_PATH
COPY ${BIN_PATH} /usr/bin/rsry

ENV HOME=/tmp \
    RUST_LOG=info

ENTRYPOINT ["/usr/bin/rsry"]
CMD ["mcp", "--ipc-socket", "/run/cloister-uds/rosary.sock"]
