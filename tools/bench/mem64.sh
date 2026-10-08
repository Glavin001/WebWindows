#!/bin/sh
# The cost of 64-bit WebAssembly memory (memory64), the question that decides
# whether 64-bit programs need a 32-bit-address mode: CoreMark built as a
# 32-bit and a 64-bit Windows .exe, each run translated on the M1 shims with
# a 32-bit and a 64-bit memory, next to the native builds.
#
#   tools/bench/mem64.sh [node]
#
# `node` defaults to the one on PATH; memory64 needs Node 24 or later (Node
# 22 has an early version behind --experimental-wasm-memory64, which this
# script passes there). CoreMark calibrates itself to run for at least 10
# seconds per configuration.
set -eu
COMMIT=1f483d5b8316753a742cbf5590caf5bd0a4e4777
NODE=${1:-node}
root=$(cd "$(dirname "$0")/../.." && pwd)
src="$root/target/coremark-src"
out="$root/target/bench"
if [ "$(git -C "$src" rev-parse HEAD 2>/dev/null)" != "$COMMIT" ]; then
  rm -rf "$src"
  git init -q "$src"
  git -C "$src" fetch -q --depth 1 https://github.com/eembc/coremark "$COMMIT"
  git -C "$src" checkout -q FETCH_HEAD
fi
mkdir -p "$out"
cd "$src"
files="core_list_join.c core_main.c core_matrix.c core_state.c core_util.c simple/core_portme.c"
flags="-O2 -I. -Isimple -DPERFORMANCE_RUN=1 -DITERATIONS=0"
# shellcheck disable=SC2086
gcc -m32 $flags -DFLAGS_STR='"-O2"' $files -o "$out/coremark.native"
# shellcheck disable=SC2086
i686-w64-mingw32-gcc $flags -DFLAGS_STR='"-O2"' $files -o "$out/coremark.exe"
# The simple port holds pointers in a 32-bit integer; 64-bit builds get a
# copy of it with a pointer-sized one.
rm -rf simple64 && cp -r simple simple64
sed -i 's/^typedef ee_u32         ee_ptr_int;/typedef unsigned long long ee_ptr_int;/' simple64/core_portme.h
files64=$(echo "$files" | sed 's|simple/|simple64/|')
flags64=$(echo "$flags" | sed 's|-Isimple|-Isimple64|')
# shellcheck disable=SC2086
gcc $flags64 -DFLAGS_STR='"-O2"' $files64 -o "$out/coremark64.native"
# shellcheck disable=SC2086
x86_64-w64-mingw32-gcc $flags64 -DFLAGS_STR='"-O2"' $files64 -o "$out/coremark64.exe"
cd "$root"
show() { grep -E "Correct operation|Errors detected|^CoreMark 1.0" | sed 's/^/    /'; }
version=$("$NODE" --version)
echo "node $version"
flag=
case "$version" in v22.*) flag=--experimental-wasm-memory64 ;; esac
echo "== native x86 (gcc -m32)"; "$out/coremark.native" | show
echo "== native x86-64 (gcc)"; "$out/coremark64.native" | show
run() {
  # shellcheck disable=SC2068
  "$NODE" $flag runtime/node/run.mjs $@ 2>/dev/null | show
}
echo "== x86, 32-bit memory"; run "$out/coremark.exe"
echo "== x86, 64-bit memory"; run --mem64 "$out/coremark.exe"
echo "== x86-64, 32-bit memory"; run "$out/coremark64.exe"
echo "== x86-64, 64-bit memory"; run --mem64 "$out/coremark64.exe"
