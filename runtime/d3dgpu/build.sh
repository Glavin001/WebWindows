#!/bin/sh
# Builds the wasm module and its JS bindings into runtime/d3dgpu/pkg.
# Needs wasm-bindgen-cli matching the wasm-bindgen version in Cargo.lock.
set -e
cd "$(dirname "$0")/../.."
cargo build -p d3dgpu-web --target wasm32-unknown-unknown --profile release-wasm
wasm-bindgen --target web --out-dir runtime/d3dgpu/pkg target/wasm32-unknown-unknown/release-wasm/d3dgpu_web.wasm
