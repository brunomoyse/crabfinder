# syntax=docker/dockerfile:1

# --- Build -------------------------------------------------------------------
FROM rust:1.99.0-slim-trixie AS builder
WORKDIR /src
COPY . .

# Only the runtime binaries; crabfinder-eval (and its TLS stack) is a dev tool.
# Cache mounts keep the registry and target dir across builds, so binaries are
# copied out of target/ inside the same RUN.
# The workspace release profile keeps debug info for local profiling; the
# image doesn't need it, so drop it and strip symbols here.
ENV CARGO_PROFILE_RELEASE_DEBUG=0 \
    CARGO_PROFILE_RELEASE_STRIP=symbols
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked \
        -p crabfinder-service -p crabfinder-indexer -p crabfinder-batch \
 && mkdir -p /out \
 && cp target/release/crabfinder-service \
       target/release/crabfinder-indexer \
       target/release/crabfinder-batch /out/

# --- Runtime -----------------------------------------------------------------
FROM debian:trixie-slim

# curl + unzip + CA certificates for refresh_best.sh (download BeSt) and the
# health check.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl unzip \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --no-create-home crabfinder \
 && mkdir -p /index /data /jobs \
 && chown crabfinder /index /data /jobs

COPY --from=builder /out/ /usr/local/bin/
COPY scripts/refresh_best.sh /usr/local/bin/
COPY data/notability-whitelist-v1.yaml data/street-type-abbreviations-v1.yaml /usr/share/crabfinder/

# The service binds 127.0.0.1 by default; listen on all interfaces so the
# published port is reachable. The index is built into (and served from) the
# /index volume, or /data/index-current with refresh_best.sh (see
# docker-compose.yml and docs/en/02-installation.md).
ENV CRABFINDER_HOST=0.0.0.0 \
    CRABFINDER_PORT=8080 \
    CRABFINDER_INDEX=/index \
    CRABFINDER_ABBREVIATION_DICT=/usr/share/crabfinder/street-type-abbreviations-v1.yaml \
    BIN_DIR=/usr/local/bin \
    DATA_DIR=/data \
    WHITELIST=/usr/share/crabfinder/notability-whitelist-v1.yaml \
    ABBREV_DICT=/usr/share/crabfinder/street-type-abbreviations-v1.yaml

USER crabfinder
VOLUME ["/index", "/data", "/jobs"]
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=3s --start-period=60s \
    CMD curl -fs "http://127.0.0.1:${CRABFINDER_PORT}/health" >/dev/null || exit 1
CMD ["crabfinder-service"]
