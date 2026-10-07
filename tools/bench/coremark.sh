#!/bin/sh
# CoreMark (EEMBC) built natively with `gcc -m32` and as a Windows .exe with
# MinGW, both -O2; runs the native build, then the .exe translated on the M1
# shims and on translated Wine. CoreMark validates its own results and
# calibrates itself to run for at least 10 seconds.
#
#   tools/bench/coremark.sh
#
# For the browser: node tests/web/browser.mjs target/bench/coremark.exe \
#   "Correct operation validated" --wine
set -eu
COMMIT=1f483d5b8316753a742cbf5590caf5bd0a4e4777
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
cd "$root"
show() { grep -E "Correct operation|Errors detected|^CoreMark 1.0"; }
echo "== native (gcc -m32)"; "$out/coremark.native" | show
echo "== translated, M1 shims"; node runtime/node/run.mjs "$out/coremark.exe" 2>/dev/null | show
echo "== translated Wine"; node runtime/node/wine.mjs "$out/coremark.exe" 2>/dev/null | show
