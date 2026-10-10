# WebWindows

Run unmodified 32-bit Windows programs in the browser by translating their
x86 machine code to WebAssembly ahead of time, translating anything the
first pass missed while the program runs, and caching the result. Everything
happens on the user's machine; nothing is uploaded.

This repository implements **Milestones 1 to 4** of
[the plan](docs/plan.md) (problems, design and milestones M1–M8): the
translator core and test harness (M1); Windows programs running on Wine's
own DLLs translated to WebAssembly (M2); the translator, fast mode and a
translation cache in the browser, with a folder picker (M3); and windowed
programs: Wine's win32u and wineserver compiled with Emscripten, a browser
display driver, keyboard and mouse (M4). Wine's Minesweeper and Notepad run
in a page. See [docs/milestone-1.md](docs/milestone-1.md) to
[docs/milestone-4.md](docs/milestone-4.md) for status, measurements and
known limitations.

64-bit (x86-64) programs have started on a second stack alongside the
32-bit one ([the plan](docs/plan.md#64-bit-programs)): the translator, the
runtime and the instruction and program tests handle x86-64 code, on a 32-bit
or a 64-bit (memory64) WebAssembly memory, and 64-bit programs run
at their own 64-bit addresses (`0x1_4000_0000` for an .exe), on the
Milestone 1 shims and on Wine's own x86_64 DLLs, translated, with Wine's
Unix side built for wasm64: 64-bit Notepad and Minesweeper run in the
browser page next to their 32-bit builds. See
[docs/milestone-9.md](docs/milestone-9.md).

## Quick start

On a Mac (or anywhere with Docker), build in the container and run the
page on the host: `tools/docker/run.sh tools/docker/build-all.sh`, then
`node runtime/web/serve.mjs 8080`. [AGENTS.md](AGENTS.md) has what runs
where. Natively on Linux:

Requirements: Rust (stable, with the `wasm32-unknown-unknown` target),
Node 22. To build test programs and record instruction fixtures you also need
`gcc-multilib` and `gcc-mingw-w64-i686` (Debian/Ubuntu), plus
`gcc-mingw-w64-x86-64` for 64-bit programs.

```sh
cargo build -p wwt-cli                    # the `wwt` translator CLI
cargo build -p wwt-wasm --target wasm32-unknown-unknown --profile release-wasm

# Translate and run a Windows console program in Node:
node runtime/node/run.mjs tests/programs/hello.exe

# 64-bit programs (needs gcc-mingw-w64-x86-64) are detected from the PE
# header and run at their preferred base on a 64-bit (memory64) WebAssembly
# memory (--mem32 moves them below 4 GB in a 32-bit one):
x86_64-w64-mingw32-gcc -O2 -nostdlib -o hello64.exe tests/programs/hello.c -lkernel32 -Wl,-e,start
node runtime/node/run.mjs hello64.exe

# On translated Wine (build Wine's i386 PE DLLs first; needs flex, bison):
tools/wine/build.sh
cargo build --release -p wwt-cli
cargo build -p wwt-heap --target wasm32-unknown-unknown --profile release-wasm  # ntdll's heap, native
cargo build -p wwt-strings --target wasm32-unknown-unknown --profile release-wasm  # string functions, native
node runtime/node/wine.mjs tests/programs/hello.exe

# 64-bit programs on translated x86_64 Wine (a second build tree):
ARCH=x86_64 tools/wine/build.sh
node runtime/node/wine.mjs hello64.exe

# Windowed programs: Wine's Unix side with Emscripten (emcc on PATH), the
# DLLs, fonts and programs, then a program with a screenshot of its screen:
tools/wine/build.sh ntdll kernelbase kernel32 msvcrt ucrtbase advapi32 sechost user32 gdi32 \
  win32u imm32 combase comctl32 comctl32_v6 coml2 cryptbase ole32 oleaut32 rpcrt4 uxtheme \
  comdlg32 shcore shell32 shlwapi programs/winemine programs/notepad fonts \
  wined3d d3d9      # Direct3D 9, with wined3d's WebGPU backend
sh native/wine-unix/build.sh
node runtime/node/wine.mjs --screenshot mine.png --run-for 5000 \
  --input "500:click 60,120" /opt/wine-build/programs/winemine/i386-windows/winemine.exe

# 64-bit windowed programs: the same DLLs and programs for x86_64, and
# Wine's Unix side for wasm64 (its 64-bit table needs Node 24):
ARCH=x86_64 tools/wine/build.sh ntdll kernelbase kernel32 msvcrt ucrtbase advapi32 sechost user32 \
  gdi32 win32u imm32 combase comctl32 comctl32_v6 coml2 cryptbase ole32 oleaut32 rpcrt4 uxtheme \
  comdlg32 shcore shell32 shlwapi programs/winemine programs/notepad fonts
ARCH=x86_64 sh native/wine-unix/build.sh
node runtime/node/wine.mjs --screenshot notepad64.png --run-for 6000 --input "1500:text Hello" \
  /opt/wine-build64/programs/notepad/x86_64-windows/notepad.exe

# Or in the browser (Chromium): serve with the required headers, open the
# page and pick a folder that contains an .exe, or try a sample: hello as
# a 32-bit and a 64-bit console program, and Wine's Minesweeper and Notepad
# as 32-bit and 64-bit programs. For "on Wine", build the bundles first.
node runtime/node/wine-bundle.mjs
node runtime/node/wine-bundle.mjs --arch x64
node runtime/web/serve.mjs 8080
# http://localhost:8080/runtime/web/
```

CI runs for pull requests and for `main`. Its deploy job puts the static
site on Vercel (`tools/site/deploy.sh`, with the `VERCEL_TOKEN` secret)
as a GitHub deployment: the `Preview` environment for pull requests (the
pull request links it), `Production` for `main`. The site goes up
prebuilt, with its cross-origin isolation headers, so Vercel builds
nothing (`vercel.json` turns its Git builds off).

The site's sample list starts with benchmarks the deploy job builds from
their pinned sources (`tools/site/apps.mjs`; nothing built is committed):
SQLite's speedtest1, Lua with the suite's workloads, CoreMark and
apibench, each one click away with its arguments and input files. The
page translates with bounds traps instead of memory checks where the
browser allows (`docs/memory-traps.md`); tick "faithful memory checks" to
compare. For a local site, run `node tools/site/apps.mjs` before
`tools/site/build.sh`.

The CLI also inspects and translates binaries:

```sh
target/debug/wwt info program.exe          # sections, imports, exports
target/debug/wwt translate program.exe     # -> program.wasm (+ --report)
target/debug/wwt ir program.exe --func 401000   # optimized IR
target/debug/wwt wat program.exe           # generated WebAssembly as text
target/debug/wwt pack program.exe -o out/  # static web app directory
```

Picking up the graphics work (Direct3D, DirectDraw, OpenGL, the games)?
Start with [docs/handoff.md](docs/handoff.md): state, setup, commands,
open problems and gotchas.

## How it fits together

```
 program.exe ──► wwt (Rust) ──────────────► program.wasm ──┐
                  load → discover → decode                  │
                  → lift (IR) → optimize                    ▼
                  → WebAssembly → link            runtime (JS + kernel.wasm)
                                                  one shared memory:
 code missed ahead of time ◄── fast mode ◄──────  [ Windows process | native ]
 (translator compiled to wasm, runs in the page)  dispatcher, lookup tables,
          └──► profile ──► next ahead-of-time pass   Win32 shims (M1 only)
```

* **Translator** (`crates/wwt`): the seven layers of the plan, one module
  each — `pe` (load), `discover` (find code, jump tables; decode with
  `iced-x86`), `lift` (x86 semantics → IR, including x87 and
  SSE/SSE2/MMX), `opt` (flag
  lowering with compare-and-branch fusion, folding, liveness-based DCE),
  `codegen` (one WebAssembly function per x86 function, structured control
  flow from the dominator tree, `wasm-encoder`), `translate` (orchestration
  and module metadata). `abi` is the contract with the runtime.
* **Runtime** (`runtime/`): `runtime.mjs` owns the memory map, the function
  table and the two-level address lookup; `kernel` (generated by `wwt
  kernel`) holds the dispatcher loop; `fastmode.mjs` translates missed code
  with `crates/wwt-wasm`; `win32.mjs` is a temporary Win32 layer (kernel32 and
  msvcrt shims) from M1; `wine/` stands in for Wine's Unix side under
  Wine's DLLs, which are translated like any other code (M2), and loads the
  parts of it compiled with Emscripten (`native/wine-unix`: wineserver,
  win32u and the browser display driver, M4); `node/` and `web/` are the
  two hosts.
* **Memory**: x86 address `A` is WebAssembly address `A`. The low *guest
  limit* bytes (1 GB by default, 2 GB on Wine) are the Windows process; the
  native runtime lives above it, and with Wine's Unix side, the Emscripten
  module's data, heap and stacks above that (`native/wine-unix/build.sh`). Registers live in WebAssembly locals inside a function and
  are written back to a per-thread CPU struct at calls, exits and fault
  points.

## Testing

The plan's verification pipeline, as implemented:

| Layer | What | Command |
| --- | --- | --- |
| 1. Instructions | Every legacy instruction form valid in 32-bit user mode (integer, flag-fusion pairs, x87, SSE/SSE2/MMX), random inputs, recorded on a real x86 CPU by `tools/oracle` | `cargo test -p wwt-testkit` |
| 2. Programs | Hand-written C programs, Csmith programs and GCC's torture tests built with MinGW vs. a native `gcc -m32` build, on the shims or (`--wine`) on translated Wine | `node tests/programs/check.mjs [--csmith N] [--torture DIR] [--wine]` |
| 3. Wine's own tests | Every unit of Wine's `kernel32`, `user32` and `gdi32` conformance tests on translated Wine, against recorded baselines | `node tests/wine/winetest.mjs --baseline tests/wine/baseline/user32_test.json .../user32_test.exe` |
| 4. Real software (start) | The browser front end in headless Chromium: the cache and profile loop, the folder picker, Wine's Minesweeper and Notepad driven with mouse and keyboard, and Direct3D 9 test programs on WebGPU; the same windowed programs headless in Node with screenshots; a Direct3D 9 benchmark | `node tests/web/browser.mjs`, `tests/web/picker.mjs`, `tests/web/gui.mjs`, `tests/wine/gui.mjs`, `tests/web/d3d9bench.mjs` |
| 5. Own output | Snapshots of IR and WAT for committed binaries | `cargo test -p wwt --test snapshots` |
| Speed | CoreMark native vs. Emscripten vs. translated, with checksum check; profiles by function | `node tools/bench/coremark.mjs`, see [docs/performance.md](docs/performance.md) |
| Comparison | SQLite, Lua, CoreMark and Windows API workloads on native, Emscripten, Wine, qemu-i386, Wine-Assembly and ours; first-launch costs | `node tools/bench/suite.mjs`, `tools/bench/firstlaunch.mjs`, see [docs/comparison.md](docs/comparison.md) |

Instruction fixtures (`tests/fixtures/instructions/*.jsonl.gz`) are recorded
on x86 hardware and replayed anywhere:

```sh
cargo run -p wwt-testkit --bin record            # re-record on this CPU
cargo run -p wwt-testkit --bin record -- --check # verify fixtures match it
cargo run -p wwt-testkit --bin coverage -- *.exe # forms in binaries vs suite
```

CI (`.github/workflows/ci.yml`) runs all of this on x86 Linux runners,
re-checks the fixtures against the runner's CPU, runs the same MinGW
executables natively on a Windows runner, and runs the Emscripten spike.
Wine is built once per run (from a cached build tree) and its tests and
the GCC torture tests run in parallel shards (`check.mjs --shard K/N`).
A change to nothing but documentation (Markdown, `docs/`) skips it all;
the `CI passed` job sums up the run and is the one check to require.

## Repository layout

```
crates/wwt          translator library
crates/wwt-cli      `wwt` command-line tool
crates/wwt-wasm     translator compiled to WebAssembly (fast mode, browser)
crates/wwt-heap     ntdll's heap as native WebAssembly, for translated Wine
crates/wwt-strings  hot string, locale and TLS functions as native WebAssembly, for translated Wine
crates/wwt-testkit  instruction generator, oracle driver, wasmtime runner
crates/d3dgpu-*     Direct3D 9/10/11 on WebGPU core: protocol, shader translators
                    (SM1-3, DXBC SM4/5), emulation library, render core (wgpu),
                    scenes, web build
runtime/            JavaScript runtime (Node and browser hosts)
runtime/d3dgpu/     d3dgpu demo page, render and producer workers, test runner
native/wine-unix    Wine's Unix side (wineserver, win32u, display driver) for Emscripten
native/wined3d-wgpu wined3d's WebGPU backend (patched into Wine's source by tools/wine/build.sh)
tools/oracle        native x86 oracle for instruction tests
tools/wine          builds Wine's i386 PE DLLs, programs, tests and fonts
tools/wine-layout   generates Wine's structure layouts for the runtime
tools/samples       builds the test programs the web page lists (runtime/web/samples.json)
tools/torture       fetches GCC's torture tests
tools/bench         CoreMark tiers incl. Emscripten, profiler, per-function comparison, A/B
tools/site          assembles the static site (with benchmarks built from source) and deploys it to Vercel
tests/              fixtures, test programs, Csmith runtime, browser test
spikes/             M1 spikes: memory size, Emscripten above the guest limit
docs/               the plan (plan.md), each milestone's status, performance guide
```

## Direct3D on WebGPU

The `d3dgpu-*` crates are the reusable core for running Direct3D 9, 10 and
11 games on WebGPU. wined3d's `adapter_wgpu` backend (`native/wined3d-wgpu`)
drives it through a command stream: Direct3D 9 programs on translated Wine
draw through it in the browser. See [docs/d3d-webgpu.md](docs/d3d-webgpu.md).

```sh
cargo test -p d3dgpu-core                     # 54 scenes (D3D9 and D3D11) on native wgpu
cargo test -p d3dgpu-dxbc                     # DXBC translator (tools/dxbc/ has the corpora)
runtime/d3dgpu/build.sh                       # wasm + bindings (fetches wasm-bindgen-cli)
node tests/web/d3dgpu.mjs                     # the same scenes in headless Chromium
node runtime/web/serve.mjs 8080               # http://localhost:8080/runtime/d3dgpu/
                                              # (also /runtime/d3dgpu/ on the deployed site)
```
