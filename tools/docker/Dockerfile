# The build environment: what CI's Ubuntu runners have (.github/workflows/ci.yml,
# .github/actions), as an x86-64 image. Everything CPU-side builds here: the
# translator, Wine's PE DLLs (MinGW), Wine's Unix side (Emscripten), the Wine
# bundle, the d3dgpu module, test programs, and the program tests, whose
# reference runs need an x86 CPU (gcc -m32). The browser and GPU side runs on
# the host. Use it through tools/docker/run.sh, which mounts the repository.
FROM ubuntu:24.04
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential gcc-multilib flex bison pkg-config cmake make git curl ca-certificates xz-utils zstd python3 \
      gcc-mingw-w64-i686 binutils-mingw-w64-i686 gcc-mingw-w64-x86-64 binutils-mingw-w64-x86-64 \
      libfreetype-dev csmith binaryen wabt mesa-vulkan-drivers \
    && rm -rf /var/lib/apt/lists/*
# Node 22, as CI (.nvmrc on the host).
ARG NODE_VERSION=22.22.2
RUN curl -fsSL https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-linux-x64.tar.xz | tar xJ -C /usr/local --strip-components=1
# Rust: rust-toolchain.toml picks the channel; stable with the wasm target
# here so a first build doesn't download it.
ENV PATH=/root/.cargo/bin:$PATH
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable \
      --target wasm32-unknown-unknown --component rustfmt,clippy
# Emscripten, CI's EMSDK_VERSION, for Wine's Unix side.
ARG EMSDK_VERSION=6.0.11
RUN git clone --depth 1 https://github.com/emscripten-core/emsdk /opt/emsdk \
    && cd /opt/emsdk && ./emsdk install ${EMSDK_VERSION} && ./emsdk activate ${EMSDK_VERSION}
ENV EMSDK=/opt/emsdk
# wasm-tools, a prebuilt release.
ARG WASM_TOOLS_VERSION=1.261.0
RUN curl -fsSL https://github.com/bytecodealliance/wasm-tools/releases/download/v${WASM_TOOLS_VERSION}/wasm-tools-${WASM_TOOLS_VERSION}-x86_64-linux.tar.gz \
      | tar xz -C /usr/local/bin --strip-components=1 wasm-tools-${WASM_TOOLS_VERSION}-x86_64-linux/wasm-tools
WORKDIR /work
