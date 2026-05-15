# e2e.Dockerfile — fresh-setup regression test.
#
# Starts from vanilla Ubuntu, installs the prereqs documented in
# docs/GETTING_STARTED.md, builds rsry, and runs scripts/e2e-fresh-setup.sh
# which replays the documented onboarding path and asserts that the
# silent-init / migration-noise regressions stay fixed.
#
# Build + run from the repo root:
#   docker build -f e2e.Dockerfile -t rosary-e2e .
#   docker run --rm rosary-e2e
#
# Or via the Taskfile (also runs on macOS / OrbStack):
#   task e2e:fresh

FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin

# Minimum prereqs that match GETTING_STARTED.md Prerequisites.
# capnproto: build.rs codegen for schemas/cloister.capnp
# pkg-config + libssl-dev: openssl-sys (vendored is on but pkg-config is
#   probed at build time anyway)
# git: required by build.rs and by the test (`git init`)
# curl/ca-certificates: for downloading rustup + dolt
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        build-essential ca-certificates curl git pkg-config \
        libssl-dev capnproto && \
    rm -rf /var/lib/apt/lists/*

# Rust toolchain (matches `brew install rustup; rustup-init` on macOS)
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable

# Dolt — required at runtime by `rsry enable` / bead store.
# Mirrors `brew install dolt` on macOS.
RUN curl -fsSL https://github.com/dolthub/dolt/releases/latest/download/dolt-linux-amd64.tar.gz \
        -o /tmp/dolt.tgz && \
    tar -xzf /tmp/dolt.tgz -C /tmp && \
    cp /tmp/dolt-linux-amd64/bin/dolt /usr/local/bin/dolt && \
    rm -rf /tmp/dolt.tgz /tmp/dolt-linux-amd64

WORKDIR /src
COPY . /src

# Build the debug binary the e2e script will exercise. Debug is faster than
# release and exercises the same code paths for an init-flow smoke test.
RUN cargo build --bin rsry && \
    install -m 0755 target/debug/rsry /usr/local/bin/rsry

# Don't bake any dolt identity into the image — the e2e script asserts the
# preflight rejects this state before configuring it.

ENTRYPOINT ["/bin/bash", "/src/scripts/e2e-fresh-setup.sh"]
