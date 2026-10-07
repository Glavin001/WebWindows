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
or a 64-bit (memory64) WebAssembly memory, and 64-bit console programs run
at their own 64-bit addresses (`0x1_4000_0000` for an .exe), on the
Milestone 1 shims and on Wine's own x86_64 DLLs, translated (console
programs so far; windowed ones need Wine's Unix side built for wasm64). See
[docs/milestone-9.md](docs/milestone-9.md).

## Quick start

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
node runtime/node/wine.mjs tests/programs/hello.exe

# 64-bit programs on translated x86_64 Wine (a second build tree):
ARCH=x86_64 tools/wine/build.sh
node runtime/node/wine.mjs hello64.exe

# Windowed programs: Wine's Unix side with Emscripten (emcc on PATH), the
# DLLs, fonts and programs, then a program with a screenshot of its screen:
tools/wine/build.sh ntdll kernelbase kernel32 msvcrt ucrtbase advapi32 sechost user32 gdi32 \
  win32u imm32 combase comctl32 comctl32_v6 coml2 cryptbase ole32 oleaut32 rpcrt4 uxtheme \
  comdlg32 shcore shell32 shlwapi programs/winemine programs/notepad fonts
sh native/wine-unix/build.sh
node runtime/node/wine.mjs --screenshot mine.png --run-for 5000 \
  --input "500:click 60,120" /opt/wine-build/programs/winemine/i386-windows/winemine.exe

# Or in the browser (Chromium): serve with the required headers, open the
# page and pick a folder that contains an .exe, or try Wine's Minesweeper
# and Notepad. For "on Wine", build the bundle first.
node runtime/node/wine-bundle.mjs
node runtime/web/serve.mjs 8080
# http://localhost:8080/runtime/web/
```

CI runs for pull requests and for `main`. Its wine job also deploys the
static site to Vercel (`tools/site/deploy.sh`, with the `VERCEL_TOKEN`
secret): a preview for each pull request, linked in a comment on it, and
production for `main`. The site goes up prebuilt, with its cross-origin
isolation headers, so Vercel builds nothing (`vercel.json` turns its Git
builds off).

The CLI also inspects and translates binaries:

```sh
target/debug/wwt info program.exe          # sections, imports, exports
target/debug/wwt translate program.exe     # -> program.wasm (+ --report)
target/debug/wwt ir program.exe --func 401000   # optimized IR
target/debug/wwt wat program.exe           # generated WebAssembly as text
target/debug/wwt pack program.exe -o out/  # static web app directory
```

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
| 4. Real software (start) | The browser front end in headless Chromium: the cache and profile loop, the folder picker, and Wine's Minesweeper and Notepad driven with mouse and keyboard; the same windowed programs headless in Node with screenshots | `node tests/web/browser.mjs`, `tests/web/picker.mjs`, `tests/web/gui.mjs`, `tests/wine/gui.mjs` |
| 5. Own output | Snapshots of IR and WAT for committed binaries | `cargo test -p wwt --test snapshots` |

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

## Repository layout

```
crates/wwt          translator library
crates/wwt-cli      `wwt` command-line tool
crates/wwt-wasm     translator compiled to WebAssembly (fast mode, browser)
crates/wwt-testkit  instruction generator, oracle driver, wasmtime runner
runtime/            JavaScript runtime (Node and browser hosts)
native/wine-unix    Wine's Unix side (wineserver, win32u, display driver) for Emscripten
tools/oracle        native x86 oracle for instruction tests
tools/wine          builds Wine's i386 PE DLLs, programs, tests and fonts
tools/wine-layout   generates Wine's structure layouts for the runtime
tools/torture       fetches GCC's torture tests
tools/bench         CoreMark: native vs. translated (shims and Wine)
tools/site          assembles the static site and deploys it to Vercel
tests/              fixtures, test programs, Csmith runtime, browser test
spikes/             M1 spikes: memory size, Emscripten above the guest limit
docs/               the plan (plan.md) and each milestone's status
```
