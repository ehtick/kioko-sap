# -----------------------------------------------------------------------------
# Build stage
# -----------------------------------------------------------------------------
# Toolchain versions are pinned and injected by scripts/deploy_docker.sh.
# The defaults below are only used for a bare `docker build` with no --build-arg.
ARG RUST_VERSION=1.93.0

FROM rust:${RUST_VERSION}-bookworm AS build

ARG DEBIAN_FRONTEND=noninteractive
ARG NODE_MAJOR=24
ARG DENO_VERSION=2.1.4
ARG WASM_PACK_VERSION=0.13.1
WORKDIR /app

# ---- System deps ----
# - libssl-dev is only needed if you still depend on OpenSSL/native-tls
# - libpq-dev is only needed if something links to libpq (sqlx generally doesn't)
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    git \
    bash \
    pkg-config \
    build-essential \
    libssl-dev \
    libpq-dev \
    && rm -rf /var/lib/apt/lists/*

# ---- Node + pnpm (via corepack) ----
# Use Node ${NODE_MAJOR}.x because your package.json requires node ^24.9.0
RUN curl -fsSL https://deb.nodesource.com/setup_${NODE_MAJOR}.x | bash - \
    && apt-get update && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/* \
    && corepack enable

# ---- Deno ----
RUN curl -fsSL https://deno.land/install.sh | sh -s "v${DENO_VERSION}" \
    && mv /root/.deno/bin/deno /usr/local/bin/deno

# ---- wasm-pack ----
RUN rustup target add wasm32-unknown-unknown \
    && cargo install wasm-pack --version ${WASM_PACK_VERSION} --locked
