#!/bin/sh
# Computes structure layouts from Wine's headers (needs the Wine source tree
# in WINE_SRC and a configured build in WINE_BUILD) and runs the result.
set -e
here=$(cd "$(dirname "$0")" && pwd)
cd "$here"
# ARCH=x86_64 prints the 64-bit layouts (layout64.json).
ARCH=${ARCH:-i386}
WINE_SRC=${WINE_SRC:-/opt/wine-src/wine-11.0}
case $ARCH in
  # i386 symbols carry a leading underscore.
  i386) CC=i686-w64-mingw32-gcc; us=_; WINE_BUILD=${WINE_BUILD:-/opt/wine-build} ;;
  x86_64) CC=x86_64-w64-mingw32-gcc; us=; WINE_BUILD=${WINE_BUILD:-/opt/wine-build64} ;;
  *) echo "ARCH must be i386 or x86_64" >&2; exit 2 ;;
esac
# gen.sh [audio]: the core structures, or mmdevapi's audio driver calls.
src=${1:-layout}
$CC -w -nostdlib -Wl,-e,$us$([ "$src" = layout ] && echo start || echo entry) -o /tmp/wine-layout.exe "$src.c" -lkernel32 -I"$WINE_BUILD/include" -I"$WINE_SRC/include" -I"$WINE_SRC/include/msvcrt" -I"$WINE_SRC/dlls/mmdevapi" -D__WINESRC__ -nostdinc -isystem "$($CC -print-file-name=include)" 2>&1 >&2
node "$here/../../runtime/node/run.mjs" /tmp/wine-layout.exe
