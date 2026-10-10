#!/bin/sh
# Builds everything the page (runtime/web) needs to run programs on Wine, as
# CI's wine-build job does: the translator and its WebAssembly build, the
# native heap and string modules, the d3dgpu module (runtime/d3dgpu/pkg),
# Wine's i386 PE DLLs, programs and the conformance tests CI builds, Wine's
# Unix side, and the bundle (target/wine-bundle). Each step is incremental.
# Run it in the build container:
#
#   tools/docker/run.sh tools/docker/build-all.sh
set -eu
cd "$(dirname "$0")/../.."
# The DLL list is CI's (the composite action that builds Wine).
targets=$(sed -n 's/.*WINE_TARGETS=\([^"]*\)".*/\1/p' .github/actions/wine/action.yml)
[ -n "$targets" ] || { echo "no WINE_TARGETS in .github/actions/wine/action.yml" >&2; exit 1; }
step() { echo "== $*" >&2; }
step translator; cargo build --release -p wwt-cli
step "translator, heap and strings (wasm)"; cargo build -p wwt-wasm -p wwt-heap -p wwt-strings --target wasm32-unknown-unknown --profile release-wasm
step d3dgpu; runtime/d3dgpu/build.sh
step "Wine's PE side"; sh tools/wine/build.sh $targets
step "Wine's Unix side"; sh native/wine-unix/build.sh
step "the Wine bundle"; node runtime/node/wine-bundle.mjs
echo "done: serve with node runtime/web/serve.mjs 8080 (http://localhost:8080/runtime/web/)" >&2
