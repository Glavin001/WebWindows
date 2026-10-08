# Benchmark results, 8 October 2026

A snapshot of where translated Wine stood on this date, against native
code, Emscripten and the other ways to run x86 code, with the version of
every lane. The JSON files hold every measurement; this page summarizes
them. How the lanes work and how to rerun them: [../../comparison.md](../../comparison.md).

## Machine and versions

| | |
| --- | --- |
| Machine | Intel Xeon @ 2.80 GHz, 4 vCPU, a shared cloud container (runs vary by 5–10%), Linux 6.18.44, no frequency governor |
| WebWindows | translator and runtime at `dc901af` (lanes scripts at `96b3c14`) |
| Node | v22.22.0 (V8 12.4.254.21) for every WebAssembly lane |
| Native and Linux builds | gcc 13.3.0 (Ubuntu 24.04), `-m32 -O2` |
| Windows builds | i686-w64-mingw32-gcc 13-win32, `-O2` |
| Emscripten | 6.0.11 (`a001454`), `-O2` |
| Wine (native) | wine-9.0 (Ubuntu 9.0~repack-4build3) |
| Translated Wine | Wine 11.0 DLLs, built by `tools/wine/build.sh` |
| qemu | qemu-i386 8.2.2 (Ubuntu 1:8.2.2+ds-0ubuntu1.18), user mode |
| Wine-Assembly | github.com/vgrichina/wine-assembly `d1e40e9` (2026-10-08), with `tools/bench/wine-assembly-console.patch` |
| Theseus | github.com/evmar/theseus `ec76333` (2026-10-06); rustc 1.99.0; wasm32 with rustc 1.101.0-nightly (2026-10-07) and wasm-bindgen 0.2.121 |
| v86 | npm `v86` 0.5.470 (`6db8b15`); SeaBIOS and VGA BIOS from github.com/copy/v86 `6db8b15` |

## Files

| File | What | Produced by |
| --- | --- | --- |
| `suite.json` | SQLite, Lua, CoreMark, apibench on native, Emscripten, Wine, qemu, Wine-Assembly and ours (ahead of time and in-browser); machine record | `WINE_ASSEMBLY=… node tools/bench/suite.mjs --json suite.json` |
| `suite-reference.json` | The reference times the speed log divides by (native, Emscripten, Wine, ours) | `node tools/bench/suite.mjs` |
| `coremark-lanes.json` | CoreMark on Theseus, v86 and every other lane, all rounds | `tools/bench/lanes/coremark-lanes.sh` |
| `firstlaunch.json` | Translation time, output size and compile time per image | `node tools/bench/firstlaunch.mjs --json` |
| `history.json` | The suite at 20 commits from this day's work, each rebuilt from its commit | `tools/bench/history/run.mjs` (below) |
| `ablations.json` | The current build with one optimization off at a time | `ABL=1 tools/bench/history/run.mjs` |

## The suite

Seconds, median of 3 (`suite.json`; the qemu-i386 Lua rows from a second
run of native, qemu and ours, since the first passed the script by its
Windows path):

| Benchmark | Native | Emscripten | Wine | qemu-i386 | Wine-Assembly | Ours, in-browser | Ours |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CoreMark | 1.383 | 1.514 | 1.425 | 4.996 | 82.44 | 2.308 | 2.487 |
| SQLite speedtest1 | 5.161 | 5.306 | 47.06 | 32.57 | — | 12.01 | 11.83 |
| Lua fib | 0.053 | 0.080 | 0.045 | 0.407 | — | 0.245 | 0.248 |
| Lua tables | 0.365 | 0.741 | 0.382 | 3.699 | — | 1.089 | 1.066 |
| Lua strings | 0.233 | 0.305 | 0.283 | 2.387 | — | 0.848 | 0.799 |
| Lua sort | 0.552 | 0.595 | 0.593 | 5.192 | — | 1.686 | 1.584 |
| Lua objects | 0.876 | 1.375 | 0.919 | 10.98 | — | 3.555 | 3.498 |
| Lua float | 0.398 | 0.934 | 0.387 | 11.26 | — | 1.584 | 1.664 |
| apibench heap | | | 0.080 | | | 0.033 | 0.035 |
| apibench malloc | | | 0.092 | | | 0.041 | 0.043 |
| apibench files | | | 1.421 | | | 0.136 | 0.133 |
| apibench seek | | | 0.206 | | | 0.225 | 0.305 |
| apibench strings | | | 0.902 | | | 1.269 | 1.354 |
| apibench sync | | | 0.071 | | | 0.117 | 0.124 |
| apibench qsort | | | 0.277 | | | 1.009 | 1.011 |

Geometric means against native (portable workloads) or native Wine
(apibench): Emscripten 70%, native Wine 74%, ours 59% (in-browser
translator 61%), qemu-i386 21% (CoreMark and SQLite only in that run),
Wine-Assembly 2% (CoreMark). The registry benchmark is not counted: the
runtime has no registry yet.

