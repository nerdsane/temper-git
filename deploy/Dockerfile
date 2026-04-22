# temper-git container — vanilla Temper kernel + temper-git OS-app
# bundle baked in. Built from the submodule at temper/ (must be on a
# branch that carries ADR-0056 Phase 1 so the HttpEndpoint entity is
# registered at boot).
#
# Runtime flow:
#   - temper serve reads TEMPER_OS_APPS_DIR (set below) to discover
#     apps, picks up temper-git, runs L0-L3 verification on its 13
#     specs, registers the entity sets under Temper.Git.
#   - OData is served at /tdata; $metadata returns the merged CSDL
#     (Temper.System + Temper.Git).
#   - Git smart-HTTP + GitHub REST surfaces will 501 until upstream
#     kernel deltas K-1 Phase 2 + K-2 land.
#
# Kept deliberately close to temper/Dockerfile's shape so upstream
# rebuild fixes propagate here mechanically.

# ── Stage 1: Chef ────────────────────────────────────────────────────────
FROM rust:1-bookworm AS chef
RUN cargo install cargo-chef --locked
RUN apt-get update && apt-get install -y \
    pkg-config libssl-dev python3-dev clang libclang-dev libjemalloc-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app

# ── Stage 2: Planner ────────────────────────────────────────────────────
#
# Only the temper submodule goes into the build context here —
# temper-git's canonical crate isn't part of the runtime image.
FROM chef AS planner
COPY temper/ ./
RUN cargo chef prepare --recipe-path recipe.json

# ── Stage 3: Builder ────────────────────────────────────────────────────
FROM chef AS builder
RUN rustup toolchain install 1.92 && rustup default 1.92

COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

COPY temper/ ./
RUN cargo build --release --bin temper

# ── Stage 4: Runtime ────────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y \
    ca-certificates libssl3 python3 libz3-4 libjemalloc2 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/temper /usr/local/bin/temper

# Bake the temper-git app bundle into the image at a known path.
# On boot, temper scans TEMPER_OS_APPS_DIR and auto-installs any app
# whose app.toml sets startup_install = "core". See
# crates/temper-platform/src/os_apps/mod.rs.
RUN mkdir -p /app/os-apps/temper-git
COPY app.toml APP.md /app/os-apps/temper-git/
COPY specs/    /app/os-apps/temper-git/specs/
COPY policies/ /app/os-apps/temper-git/policies/

ENV RUST_LOG=info,temper=info \
    TEMPER_OS_APPS_DIR=/app/os-apps

EXPOSE 3000
# `--skill temper-git` installs the baked app into the `default`
# tenant on first boot. Turso storage persists the install in
# tenant_installed_apps, so subsequent boots restore it from the DB
# (bootstrap Phase 8b) and this flag is a no-op.
# Postgres storage is a future path once ADR-0006's Cloud-SQL-backed
# EventStore and ADR-0004's bottomless-to-GCS migration both land.
CMD ["temper", "serve", "--port", "3000", "--storage", "turso", "--skill", "temper-git"]
