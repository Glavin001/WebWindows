#!/bin/sh
# Builds the wasm module and its JS bindings into runtime/d3dgpu/pkg.
# Uses wasm-bindgen-cli matching the wasm-bindgen version in Cargo.lock,
# downloading a prebuilt one into target/wasm-bindgen when the one on PATH
# doesn't match (x86-64 Linux only).
set -e
cd "$(dirname "$0")/../.."
cargo build -p d3dgpu-web --target wasm32-unknown-unknown --profile release-d3dgpu
WB=$(awk '/^name = "wasm-bindgen"$/ { getline; gsub(/[^0-9.]/, ""); print; exit }' Cargo.lock)
bindgen=wasm-bindgen
if ! wasm-bindgen --version 2>/dev/null | grep -q "$WB"; then
  bindgen=target/wasm-bindgen/$WB/wasm-bindgen
  if [ ! -x "$bindgen" ]; then
    mkdir -p "target/wasm-bindgen/$WB"
    curl -sSfL "https://github.com/wasm-bindgen/wasm-bindgen/releases/download/$WB/wasm-bindgen-$WB-x86_64-unknown-linux-musl.tar.gz" |
      tar xz -C "target/wasm-bindgen/$WB" --strip-components=1 "wasm-bindgen-$WB-x86_64-unknown-linux-musl/wasm-bindgen"
  fi
fi
"$bindgen" --target web --out-dir runtime/d3dgpu/pkg target/wasm32-unknown-unknown/release-d3dgpu/d3dgpu_web.wasm
