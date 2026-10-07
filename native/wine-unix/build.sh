#!/bin/sh
# Builds Wine's Unix side for the browser with Emscripten (Milestone 4):
# wineserver, linked into the same module and called in-process.
#
#   native/wine-unix/build.sh
#
# Environment: WINE_SRC, WINE_BUILD (as tools/wine/build.sh), EMSDK.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
WINE_SRC=${WINE_SRC:-/opt/wine-src/wine-11.0}
WINE_BUILD=${WINE_BUILD:-/opt/wine-build}
out=$root/target/wine-unix
if ! command -v emcc > /dev/null; then export PATH="${EMSDK:-/opt/emsdk}/upstream/emscripten:$PATH"; fi
mkdir -p "$out/include" "$out/obj"
sh "$here/gen-config.sh" "$WINE_BUILD/include/config.h" "$out/include/config.h"
python3 "$here/prepare.py" "$WINE_SRC" "$WINE_BUILD" "$out/src"

# The browser's file system: Wine's data files live under /wine (MEMFS).
# Atomics and bulk memory: the module shares the guest's memory, which is a
# shared WebAssembly memory.
CFLAGS="-O2 -g0 -w -matomics -mbulk-memory -D__WINESRC__ -D_GNU_SOURCE -include stdarg.h -I$out/include -I$WINE_BUILD/include -I$out/src/include"
CFLAGS="$CFLAGS -DBINDIR=\"/wine/bin\" -DDATADIR=\"/wine/share\" -DLIBDIR=\"/wine/lib\""
fail=0
compile() { # dir file.c extra-flags
  obj="$out/obj/$1/$(basename "$2" .c).o"
  mkdir -p "$(dirname "$obj")"
  if [ ! -f "$obj" ] || [ "$2" -nt "$obj" ]; then
    # shellcheck disable=SC2086
    if ! emcc -c $CFLAGS $3 "$2" -o "$obj" 2> "$obj.err"; then
      echo "FAIL $2: $(grep -m1 'error' "$obj.err" | cut -c1-200)"
      fail=1
    fi
  fi
}
# wineserver, with its main() renamed: the in-process glue starts it.
for f in "$out"/src/server/*.c; do
  case $f in */main.c) extra=-Dmain=wineserver_main ;; *) extra= ;; esac
  compile server "$f" "-I$out/src/server $extra"
done
compile inproc "$here/inproc/server.c" "-I$out/src/server"
compile inproc "$here/inproc/client.c" "-DWINE_UNIX_LIB -I$out/src/dlls/ntdll -I$out/src/dlls/ntdll/unix"
# ntdll's Unix side: the parts that talk to wineserver.
for f in sync registry; do
  compile ntdll "$out/src/dlls/ntdll/unix/$f.c" "-DWINE_UNIX_LIB -I$out/src/dlls/ntdll -I$out/src/dlls/ntdll/unix"
done
[ $fail = 0 ] || exit 1

# Memory layout, shared with runtime/runtime.mjs: the guest region (2 GB for
# Wine), the runtime's native area, then this module's data, stack and heap.
GUEST_LIMIT=$((0x80000000))
NATIVE_SIZE=$((0x10000000))
EXTRA_SIZE=$((0x10000000))
GLOBAL_BASE=$((GUEST_LIMIT + NATIVE_SIZE))
MEMORY_SIZE=$((GLOBAL_BASE + EXTRA_SIZE))

# Exports: the NT calls Wine's Unix code implements here, plus the glue's
# own entry points (marked EMSCRIPTEN_KEEPALIVE).
exports=$(grep -ohE '^NTSTATUS WINAPI Nt[A-Za-z]+' "$out/src/dlls/ntdll/unix/sync.c" "$out/src/dlls/ntdll/unix/registry.c" \
  | awk '{print "_"$3}' | sort -u | tr '\n' ',')_NtClose,_NtDuplicateObject,_malloc,_free
emcc "$out"/obj/*/*.o -o "$out/wine_unix.mjs" -O2 --no-entry \
  -sMODULARIZE -sEXPORT_ES6 -sEXPORT_NAME=createWineUnix -sENVIRONMENT=web,worker,node \
  -sIMPORTED_MEMORY -sSHARED_MEMORY -sALLOW_MEMORY_GROWTH=0 \
  -sGLOBAL_BASE=$GLOBAL_BASE -sINITIAL_MEMORY=$MEMORY_SIZE -sSTACK_SIZE=4MB \
  -sEXPORTED_FUNCTIONS="$exports" -sEXPORTED_RUNTIME_METHODS=FS \
  -sERROR_ON_UNDEFINED_SYMBOLS=1
cat > "$out/wine_unix.json" <<JSON
{ "guestLimit": $GUEST_LIMIT, "nativeSize": $NATIVE_SIZE, "extraSize": $EXTRA_SIZE, "globalBase": $GLOBAL_BASE }
JSON
ls -la "$out/wine_unix.wasm"
