#!/bin/sh
# Assembles the static site (the page, its runtime, the translator compiled
# to WebAssembly and the Wine bundle) from a local build:
#
#   tools/site/build.sh [out dir]          (default: target/site)
#
# Needs the translator (cargo build -p wwt-wasm --target wasm32-unknown-unknown
# --profile release-wasm) and the bundle (node runtime/node/wine-bundle.mjs).
# CI deploys the result to Vercel (tools/site/deploy.sh).
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
out=${1:-$root/target/site}
translator=$root/target/wasm32-unknown-unknown/release-wasm/wwt_wasm.wasm
bundle=$root/target/wine-bundle
[ -f "$translator" ] || { echo "missing $translator" >&2; exit 1; }
[ -f "$bundle/manifest.json" ] || { echo "missing $bundle; run node runtime/node/wine-bundle.mjs" >&2; exit 1; }

rm -rf "$out"
mkdir -p "$out/runtime" "$out/target/wasm32-unknown-unknown/release-wasm" "$out/tests/programs"
# The page's code keeps its layout: it finds the translator and the bundle
# at ../../target/ from runtime/web/.
for d in web wine; do cp -r "$root/runtime/$d" "$out/runtime/$d"; done
cp "$root"/runtime/*.mjs "$out/runtime/"
cp "$translator" "$out/target/wasm32-unknown-unknown/release-wasm/"
cp -r "$bundle" "$out/target/wine-bundle"
# The x86_64 bundles (64-bit programs on Wine; -m32 for browsers without
# 64-bit WebAssembly memory), when built.
for b in wine-bundle64 wine-bundle64-m32; do
  if [ -f "$root/target/$b/manifest.json" ]; then cp -r "$root/target/$b" "$out/target/$b"; fi
done
cp "$root/tests/programs/hello.exe" "$root/tests/programs/hello64.exe" "$out/tests/programs/"
cat > "$out/index.html" <<'HTML'
<!doctype html>
<meta charset="utf-8">
<title>WebWindows</title>
<meta http-equiv="refresh" content="0; url=runtime/web/">
<p><a href="runtime/web/">WebWindows</a></p>
HTML
git -C "$root" rev-parse HEAD > "$out/version.txt" 2>/dev/null || true
du -sh "$out"
