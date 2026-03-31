FROM rust:1.92-bookworm

ARG DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
    clang \
    curl \
    git \
    jq \
    liblmdb-dev \
    pkg-config \
    python3 \
    python3-pip \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN cargo install --locked cargo-deny@0.19.0 \
    && cargo install --locked cargo-nextest@0.9.132 \
    && cargo install --locked cargo-llvm-cov@0.8.5 \
    && cargo install --locked cargo-mutants@27.0.0

WORKDIR /workspace/batpak
