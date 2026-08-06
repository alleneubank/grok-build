# Linux x86_64 builder for fork releases. Invoked from an Apple Silicon Mac via
# OrbStack/Docker (`--platform linux/amd64`), same pattern as the codex fork.
#
# Build:
#   docker build --platform linux/amd64 \
#     -t grok-fork-release-linux:rust-1.94.0 \
#     -f .github/docker/fork-release-linux.Dockerfile \
#     .github/docker
# Prefer a tag that is commonly already local (avoids interactive keychain
# pulls when the Docker credential helper is locked). Pin by digest in CI if needed.
FROM debian:bookworm-slim

ARG DEBIAN_FRONTEND=noninteractive
ARG RUST_VERSION=1.94.0

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        binutils \
        build-essential \
        ca-certificates \
        clang \
        cmake \
        curl \
        file \
        g++ \
        g++-x86-64-linux-gnu \
        gcc-x86-64-linux-gnu \
        git \
        libssl-dev \
        lld \
        pkg-config \
        protobuf-compiler \
        python3 \
        xz-utils \
    && rm -rf /var/lib/apt/lists/*

ENV CARGO_HOME=/cargo
ENV RUSTUP_HOME=/rustup
ENV PATH=/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
ENV PROTOC=/usr/bin/protoc
# Fleet ships linux.x86_64 (mise linux-x64). Always cross-compile to amd64 so
# the image works whether the Docker engine is native amd64 or arm64+qemu.
ENV CARGO_BUILD_TARGET=x86_64-unknown-linux-gnu
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc
ENV CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc
ENV CXX_x86_64_unknown_linux_gnu=x86_64-linux-gnu-g++

RUN curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs \
        | sh -s -- -y --profile minimal --default-toolchain "${RUST_VERSION}" \
    && rustup target add --toolchain "${RUST_VERSION}" x86_64-unknown-linux-gnu \
    && rustc --version \
    && protoc --version

WORKDIR /workspace