#!/bin/bash
# Builds each checkpoint in checkpoints.txt in its own worktree: the
# translator (native and WebAssembly) and the runtime's WebAssembly helpers,
# so run.mjs can measure every checkpoint with its own code.
#
#   tools/bench/history/build.sh        (HIST_DIR, default target/history)
set -e
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../.." && pwd)
H=${HIST_DIR:-$root/target/history}
mkdir -p "$H"
T=$H/target-shared
while read -r c label rest; do
  wt=$H/wt-$label
  [ -d "$wt" ] || git -C "$root" worktree add -q --detach "$wt" "$c"
  # Runtimes from before `--file` get Lua's script from one added line.
  w=$wt/runtime/node/wine.mjs
  grep -q "'--file'" "$w" || grep -q 'bench.lua' "$w" ||
    sed -i "/^files.set(exeDos, readFileSync(exe));/a files.set('c:\\\\\\\\bench.lua', readFileSync('$root/tools/bench/workloads/bench.lua')); // measurement only" "$w"
  # One cargo target directory for all: touch the sources so cargo does not
  # reuse another checkpoint's artifacts by their timestamps.
  find "$wt/crates" -name '*.rs' -exec touch {} +
  mkdir -p "$wt/target/release" "$wt/target/wasm32-unknown-unknown/release-wasm"
  (cd "$wt" && CARGO_TARGET_DIR=$T cargo build -q --release -p wwt-cli) || { echo "build failed: $label"; continue; }
  cp "$T/release/wwt" "$wt/target/release/wwt"
  for p in wwt-wasm wwt-heap wwt-strings; do
    [ -d "$wt/crates/$p" ] || continue
    (cd "$wt" && CARGO_TARGET_DIR=$T cargo build -q --target wasm32-unknown-unknown --profile release-wasm -p "$p")
    cp "$T/wasm32-unknown-unknown/release-wasm/${p//-/_}.wasm" "$wt/target/wasm32-unknown-unknown/release-wasm/"
  done
  echo "built $label"
done < "$here/checkpoints.txt"
