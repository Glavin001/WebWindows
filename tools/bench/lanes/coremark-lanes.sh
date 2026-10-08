#!/bin/bash
# CoreMark (30000 iterations, i586 code, no C runtime) on the lanes that
# need a program without msvcrt: the .exe under native Wine, ours ahead of
# time and with the in-browser translator, Theseus (native and WebAssembly),
# v86 (as a multiboot kernel) and Wine-Assembly; plus the same source built
# for Linux, natively and under qemu-i386. Seconds of the measured part.
#
#   tools/bench/lanes/coremark-lanes.sh [rounds]
#
# Opt-in lanes, by environment:
#   THESEUS=<checkout of github.com/evmar/theseus>   (needs nightly Rust with
#           rust-src and the wasm32 target, and wasm-bindgen on PATH)
#   V86_DIR=<dir with `npm install v86`> V86_BIOS=<v86 checkout>/bios
#   WINE_ASSEMBLY=<checkout, with ../wine-assembly-console.patch applied>
set -e
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../.." && pwd)
cm=$root/target/coremark-src
[ -d "$cm" ] || node "$root/tools/bench/coremark.mjs" --build-only --tiers native
out=$root/target/lanes
rm -rf "$out" && mkdir -p "$out/win32" "$out/mb"
rounds=${1:-3}
defs=(-O2 -march=i586 -mtune=generic -fno-isolate-erroneous-paths-dereference -DHAS_FLOAT=0 -DMAIN_HAS_NOARGC=1
  -DPERFORMANCE_RUN=1 -DITERATIONS=30000 '-DFLAGS_STR="-O2"' '-DCOMPILER_VERSION="GCC"')
srcs=("$cm"/core_list_join.c "$cm"/core_main.c "$cm"/core_matrix.c "$cm"/core_state.c "$cm"/core_util.c)

# The Windows build: kernel32 only (GetTickCount, WriteFile).
cp "$cm"/barebones/{core_portme.c,core_portme.h,ee_printf.c} "$out/win32/"
patch -s -d "$out/win32" -p1 < "$here/coremark-win32.patch"
i686-w64-mingw32-gcc "${defs[@]}" -ffreestanding -nostdlib -I"$out/win32" -I"$cm" "${srcs[@]}" \
  "$out"/win32/{core_portme.c,ee_printf.c} "$here/win32-start.c" -e _start -lkernel32 -lgcc -o "$out/coremark.exe"
# The multiboot kernel: serial output, @@START/@@STOP around the timed part.
cp "$cm"/barebones/{core_portme.c,core_portme.h,ee_printf.c} "$out/mb/"
patch -s -d "$out/mb" -p1 < "$here/coremark-multiboot.patch"
gcc -m32 "${defs[@]}" -fno-pie -no-pie -fno-stack-protector -ffreestanding -nostdlib -static -I"$out/mb" -I"$cm" \
  -Wl,-T,"$here/multiboot.ld" -Wl,--build-id=none "$here/multiboot-boot.S" "${srcs[@]}" \
  "$out"/mb/{core_portme.c,ee_printf.c} "$here/multiboot-libc.c" -lgcc -o "$out/coremark.mb"
# The Linux build (with the C library: floating point in its report).
(cd "$cm" && gcc -m32 "${defs[@]/-DHAS_FLOAT=0/-DHAS_FLOAT=1}" -I. -Isimple "${srcs[@]}" simple/core_portme.c -o "$out/coremark.native")

ticks() { awk '/Total ticks/ { printf "%.3f", $4 / 1000; exit }'; }
ips() { awk '/Iterations\/Sec/ { printf "%.3f", 30000 / $3; exit }'; }
host() { awk '/host time/ { printf "%.3f", $4 / 1000; exit }'; }

