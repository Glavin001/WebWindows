#!/bin/sh
# Builds Wine's Unix side for the browser with Emscripten (Milestone 4):
# wineserver, linked into the same module and called in-process.
#
#   native/wine-unix/build.sh
#   ARCH=x86_64 native/wine-unix/build.sh   # for 64-bit Wine: a wasm64
#                                           # module in target/wine-unix64
#
# Environment: ARCH, WINE_SRC, WINE_BUILD (as tools/wine/build.sh), EMSDK.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
ARCH=${ARCH:-i386}
WINE_SRC=${WINE_SRC:-/opt/wine-src/wine-11.0}
case $ARCH in
  i386) WINE_BUILD=${WINE_BUILD:-/opt/wine-build}; out=$root/target/wine-unix; wasm64= ;;
  # wasm64 (memory64) with the x86_64 data layout (prepare.py), sharing the
  # 64-bit guest's memory.
  x86_64) WINE_BUILD=${WINE_BUILD:-/opt/wine-build64}; out=$root/target/wine-unix64; wasm64=-sMEMORY64=1 ;;
  *) echo "ARCH must be i386 or x86_64" >&2; exit 2 ;;
esac
if ! command -v emcc > /dev/null; then export PATH="${EMSDK:-/opt/emsdk}/upstream/emscripten:$PATH"; fi
mkdir -p "$out/include" "$out/obj"
# Headers Wine generates from IDL (d3d11.h for win32u's d3dkmt.c, ...): a
# Wine build tree only has the ones its DLLs needed, so make them all.
idl_headers=$(for f in "$WINE_SRC"/include/*.idl; do n=$(basename "$f" .idl); \
  grep -q "^include/$n.h:" "$WINE_BUILD/Makefile" && echo "include/$n.h"; done)
make -s -C "$WINE_BUILD" -j"$(nproc)" $idl_headers
sh "$here/gen-config.sh" "$WINE_BUILD/include/config.h" "$out/include/config.h"
python3 "$here/prepare.py" "$WINE_SRC" "$WINE_BUILD" "$out/src" "$ARCH"

# Memory layout, shared with runtime/runtime.mjs: the guest region (2 GB for
# i386 Wine, 8 GB for x86_64), the runtime's native area, then this module's
# data, stack and heap.
if [ "$ARCH" = x86_64 ]; then GUEST_LIMIT=$((0x200000000)); else GUEST_LIMIT=$((0x80000000)); fi
NATIVE_SIZE=$((0x10000000))
EXTRA_SIZE=$((0x10000000))
GLOBAL_BASE=$((GUEST_LIMIT + NATIVE_SIZE))
MEMORY_SIZE=$((GLOBAL_BASE + EXTRA_SIZE))

# The browser's file system: Wine's data files live under /wine (MEMFS).
# Atomics and bulk memory: the module shares the guest's memory, which is a
# shared WebAssembly memory.
CFLAGS="-O2 -g0 -w $wasm64 -DGUEST_LIMIT=$GUEST_LIMIT -matomics -mbulk-memory -D__WINESRC__ -D_GNU_SOURCE -include stdarg.h -I$out/include -I$WINE_BUILD/include -I$out/src/include"
# Names wineserver and win32u share with the rest of the module: in Wine
# they are separate programs and libraries, here one module. The loop at the
# end of compile_all finds clashes with nm and renames them in the server
# (server_names.h) or win32u (win32u_names.h), then recompiles.
touch "$out/include/server_names.h" "$out/include/win32u_names.h"

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
compile_all() {
  # wineserver, with its main() renamed: the in-process glue starts it. Its
  # data files live under /wine in the module's in-memory file system.
  for f in "$out"/src/server/*.c; do
    case $f in
      */main.c) extra=-Dmain=wineserver_main ;;
      */unicode.c) extra='-DBINDIR="/wine/bin" -DDATADIR="/wine/share"' ;;
      *) extra= ;;
    esac
    compile server "$f" "-I$out/src/server -include $out/include/server_names.h $extra"
  done
  compile server "$here/inproc/server.c" "-I$out/src/server -include $out/include/server_names.h"
  compile inproc "$here/inproc/client.c" "-DWINE_UNIX_LIB -I$out/src/dlls/ntdll -I$out/src/dlls/ntdll/unix"
  # ntdll's Unix side: the parts that talk to wineserver.
  for f in sync registry env security; do
    compile ntdll "$out/src/dlls/ntdll/unix/$f.c" "-DWINE_UNIX_LIB -I$out/src/dlls/ntdll -I$out/src/dlls/ntdll/unix"
  done
  # win32u's Unix side (the files Wine marks "makedep unix"; main.c is the
  # PE side, translated like any other DLL).
  for f in "$out"/src/dlls/win32u/*.c "$out"/src/dlls/win32u/dibdrv/*.c; do
    extra=
    case $f in
      */win32u/main.c) continue ;;
      */dibdrv/*) dir=win32u/dibdrv ;;
      */win32u/freetype.c) dir=win32u; extra="-sUSE_FREETYPE=1 -Ddlopen=wasm_dlopen -Ddlsym=wasm_dlsym -Ddlerror=wasm_dlerror" ;;
      *) dir=win32u ;;
    esac
    compile $dir "$f" "$extra -DWINE_UNIX_LIB -D_WIN32U_ -D__wine_unix_call_funcs=win32u_unix_call_funcs -include $out/include/win32u_names.h -I$out/src/dlls/win32u -I$out/src/dlls/ntdll"
  done
  # Typed thunks for win32u's system calls, and the entry points around them.
  python3 "$here/gen-syscalls.py" "$out/src" "$out/src/win32u_syscalls.c" "$ARCH"
  W32U="-DWINE_UNIX_LIB -D_WIN32U_ -I$out/src/dlls/win32u -I$out/src/dlls/ntdll -I$here/inproc"
  compile inproc "$out/src/win32u_syscalls.c" "$W32U"
  # Typed thunks for the NT calls the host routes to this module.
  NTFILES="$out/src/dlls/ntdll/unix/sync.c $out/src/dlls/ntdll/unix/registry.c $out/src/dlls/ntdll/unix/security.c"
  { grep -ohE '^NTSTATUS WINAPI Nt[A-Za-z]+' $NTFILES | awk '{print $3}'; echo NtClose; echo NtDuplicateObject; } \
    | sort -u > "$out/src/nt_calls.txt"
  # shellcheck disable=SC2086
  python3 "$here/gen-ntcalls.py" "$out/src/nt_calls.c" "$ARCH" "$out/src/nt_calls.txt" $NTFILES "$here/inproc/client.c"
  compile inproc "$out/src/nt_calls.c" "-DWINE_UNIX_LIB -I$out/src/dlls/ntdll -I$out/src/dlls/ntdll/unix"
  compile inproc "$here/inproc/win32u.c" "$W32U"
  compile driver "$here/driver/browser.c" "$W32U"
  compile inproc "$here/inproc/freetype.c" "-sUSE_FREETYPE=1"
  compile inproc "$here/inproc/host.c" "-DWINE_UNIX_LIB -I$out/src/dlls/ntdll -I$out/src/dlls/ntdll/unix"

}

