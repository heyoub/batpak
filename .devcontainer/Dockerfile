FROM rust:1.92-bookworm

ARG DEBIAN_FRONTEND=noninteractive
# Host-matched UID/GID so files created inside the container appear owned
# by the host user on the mounted workspace, not root or nobody.
# Override via .devcontainer/devcontainer.json build args.
ARG USER_UID=1000
ARG USER_GID=1000
ENV PATH="/usr/local/cargo/bin:${PATH}"

RUN apt-get update && apt-get install -y --no-install-recommends \
    clang \
    just \
    mold \
    curl \
    dmsetup \
    e2fsprogs \
    git \
    pkg-config \
    ca-certificates \
    sudo \
    util-linux \
    && rm -rf /var/lib/apt/lists/*

# Non-root user matching host UID/GID. Passwordless sudo so the container
# can still elevate when a tool really needs to (e.g. installing a one-off
# apt package), without making every build run as root.
RUN groupadd --gid ${USER_GID} dev \
    && useradd --uid ${USER_UID} --gid ${USER_GID} -m -s /bin/bash dev \
    && echo 'dev ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/dev \
    && chmod 0440 /etc/sudoers.d/dev

# Prefer prebuilt binaries for the heavyweight cargo subcommands so local
# preflight cold-starts do not spend minutes compiling tooling from source.
# cargo-binstall itself is bootstrapped from its published release installer.
RUN curl -L --proto '=https' --tlsv1.2 -sSf \
    https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh \
    | bash
RUN cargo binstall --no-confirm cargo-deny@0.19.0 || cargo install --locked cargo-deny@0.19.0
RUN cargo binstall --no-confirm cargo-audit@0.22.1 || cargo install --locked cargo-audit@0.22.1
RUN cargo binstall --no-confirm cargo-nextest@0.9.132 || cargo install --locked cargo-nextest@0.9.132
RUN cargo binstall --no-confirm cargo-llvm-cov@0.8.5 || cargo install --locked cargo-llvm-cov@0.8.5
# cargo-public-api generates rustdoc JSON, which is only available on
# nightly. Install a minimal nightly toolchain alongside the pinned
# stable so `cargo xtask public-api --strict` can auto-fall-back to
# nightly for rustdoc-json generation without forcing the rest of
# the build off the canonical 1.92 stable channel.
RUN rustup toolchain install nightly --profile minimal --no-self-update
RUN cargo binstall --no-confirm cargo-public-api@0.51.0 || cargo install --locked cargo-public-api@0.51.0
# The published GNU binary currently requires a newer glibc than bookworm ships,
# so install cargo-mutants from source in the canonical container.
RUN cargo install --locked cargo-mutants@27.0.0

# System-level safe.directory so every user in the container (root and dev)
# sees the mounted workspace as trusted, regardless of UID mapping.
RUN git config --system --add safe.directory /workspace/batpak

# Give dev ownership of the cargo + rustup homes so cargo can write without
# needing sudo. Installed-tool binaries in /usr/local/cargo/bin stay readable.
RUN chown -R dev:dev /usr/local/cargo /usr/local/rustup

USER dev
WORKDIR /workspace/batpak/bpk-lib
