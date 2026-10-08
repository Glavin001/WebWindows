#!/bin/sh
# Builds the Windows test programs the web page offers as samples
# (runtime/web/samples.json) into tests/programs/samples/, with MinGW at -O2:
# the Windows-only tests (tests/wine/win32/*.c, linked with the libraries
# their `libs:` line names) and the DirectDraw and DirectSound demo
# (tests/web/ddsound.c). The executables are committed, so the page and the
# site need no compiler.
#
#   tools/samples/build.sh
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
out=$root/tests/programs/samples
mkdir -p "$out"
for src in "$root"/tests/wine/win32/*.c; do
  name=$(basename "$src" .c)
  libs=$(sed -n 's/^ \* libs: //p' "$src")
  # shellcheck disable=SC2086
  i686-w64-mingw32-gcc -O2 -s -o "$out/$name.exe" "$src" $libs
done
i686-w64-mingw32-gcc -O2 -s -mwindows -o "$out/ddsound.exe" "$root/tests/web/ddsound.c" \
  -lddraw -ldsound -ldxguid -lgdi32 -luser32 -lwinmm
ls -la "$out"
