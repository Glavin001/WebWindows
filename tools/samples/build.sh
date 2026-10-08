#!/bin/sh
# Builds the Windows test programs the web page offers as samples
# (runtime/web/samples.json), with MinGW at -O2: into tests/programs/samples/
# the Windows-only tests (tests/wine/win32/*.c, linked with the libraries
# their `libs:` line names) and the DirectDraw and DirectSound demo
# (tests/web/ddsound.c); next to their sources the Direct3D 9 programs
# (tests/programs/gui/d3d9*.c, which load d3d9.dll themselves) and the
# OpenGL ones (gltri.c, glbench.c), which use opengl32. The
# executables are committed, so the page and the site need no compiler.
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
for name in d3d9tri d3d9bench; do
  i686-w64-mingw32-gcc -O2 -s -Wno-missing-braces -o "$root/tests/programs/gui/$name.exe" "$root/tests/programs/gui/$name.c"
done
# OpenGL through opengl32 (native/opengl32, over Direct3D 9).
for name in gltri glbench; do
  i686-w64-mingw32-gcc -O2 -s -o "$root/tests/programs/gui/$name.exe" "$root/tests/programs/gui/$name.c" -lopengl32 -lgdi32
done
ls -la "$out" "$root"/tests/programs/gui/d3d9*.exe "$root"/tests/programs/gui/gl*.exe
