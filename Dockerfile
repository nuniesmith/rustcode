# syntax=docker/dockerfile:1.7

# =============================================================================
# rustcode — multi-stage build
#
# Stage 1: chef-planner  – compute a recipe of workspace dependencies
# Stage 2: chef-builder  – build dependencies (cached), then the workspace
# Stage 3: runtime       – minimal Debian slim image with the rustcode binary
#
# Build:
#   docker build -t rustcode:latest .
#
# Run (standalone, expects an external Postgres):
#   docker run --rm -p 3500:3500 --env-file .env rustcode:latest
#
# Or via docker-compose (brings up Postgres alongside):
#   docker compose up --build
# =============================================================================

# Pin to the toolchain the project is currently developed against.
#
# NOTE: Debian must be `trixie` (or newer). `fastembed` pulls a pre-built
# `ort-sys` (ONNX Runtime) binary from pyke.io that references the
# glibc ≥ 2.38 ISO-C23 symbols (`__isoc23_strtoll`, etc.). Bookworm ships
# glibc 2.36 and fails to link with `undefined symbol: __isoc23_strtoll`.
ARG RUST_VERSION=1.94
ARG DEBIAN_VERSION=trixie

# -----------------------------------------------------------------------------
# Stage 1 — cargo-chef planner: produce a `recipe.json` of the dependency graph
# -----------------------------------------------------------------------------
FROM rust:${RUST_VERSION}-${DEBIAN_VERSION} AS chef
RUN cargo install cargo-chef --locked --version ^0.1
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# -----------------------------------------------------------------------------
# Stage 2 — builder: install system deps, cook dependencies, build rustcode
# -----------------------------------------------------------------------------
FROM chef AS builder

# System libraries needed by git2 (libssh2 + openssl), sqlx, fastembed/ort.
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential \
        cmake \
        pkg-config \
        libssl-dev \
        libssh2-1-dev \
        zlib1g-dev \
        ca-certificates \
        curl \
    && rm -rf /var/lib/apt/lists/*

# SQLX_OFFLINE=true would require committed query cache; we use the regular
# online migrate! macro which only needs the SQL files (copied below), so this
# stays off.
ENV CARGO_INCREMENTAL=0 \
    CARGO_TERM_COLOR=always \
    RUST_BACKTRACE=1

COPY --from=planner /app/recipe.json recipe.json

# Cook dependencies — this layer is cached until Cargo.lock / Cargo.toml change.
RUN cargo chef cook --release --recipe-path recipe.json

# Copy the rest of the workspace and build the release binary.
COPY . .

RUN cargo build --release --bin rustcode

# -----------------------------------------------------------------------------
# Stage 3 — runtime: just the binary + minimal shared libs
# -----------------------------------------------------------------------------
FROM debian:${DEBIAN_VERSION}-slim AS runtime

# Trixie completed the time_t transition: `libssl3` → `libssl3t64`,
# `libssh2-1` → `libssh2-1t64`. `libgomp1` is required by the
# ONNX Runtime backend that `fastembed` uses.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        libssl3t64 \
        libssh2-1t64 \
        libgomp1 \
        zlib1g \
        libgcc-s1 \
        git \
        curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 1000 rustcode \
    && useradd  --system --uid 1000 --gid rustcode --create-home --home-dir /home/rustcode rustcode

WORKDIR /app

# Binary
COPY --from=builder /app/target/release/rustcode /usr/local/bin/rustcode

# SQL migrations are loaded from disk at runtime via sqlx::migrate!() — copy them
# so the embedded migrator can find ./sql relative to the working directory.
COPY --from=builder /app/sql /app/sql

# Writable directories for the per-repo cache, indexed repos, and task files.
RUN mkdir -p /app/repos /app/tasks /app/data /home/rustcode/.rustcode \
    && chown -R rustcode:rustcode /app /home/rustcode

USER rustcode

ENV HOST=0.0.0.0 \
    PORT=3500 \
    REPOS_DIR=/app/repos \
    RUST_LOG=info,rustcode=debug,sqlx=warn

EXPOSE 3500

# `/healthz` is the cheap health endpoint exposed by the server.
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD curl -fsS http://localhost:${PORT}/healthz || exit 1

ENTRYPOINT ["/usr/local/bin/rustcode"]
