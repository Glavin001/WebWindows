# Milestone 1 — translator core and test harness

Status as of October 7, 2026.

## Done-when criteria

| Criterion (from the plan) | Status | Evidence |
| --- | --- | --- |
| A tiny hand-built .exe runs correctly in Node | Done | `node runtime/node/run.mjs tests/programs/hello.exe` (no C runtime, kernel32 only). MinGW programs with the full C runtime also run. |
| The instruction suite passes for everything MinGW emits | Done | Every instruction form found in 170 MinGW-built binaries (hand-written programs and Csmith, -O0…-Os; 277 distinct forms) has recorded cases, and all cases pass (`wwt-testkit --bin coverage`). |
| Test layers 1 and 5, CI recording results on x86 runners | Done | `cargo test -p wwt-testkit`, `cargo test -p wwt --test snapshots`; CI re-records the fixtures on its x86 runner and fails if they differ. |
| Spike: Emscripten keeps data, heap and stacks above the guest limit | Done | `spikes/emscripten/build.sh` passes at 1 GB and 2 GB limits (see `spikes/README.md`). |

## Test results

**Layer 1 — instructions**, recorded on a real x86 CPU by `tools/oracle` and
replayed through the translator under wasmtime, both in optimized and fast
mode:

| Group | Instruction forms | Cases | Result |
| --- | --- | --- | --- |
| Integer (every legacy form valid in 32-bit user mode, excluding I/O, segments, far branches, BCD) | 472 | 11,308 | all pass |
| Flag fusion (producer + `jcc`/`setcc`/`cmovcc`/`adc`/… pairs) | 3,691 pairs | 4,000 | all pass |
| x87 | 134 | 3,200 | all pass |