if [ -n "$THESEUS" ]; then
  # Translate, adding the entry points the run reports missing, until it runs.
  t=$THESEUS
  grep -q '"out/coremark"' "$t/Cargo.toml" || sed -i 's|  "out/winapi",|  "out/winapi",\n  "out/coremark",|' "$t/Cargo.toml"
  mkdir -p "$t/out/coremark/src" "$out/sdl"
  sed -e 's/winapi-exe/coremark-exe/' -e 's/winapi_lib/coremark/' "$t/out/winapi/Cargo.toml" > "$t/out/coremark/Cargo.toml"
  cp "$t/out/winapi/src/main.rs" "$t/out/winapi/src/lib.rs" "$t/out/coremark/src/"
  gcc -O2 -shared -fPIC "$here/theseus-sdl-stub.c" -o "$out/sdl/libSDL3.so"
  : > "$out/missing.txt"
  for i in 1 2 3 4 5; do
    (cd "$t" && RUSTFLAGS="-L $out/sdl" CARGO_TARGET_DIR=$out/theseus-target cargo run -q --release -p tc -- \
      --exe "$out/coremark.exe" --out out/coremark --entry-points-file "$out/missing.txt" > /dev/null 2>&1)
    (cd "$t" && RUSTFLAGS="-L $out/sdl" CARGO_TARGET_DIR=$out/theseus-target cargo build -q --release -p coremark-exe)
    THESEUS_MISSING_ADDRS=$out/missing.txt LD_LIBRARY_PATH=$out/sdl "$out/theseus-target/release/coremark-exe" > "$out/theseus.txt" 2>&1 || true
    grep -q crcfinal "$out/theseus.txt" && break
  done
  link=""
  for a in --shared-memory --max-memory=1073741824 --import-memory --export=__heap_base --export=__wasm_init_tls \
    --export=__tls_size --export=__tls_align --export=__tls_base; do link="$link -Clink-args=$a"; done
  (cd "$t" && RUSTFLAGS="-Ctarget-feature=+atomics $link" CARGO_TARGET_DIR=$out/theseus-target cargo +nightly build -q --lib --release \
    -Z build-std=std,panic_abort --target wasm32-unknown-unknown -p coremark-exe)
  wasm-bindgen --out-dir "$out/theseus-web" --target web --reference-types "$out/theseus-target/wasm32-unknown-unknown/release/coremark.wasm"
  cp "$here/theseus-run.mjs" "$out/theseus-web/run.mjs"
fi

# What ran: keep this with any results.
echo "date $(date -u +%Y-%m-%dT%H:%MZ)"
echo "machine $(awk -F': ' '/model name/ { print $2; exit }' /proc/cpuinfo), $(nproc) CPUs, Linux $(uname -r)"
echo "wwt $(git -C "$root" rev-parse --short HEAD)"
echo "node $(node --version) (V8 $(node -p process.versions.v8))"
echo "gcc $(gcc -dumpfullversion); mingw $(i686-w64-mingw32-gcc -dumpversion)"
command -v wine > /dev/null && echo "wine $(wine --version)"
command -v qemu-i386 > /dev/null && echo "qemu $(qemu-i386 --version | head -1)"
if [ -n "$THESEUS" ]; then
  echo "theseus $(git -C "$THESEUS" rev-parse --short HEAD) ($(git -C "$THESEUS" log -1 --format=%cs)); $(rustc --version); $(rustc +nightly --version); $(wasm-bindgen --version)"
fi
if [ -n "$V86_DIR" ]; then
  echo "v86 $(node -p "require('$V86_DIR/node_modules/v86/package.json').version") (npm); BIOS from $(git -C "$V86_BIOS/.." rev-parse --short HEAD 2> /dev/null || echo "$V86_BIOS")"
fi
[ -n "$WINE_ASSEMBLY" ] && echo "wine-assembly $(git -C "$WINE_ASSEMBLY" rev-parse --short HEAD) ($(git -C "$WINE_ASSEMBLY" log -1 --format=%cs))"

lane() { printf '%-26s %s\n' "$1" "${2:-—}"; }
for r in $(seq "$rounds"); do
  echo "round $r"
  lane native "$("$out/coremark.native" | ips)"
  command -v qemu-i386 > /dev/null && lane qemu-i386 "$(qemu-i386 "$out/coremark.native" | ips)"
  command -v wine > /dev/null && lane "exe on native Wine" "$(WINEDEBUG=-all wine "$out/coremark.exe" 2> /dev/null | ticks)"
  lane "ours (ahead of time)" "$(node "$root/runtime/node/wine.mjs" "$out/coremark.exe" 2> /dev/null | ticks)"
  lane "ours (in-browser)" "$(WWT_TRANSLATOR=wasm node "$root/runtime/node/wine.mjs" "$out/coremark.exe" 2> /dev/null | ticks)"
  if [ -n "$THESEUS" ]; then
    lane "theseus (native)" "$(cd /tmp && LD_LIBRARY_PATH=$out/sdl "$out/theseus-target/release/coremark-exe" 2> /dev/null | ticks)"
    lane "theseus (wasm)" "$(node "$out/theseus-web/run.mjs" coremark 2> /dev/null | ticks)"
  fi
  [ -n "$V86_DIR" ] && lane "v86 (multiboot kernel)" "$(node "$here/v86-run.mjs" "$out/coremark.mb" 2> /dev/null | host)"
done
if [ -n "$WINE_ASSEMBLY" ]; then
  lane wine-assembly "$(cd "$WINE_ASSEMBLY" && WA_DUMP_CONSOLE=80 node test/run.js --exe="$out/coremark.exe" --max-batches=1000000000 \
    --stuck-after=0 --max-seconds=1200 --quiet-api --quiet-blocks --no-renderer --real-ticks 2> /dev/null | ticks)"
fi