NM=$(dirname "$(command -v emcc)")/../bin/llvm-nm
[ -x "$NM" ] || NM=llvm-nm
defined() { # object directories...
  for d in "$@"; do find "$out/obj/$d" -name '*.o'; done | xargs "$NM" --defined-only -g 2> /dev/null \
    | awk 'NF == 3 && $2 ~ /[TDBRVW]/ {print $3}' | sort -u
}
for round in 1 2 3; do
  compile_all
  [ $fail = 0 ] || exit 1
  new_server=$(comm -12 "$(defined server > "$out/def-server"; echo "$out/def-server")" \
    "$(defined win32u ntdll inproc driver > "$out/def-other"; echo "$out/def-other")")
  new_win32u=$(comm -12 "$(defined win32u > "$out/def-win32u"; echo "$out/def-win32u")" \
    "$(defined ntdll inproc driver > "$out/def-ntdll"; echo "$out/def-ntdll")" | grep -v '^wasm_\|^win32u_' || true)
  [ -z "$new_server$new_win32u" ] && break
  for n in $new_server; do echo "#define $n server_$n" >> "$out/include/server_names.h"; done
  for n in $new_win32u; do echo "#define $n win32u_$n" >> "$out/include/win32u_names.h"; done
  echo "renamed: $(echo $new_server $new_win32u)"
  [ -n "$new_server" ] && rm -rf "$out/obj/server"
  [ -n "$new_win32u" ] && rm -rf "$out/obj/win32u"
done
[ $fail = 0 ] || exit 1

# Exports: the NT calls Wine's Unix code implements here (through
# wasm_nt_call, see gen-ntcalls.py), plus the glue's own entry points
# (marked EMSCRIPTEN_KEEPALIVE).
exports=_wasm_nt_call,_malloc,_free,_wasm_win32u_syscall,_wasm_win32u_unix_call
# shellcheck disable=SC2086
emcc $(find "$out/obj" -name '*.o') -o "$out/wine_unix.mjs" -O2 $wasm64 --no-entry --pre-js "$here/inproc/pre.js" \
  -sUSE_FREETYPE=1 -sMODULARIZE -sEXPORT_ES6 -sEXPORT_NAME=createWineUnix -sENVIRONMENT=web,worker,node \
  -sIMPORTED_MEMORY -sSHARED_MEMORY -sALLOW_MEMORY_GROWTH=0 \
  -sGLOBAL_BASE=$GLOBAL_BASE -sINITIAL_MEMORY=$MEMORY_SIZE -sMAXIMUM_MEMORY=$MEMORY_SIZE -sSTACK_SIZE=4MB \
  -sEXPORTED_FUNCTIONS="$exports" -sEXPORTED_RUNTIME_METHODS=FS,HEAPU8,UTF8ToString \
  -sERROR_ON_UNDEFINED_SYMBOLS=1 -Wl,--error-limit=0 2> "$out/link.log" || { cat "$out/link.log"; exit 1; }
# Two definitions of a name with different signatures only warn, and one of
# them silently wins: never accept that.
if grep -q "signature mismatch" "$out/link.log"; then grep -A2 "signature mismatch" "$out/link.log"; exit 1; fi
cat > "$out/wine_unix.json" <<JSON
{ "arch": "$ARCH", "guestLimit": $GUEST_LIMIT, "nativeSize": $NATIVE_SIZE, "extraSize": $EXTRA_SIZE, "globalBase": $GLOBAL_BASE }
JSON
cp "$out/src/win32u_syscalls.json" "$out/win32u_syscalls.json"
cp "$out/src/nt_calls.json" "$out/nt_calls.json"
ls -la "$out/wine_unix.wasm"