Undefined flags are masked per instruction (iced-x86's tables plus
count-dependent rules for shifts and rotates). x87 cases run with double
precision control, since registers are kept as f64 (the plan's default);
transcendental results are compared within 1e-14 relative error.

**Layer 2 — programs** (`tests/programs/check.mjs`): each C program is built
natively with `gcc -m32` as the reference and with MinGW at -O0, -O1, -O2,
-O3 and -Os; the translated program's stdout and exit code must match.

* 7 hand-written programs covering jump tables, function pointers, 64-bit
  arithmetic, x87, strings, heap, `qsort` callbacks, varargs and
  `setjmp`/`longjmp`: 35/35 runs pass.
* Csmith: 1,000 runs over 200 random programs: 889 pass, 110 skipped (the
  native build itself does not finish in 10 s), 1 failure, which found a real
  bug (a conditional jump to the next instruction; fixed, now a regression
  test in `crates/wwt/tests/regressions.rs`). A second batch of 200 programs
  runs after the fix.

**Layer 5 — snapshots**: IR, WAT and translation reports of binaries
committed in `tests/fixtures/binaries`.

**Browser**: `tests/web/browser.mjs` runs programs in headless Chromium
through the web front end: the first launch translates in the browser,
code missed ahead of time is translated at run time and saved to the
profile, the next launch re-translates with the profile, and the launch
after that loads the cached module without translating.

## Measurements

On the development machine (x86-64, Node 22):

| | |
| --- | --- |
| Ahead-of-time translation, native CLI (release) | ~0.1 s for a 7,900-instruction program (MinGW CRT + benchmark) |
| Same, translator compiled to WebAssembly in Chromium | ~250 ms |
| Fast mode, one missed function | 1–20 ms |
| `tests/programs/bench` (sieve, CRC-32, matrix multiply, quicksort) | native 0.33 s; translated 0.79 s with memory and code-write checks, 0.55 s without |

Speed work is M7; these numbers are a baseline, not a target. The checks
cost about 30% here, mostly code-write checks on stores through pointers.
The benchmark is built with `i686-w64-mingw32-gcc -O2 bench.c -o bench.exe`
(translated) and `gcc -m32 -O2 bench.c -o bench.native` (native).

## Design decisions made during M1

* **Function ABI.** Every translated function has type `(cpu) -> i32`,
  returning the x86 address to continue at. After a call, the caller
  continues inline only if the returned address is the expected return
  address; otherwise it returns too, so `push`/`ret` tricks, `longjmp` and
  exception unwinding stay correct. Jumps between functions use
  `return_call`.
* **Structured control flow.** The plan suggested Binaryen's Relooper; the
  translator instead builds structure from the dominator tree (Ramsey,
  *Beyond Relooper*, 2022), which needs no C++ dependency and also runs in
  the browser build. Irreducible functions fall back to a dispatch loop.
  Binaryen's `wasm-opt` remains an optional ahead-of-time pass
  (`wwt translate --wasm-opt`).
* **Flags.** The flag state is five state registers (kind, result,
  operands). A forward analysis tracks the kind statically, so a flag read
  becomes a direct comparison (`cmp`+`jl` is one `i32.lt_s`); unknown kinds
  call a generated helper. Liveness then deletes flag work nobody reads; the
  lazy state is written back only where it may escape.
* **Registers** live in WebAssembly locals within a function; a dirty-state
  analysis writes back only what changed, at calls, exits and fault points.
* **Address lookup.** Indirect calls and returns that miss inline use a
  two-level table in the native region (4 MB first level, 16 KB per code
  page). Table slot 0 is the kernel's miss handler, so a lookup never
  branches.
* **Memory checks.** Accesses through `esp`/`ebp` and constant addresses in
  data sections are trusted; others get one unsigned compare against the
  guest region. Stores that might hit translated code check a page bitmap.
* **Faults** write back the general registers precisely; the lazy flag
  state at a fault is not guaranteed (documented, as the plan allows).
* **Win32 layer for M1.** Until translated Wine arrives in M2, a small set
  of kernel32 and msvcrt functions are implemented in JavaScript
  (`runtime/win32.mjs`), enough for MinGW's C runtime.
* **Oracle.** Instruction ground truth comes from a 32-bit Linux process on
  an x86 CPU (identical user-mode semantics); the CI Windows job runs the
  same MinGW executables natively and checks their output against the
  translated runs.

## Ahead of the plan

* Layer 2 (programs, Csmith) and a cvise reduction script (planned for M2).
* Fast mode and the profile loop, with the translator compiled to
  WebAssembly (planned for M3).
* In-browser translation, caching in the origin private file system and a
  folder-picker page (the M3 criterion, except that the browser test loads
  the program by URL; the picker is exercised manually).

## Known limitations and next steps

* ~~SSE/SSE2 and MMX are not lifted yet.~~ Done early in M2: 286 forms
  on WebAssembly SIMD, all passing (see [milestone-2.md](milestone-2.md)).
* Not supported: BCD instructions (`aaa` family), segment-register loads
  other than recording the selector, far calls and jumps, 16-bit addressing,
  `enter` with a nesting level. They raise an "unsupported" fault.
* Windows exceptions (SEH) are not dispatched yet; a fault stops the program
  with a description (M5).
* Self-modifying code: a store into a translated page invalidates that
  page's entries for future calls; a function that patches code it is about
  to execute in the same activation keeps running the old translation.
* Atomic instructions on misaligned addresses fall back to non-atomic
  accesses. The plan's per-game strict ordering mode exists
  (`--strict-ordering`) but is untested with threads, which arrive in M5.
* `fild`/`fistp` of 64-bit integers above 2^53 lose bits (the plan's raw
  integer tagging is not implemented); precision control is ignored.
* A `longjmp` or exception unwinds every WebAssembly frame back to the
  dispatcher, so return sites on the guest stack are entered through the
  lookup (and fast mode the first time). Correct, but slower than resuming
  in place.
* The lookup table costs 16 KB of native memory per 4 KB code page; with
  all of Wine's DLLs loaded this needs a sparser structure (M2/M3).
* Deep guest recursion is bounded by the WebAssembly stack.
