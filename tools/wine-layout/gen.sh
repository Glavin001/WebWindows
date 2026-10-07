#!/bin/sh
# Computes structure layouts from Wine's headers (needs the Wine source tree
# in WINE_SRC and a configured build in WINE_BUILD) and runs the result.
set -e
cd "$(dirname "$0")"
WINE_SRC=${WINE_SRC:-/opt/wine-src/wine-11.0}
WINE_BUILD=${WINE_BUILD:-/opt/wine-build}
i686-w64-mingw32-gcc -w -nostdlib -Wl,-e,_start -o /tmp/wine-layout.exe layout.c -lkernel32 -I"$WINE_BUILD/include" -I"$WINE_SRC/include" -I"$WINE_SRC/include/msvcrt" -D__WINESRC__ -nostdinc -isystem "$(i686-w64-mingw32-gcc -print-file-name=include)" 2>&1 >&2
node "$(dirname "$0")/../../runtime/node/run.mjs" /tmp/wine-layout.exe
