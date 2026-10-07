#!/bin/sh
# Computes structure layouts from Wine's headers (needs the Wine source tree
# in WINE_SRC and a configured build in WINE_BUILD) and runs the result.
set -e
here=$(cd "$(dirname "$0")" && pwd)
cd "$here"
WINE_SRC=${WINE_SRC:-/opt/wine-src/wine-11.0}
WINE_BUILD=${WINE_BUILD:-/opt/wine-build}
# gen.sh [audio]: the core structures, or mmdevapi's audio driver calls.
src=${1:-layout}
i686-w64-mingw32-gcc -w -nostdlib -Wl,-e,_$([ "$src" = layout ] && echo start || echo entry) -o /tmp/wine-layout.exe "$src.c" -lkernel32 -I"$WINE_BUILD/include" -I"$WINE_SRC/include" -I"$WINE_SRC/include/msvcrt" -I"$WINE_SRC/dlls/mmdevapi" -D__WINESRC__ -nostdinc -isystem "$(i686-w64-mingw32-gcc -print-file-name=include)" 2>&1 >&2
node "$here/../../runtime/node/run.mjs" /tmp/wine-layout.exe
