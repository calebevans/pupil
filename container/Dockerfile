# syntax=docker/dockerfile:1

# ============================================================================
# Stage 0: Build recalld from source
# ============================================================================
# TARGETARCH is set by buildx (amd64 or arm64). We map it to a Rust
# target triple and build only for the requested platform.
FROM rust:1-alpine AS recalld-builder

ARG TARGETARCH
RUN apk add --no-cache musl-dev git && \
    if [ "$TARGETARCH" = "arm64" ]; then \
      RUST_TARGET=aarch64-unknown-linux-musl; \
    else \
      RUST_TARGET=x86_64-unknown-linux-musl; \
    fi && \
    rustup target add $RUST_TARGET && \
    echo $RUST_TARGET > /tmp/rust-target

# Clone recalld at a pinned version.
# IMPORTANT: For production builds, pin to a specific release tag or commit SHA.
ARG RECALLD_REF=v0.1.1
RUN git clone --depth 1 --branch ${RECALLD_REF} \
    https://github.com/calebevans/recalld.git /recalld

WORKDIR /recalld
RUN RUST_TARGET=$(cat /tmp/rust-target) && \
    cargo build --release --target $RUST_TARGET && \
    cp target/$RUST_TARGET/release/recalld /usr/local/bin/recalld

# ============================================================================
# Stage 1: Build pupil-agent
# ============================================================================
FROM rust:1-alpine AS builder

ARG TARGETARCH
RUN apk add --no-cache musl-dev pkgconf perl make && \
    if [ "$TARGETARCH" = "arm64" ]; then \
      RUST_TARGET=aarch64-unknown-linux-musl; \
    else \
      RUST_TARGET=x86_64-unknown-linux-musl; \
    fi && \
    rustup target add $RUST_TARGET && \
    echo $RUST_TARGET > /tmp/rust-target

WORKDIR /build
COPY . .

RUN RUST_TARGET=$(cat /tmp/rust-target) && \
    cargo build --release --target $RUST_TARGET -p pupil-agent && \
    cp target/$RUST_TARGET/release/pupil-agent /usr/local/bin/pupil-agent && \
    mkdir -p /empty-data/recalld /empty-data/sessions /empty-data/usage

# ============================================================================
# Stage 2: Runtime -- minimal distroless image
# ============================================================================
# distroless/static-debian12:nonroot provides:
# - CA certificates (for HTTPS to LLM APIs)
# - Non-root user UID 65532 (no USER directive needed)
# - Timezone data
# - ~2MB total size
# It does NOT include libc, so all binaries must be statically linked.
FROM gcr.io/distroless/static-debian12:nonroot

COPY --from=builder /usr/local/bin/pupil-agent /usr/local/bin/pupil-agent
COPY --from=recalld-builder /usr/local/bin/recalld /usr/local/bin/recalld
COPY --from=builder --chown=65532:65532 /empty-data/ /data/

# /data is the writable directory at runtime. It holds:
# - /data/recalld/     -- recalld SQLite database + WAL files
# - /data/sessions/    -- conversation session state (JSON files)
# - /data/usage/       -- daily JSONL usage logs
# - /data/audit/       -- daily JSONL audit logs (opt-in)
# - /data/.pupil-manifest.json -- build manifest
#
# No VOLUME declaration: docker commit must capture /data
# so that learned knowledge is baked into the image.
# Owned by UID 65532 (nonroot) so the agent can write at runtime.

# The agent binary serves as the entrypoint. Mode is determined by flags:
# - (no flags): interactive stdin/stdout chat
# - learn: curriculum learning mode
# - test: test execution mode
# - --port N: HTTP server mode
ENTRYPOINT ["pupil-agent"]