## Theseus and v86

CoreMark with no C runtime, built for each lane from one source
(`coremark-lanes.json`), median of 3 (Wine-Assembly 1 run):

| Lane | Seconds | vs native |
| --- | --- | --- |
| Native (Linux) | 1.341 | 100% |
| `.exe` on native Wine | 1.330 | 101% |
| Ours, ahead of time | 2.299 | 58% |
| Ours, in-browser translator | 2.285 | 59% |
| qemu-i386 | 4.689 | 29% |
| Theseus, native | 13.339 | 10% |
| v86 | 14.075 | 9.5% |
| Theseus, WebAssembly | 31.891 | 4.2% |
| Wine-Assembly | 82.065 | 1.6% |

## First launch

`firstlaunch.json`: x86 code, output size, translation time with the
native translator and the one compiled to WebAssembly, and V8 compile time
for the whole module (baseline tier, optimizing tier):

| Image | x86 code | Output | Translate | In wasm | Baseline | Optimized |
| --- | --- | --- | --- | --- | --- | --- |
| CoreMark | 37 KB | 8.6× | 0.15 s | 0.28 s | 0.01 s | 0.06 s |
| Lua | 232 KB | 12.7× | 1.27 s | 1.99 s | 0.04 s | 0.45 s |
| SQLite | 883 KB | 9.3× | 4.22 s | 6.34 s | 0.14 s | 2.05 s |
| apibench | 58 KB | 15.4× | 0.45 s | 0.64 s | 0.01 s | 0.18 s |
| ntdll | 452 KB | 8.1× | 1.59 s | 2.98 s | 0.06 s | 0.58 s |
| kernelbase | 536 KB | 6.5× | 1.42 s | 2.25 s | 0.04 s | 0.46 s |
| msvcrt | 468 KB | 8.9× | 4.22 s | 6.56 s | 0.09 s | 1.40 s |
| user32 | 600 KB | 7.9× | 1.99 s | 3.26 s | 0.06 s | 0.76 s |

## The day's progress

`history.json` reruns the suite on ours at 20 commits, from before the
performance work (`3c51121`) to `dc901af`, each with its own translator and
runtime. The 13 benchmarks the first commit ran correctly (its runtime
could not run SQLite and got the file benchmark's checksum wrong) went
from 15% to 47% of native speed; the full suite, from 34% at the first
commit that runs SQLite to 56%. Translated code for the four programs and
Wine's DLLs shrank from 69 MB to 49 MB; cold start (translating everything)
went from 15 s to 37 s, warm start stayed at 0.6–0.7 s.

`ablations.json`, the slowdown with one optimization switched off
(geometric mean over the suite): loop re-entry +27% (Lua +34%), native
heap +19% (apibench +43%), native strings +15% (apibench +27%), C call ABI
for Wine's DLLs +10%, inlining +10%, the store-map lookup on every store
+7%, thunk aliasing +6%, atomic read-modify-write +4%. No memory checks
(unsafe) would be 6% faster (CoreMark 16%, Lua 13%, SQLite 9%).

The checkpoint runner rebuilt each commit in its own worktree and ran the
suite's workloads through that commit's `runtime/node/wine.mjs`; the pages
built from these results (the speed log is `tools/bench/history/page.mjs`'s
output, the map `tools/bench/wasm-map.mjs`'s):
[speed log](https://claude.ai/artifact/GevUfYZ5rsSb5tT7pYzVs4),
[size and heat map](https://claude.ai/artifact/S8VyBpSksfWmaq5cmcN1Px)
(private links; share them from the page).

## Reproducing

On a machine with the toolchains from [../../performance.md](../../performance.md#0-prerequisites)
and Wine built (`tools/wine/build.sh`), at the commit these were taken from:

```sh
# The suite, with the opt-in Wine-Assembly lane (a checkout with
# tools/bench/wine-assembly-console.patch applied):
WINE_ASSEMBLY=/path/to/wine-assembly node tools/bench/suite.mjs --json suite.json
node tools/bench/firstlaunch.mjs --json firstlaunch.json

# Theseus, v86 and the rest on the C-runtime-free CoreMark; prints the
# versions it ran with, then each lane's seconds per round:
THESEUS=/path/to/theseus V86_DIR=/path/to/npm-dir V86_BIOS=/path/to/v86/bios \
  WINE_ASSEMBLY=/path/to/wine-assembly tools/bench/lanes/coremark-lanes.sh 3

# The progress over the day: each commit in tools/bench/history/checkpoints.txt
# built in its own worktree under target/history, then measured, then the
# ablations on the last one, then the speed log page.
tools/bench/history/build.sh
SPLIT_API=1 node tools/bench/history/run.mjs
ABL=1 OUT=abl.json node tools/bench/history/run.mjs
tools/bench/history/profile.sh
node tools/bench/history/page.mjs suite.json -o speed-log.html
```

Runs on a shared container differ by 5–10%; compare lanes within one run,
not across these files and a new run.
