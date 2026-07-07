# image.Dockerfile — assemble rosary OCI image from a krust-built static musl binary.
#
# Build is a two-step flow:
#   1. krust + cargo-zigbuild produces a static musl binary at
#      target/krust/aarch64-unknown-linux-musl/release/rsry
#      (no docker daemon involved; native cross-compile via zig).
#   2. This Dockerfile drops that binary onto a distroless base — a single
#      COPY, no Rust toolchain in the container, no virtiofs bottleneck.
#
# Base image: gcr.io/distroless/static-debian12:nonroot
#   - `static`     — no glibc, expects a fully statically-linked binary
#                    (our musl build qualifies)
#   - `debian12`   — pins the base distro so security-fix updates stay on
#                    one major version; no surprise switches to debian13
#   - `:nonroot`   — runs as uid 65532; image is rebuilt with current
#                    package security fixes by Google; not pinned to a
#                    digest because the whole point of a maintained
#                    distroless base is receiving CVE patches
#
# Pattern adopted from ley-line-open after its melange/apko path stalled on
# Apple Silicon (ley-line-open-2b255c). cluster.capnp pins `rosary:0.2.0`
# and invokes the binary as `mcp --ipc-socket /run/cloister-uds/rosary.sock`
# — both encoded as ENTRYPOINT + default CMD below.
#
# See `task image` for the wired-up invocation.

# The Taskfile (`task image:build` / `image:release`) cross-compiles rsry with
# cargo-zigbuild and stages the per-arch binary at `dist/<arch>/rsry`. buildx
# sets TARGETARCH per `--platform`, so one Dockerfile serves single-arch local
# builds and the multi-arch release build with no shell in the (distroless) image.

FROM gcr.io/distroless/static-debian12:nonroot

ARG TARGETARCH
COPY dist/${TARGETARCH}/rsry /usr/bin/rsry

ENV HOME=/tmp \
    RUST_LOG=info

ENTRYPOINT ["/usr/bin/rsry"]
CMD ["mcp", "--ipc-socket", "/run/cloister-uds/rosary.sock"]
