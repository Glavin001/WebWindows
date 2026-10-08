# Comparison lanes

Where translated Wine stands against other ways to run the same programs,
and which parts of the comparison checklist (October 2026) are covered.
Speeds are reference time ÷ the lane's time: the reference is the Linux
build run natively (`gcc -m32 -O2`), or Wine running the `.exe` natively for
the Windows API workloads. Every lane must print the reference's checksums.

## Results

`tools/bench/suite.mjs`, median of 3, one shared 4-vCPU Xeon container,
Node 22 (V8 12.4), runs interleaved by round. Seconds:

| Benchmark | Native | Emscripten | Wine (native) | qemu-i386 | Wine-Assembly | Ours, in-browser translator | Ours, ahead of time |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CoreMark | 1.38 | 1.51 | 1.43 | 5.00 | 82.4 | 2.31 | 2.49 |
| SQLite speedtest1 | 5.16 | 5.31 | 47.1 | 32.6 | — | 12.0 | 11.8 |
| Lua fib | 0.046 | 0.080 | 0.045 | 0.407 | — | 0.245 | 0.269 |
| Lua tables | 0.394 | 0.741 | 0.382 | 3.70 | — | 1.09 | 1.43 |
| Lua strings | 0.227 | 0.305 | 0.283 | 2.39 | — | 0.848 | 0.934 |
| Lua sort | 0.626 | 0.595 | 0.593 | 5.19 | — | 1.69 | 1.63 |
| Lua objects | 0.902 | 1.375 | 0.919 | 11.0 | — | 3.56 | 3.64 |
| Lua float | 0.460 | 0.934 | 0.387 | 11.3 | — | 1.58 | 1.58 |
| apibench (7, vs Wine) | | | 1.0 | | | | geomean 120% |

(The Lua rows for qemu-i386 come from a second run of native, qemu and
ahead-of-time; machine noise between runs is 5–10%.)

Relative to native: Emscripten 70–92%, ours 56% (CoreMark), 44% (SQLite),
17–38% (Lua); qemu-i386 user mode 28% (CoreMark), 16% (SQLite), 9% (Lua);
Wine-Assembly 1.7% (CoreMark). So against the other software translator
running natively, translated WebAssembly is 2–3× faster, and against the
WebAssembly interpreter about 35×. The in-browser translator (the
WebAssembly build of `wwt`, `WWT_TRANSLATOR=wasm`) produces code as fast as
the ahead-of-time one.

## First launch

`tools/bench/firstlaunch.mjs` (seconds; output size as a multiple of the
x86 code):

| Image | x86 code | Output | Translate (native) | Translate (in wasm) | Compile: baseline | Compile: optimized |
| --- | --- | --- | --- | --- | --- | --- |
| CoreMark | 37 KB | 8.6× | 0.15 | 0.28 | 0.01 | 0.06 |
| Lua | 232 KB | 12.7× | 1.27 | 1.99 | 0.04 | 0.45 |
| SQLite | 883 KB | 9.3× | 4.22 | 6.34 | 0.14 | 2.05 |
| apibench | 58 KB | 15.4× | 0.45 | 0.64 | 0.01 | 0.18 |
| ntdll | 452 KB | 8.1× | 1.59 | 2.98 | 0.06 | 0.58 |
| kernelbase | 536 KB | 6.5× | 1.42 | 2.25 | 0.04 | 0.46 |
| msvcrt | 468 KB | 8.9× | 4.22 | 6.56 | 0.09 | 1.40 |
| user32 | 600 KB | 7.9× | 1.99 | 3.26 | 0.06 | 0.76 |

Translation runs at 0.1–0.4 MB of x86 code per second, so a program's
first launch is dominated by translating it, not by compiling the result
(V8's baseline tier compiles everything in a tenth of a second).

## Diagnostics

* **Ablations** (one optimization off at a time) and **progress over the
  commits**: `docs/performance.md`, and the speed log page built from them.
* **Residue counters**: every translated module has a `wwt.residue`
  section with, per function, the x86 registers written back to and loaded
  from the CPU state, memory checks, store-map lookups and calls through the
  address lookup. `tools/bench/wasm-map.mjs` lists them next to each
  function's CPU time; in Lua the interpreter loop still does 855 state
  loads and 915 state stores around its 84 call sites.
* **Ours vs Emscripten per function**: `tools/bench/wasm-map.mjs` (code
  size and CPU time per matched function, as treemaps and a table).

## Lanes

| Lane | Status | How |
| --- | --- | --- |
| Native (Linux build) | in the suite | `native` |
| Native `.exe` under Wine | in the suite | `wine` (Ubuntu's Wine 9) |
| Emscripten (the ceiling) | in the suite | `emcc` |
| qemu user mode | in the suite | `qemu` (`qemu-i386` on the Linux build) |
| Wine-Assembly | in the suite, CoreMark only | `wine-assembly`, opt-in (below) |
| Ours, ahead of time / in-browser translator | in the suite | `wwt-wine` / `wwt-fast` |
| BottleShip, v86, Theseus | next | need headless Chromium and each project's build |
| QEMU's WebAssembly port, container2wasm, Boxedwine, CheerpX | not started | optional lanes |
| Rosetta 2, FEX/Box64, native Windows | not here | need an Apple silicon Mac, an ARM Linux machine, a Windows PC |
| Chrome | partly | `tests/web` runs translated programs in headless Chromium; no timed lane yet |
| Firefox, Safari, ARM, a mid-range laptop | not here | other engines and machines |
| 64-bit lanes | not applicable yet | the translator is 32-bit |
| Graphics stages | later | with the Direct3D work |

**Wine-Assembly** needs a checkout with
`tools/bench/wine-assembly-console.patch` applied (it prints the emulated
console's text at exit, since console programs' output goes to that window,
not to stdout), then `WINE_ASSEMBLY=/path/to/checkout node
tools/bench/suite.mjs`. The program is built for it with msvcrt's `printf`
(`-D__USE_MINGW_ANSI_STDIO=0 -fno-builtin-printf`); its clock runs in real
time (`--real-ticks`). SQLite, Lua and apibench do not run on it yet:
its C runtime has no `localeconv`, `puts` or `GetFileAttributesExW`, its
`printf` prints nothing for `%f`, `%g`, `%I64d` and `%.*s`, and `fwrite`
to stdout does not reach the console. Lua does run to completion (about
86 s at scale 1, against about 2 s for ours) but cannot print its
checksums there.

## Fairness rules

* Same machine, programs and checksums: a lane whose checksum differs is
  not counted (the suite's geometric means skip it).
* One WebAssembly engine (Node's V8) for every WebAssembly lane.
* Startup: every workload times itself inside the program, so start-up is
  not counted. Wine-Assembly's clock is put in real time for the same.
* Runs are interleaved: each round runs every lane once.
* `--pin CPU` runs each measurement under `taskset`; the JSON records the
  CPU, kernel, governor and every engine's version. The container has no
  frequency governor to set.
* Geometric means per lane, with per-benchmark tables.
