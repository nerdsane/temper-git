# Genesis Railway image.
#
# Builds the Temper binary from the pinned submodule, builds the Genesis web UI,
# and ships exactly one bootstrap OS app bundle: temper-git / Genesis.

FROM node:22-bookworm AS web-builder
WORKDIR /app/web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web ./
RUN npm run build

FROM rust:1-bookworm AS rust-builder
RUN apt-get update && apt-get install -y \
    pkg-config libssl-dev python3-dev clang libclang-dev libjemalloc-dev \
    && rm -rf /var/lib/apt/lists/*
RUN rustup toolchain install 1.92 && rustup default 1.92 && rustup target add wasm32-wasip1
WORKDIR /app
COPY . .

# Build Genesis WASM integrations and place them where app.toml declares them.
RUN cargo build --release --target wasm32-wasip1 \
    -p git_upload_pack \
    -p git_receive_pack \
    -p scm_ingest_pack \
    -p app_registry
RUN mkdir -p \
    wasm/git_upload_pack \
    wasm/git_receive_pack \
    wasm/scm_ingest_pack \
    wasm/app_registry \
    && cp target/wasm32-wasip1/release/git_upload_pack.wasm wasm/git_upload_pack/git_upload_pack.wasm \
    && cp target/wasm32-wasip1/release/git_receive_pack.wasm wasm/git_receive_pack/git_receive_pack.wasm \
    && cp target/wasm32-wasip1/release/scm_ingest_pack.wasm wasm/scm_ingest_pack/scm_ingest_pack.wasm \
    && cp target/wasm32-wasip1/release/app_registry.wasm wasm/app_registry/app_registry.wasm

RUN cargo build --manifest-path temper/Cargo.toml --release --bin temper

RUN mkdir -p /opt/genesis-os-apps/temper-git \
    && cp app.toml APP.md README.md /opt/genesis-os-apps/temper-git/ \
    && cp -R specs policies docs registry canonical wire wasm wasm-modules /opt/genesis-os-apps/temper-git/

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y \
    ca-certificates libssl3 python3 libz3-4 libjemalloc2 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=rust-builder /app/temper/target/release/temper /usr/local/bin/temper
COPY --from=rust-builder /opt/genesis-os-apps /opt/genesis-os-apps
COPY --from=web-builder /app/web/build /opt/genesis-web

ENV RUST_LOG=info,temper=info
ENV TEMPER_EVENT_STORE=postgres
ENV TEMPER_OS_APPS_DIR=/opt/genesis-os-apps
ENV TEMPER_GENESIS_WEB_DIR=/opt/genesis-web

EXPOSE 3000
CMD ["sh", "-c", "temper serve --port ${PORT:-3000} --storage postgres --no-observe --app temper-git"]
