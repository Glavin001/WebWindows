# Performance: Emscripten vs. translated WebAssembly

CoreMark goes into WebAssembly two ways:

- **Emscripten**: C source → WebAssembly.
- **WebWindows**: C source → MinGW `.exe` → `wwt` → WebAssembly.

Two questions:

1. **Same behavior.** Every build prints the same CoreMark checksums.
2. **Close performance.** Emscripten shows what WebAssembly itself can do
   on this machine. The gap between Emscripten and `wwt` is the translator's
   overhead, and that is what we work to shrink.

The tools live in `tools/bench/`:

| Script | What it does |
| --- | --- |
| `coremark.mjs` | Builds every tier, checks the checksums agree, benchmarks, prints the table (`--variants` attributes the cost of the memory checks) |
| `profile.mjs` | Self time per function for the Emscripten and translated builds side by side (or any Node command) |
| `inspect.mjs` | One function from both builds: instruction counts by kind, plus the x86, IR and both WebAssembly listings |
| `ab.mjs` | A/B test of translator variants: interleaved rounds, medians, ratio to the first variant |

## Results

Development container (4 vCPU Xeon, Node 22), CoreMark 1.0, `-O2`. Runs on
this shared machine vary by 5–10%, native included; ratios are taken
within one `coremark.mjs` run.

| Build | Iterations/s | vs. native | vs. Emscripten |
| --- | --- | --- | --- |
| Native, `gcc -m32 -O2` | ~21,600–22,600 | 100% | — |
| Native, `clang -m32 -O2` (LLVM 18) | ~18,700 | ~90% | — |
| Emscripten `-O2`, Node (CRC rewrite off, see below) | ~20,500–21,400 | ~95% | 100% |
| `wwt`, M1 shims, Node: before this work | ~7,700 | ~35% | ~37% |
| … with irreducible loops made reducible | ~8,300–9,000 | ~40% | ~42% |
| … and the store map with inline invalidation | ~9,400–10,000 | ~45% | ~47% |
| … and fewer load checks, narrower operations, constant guest limit | ~10,500 | ~49% | ~51% |
| … and inlining, constant table addresses | **~13,300** | **~59%** | **~62%** |
| Same, headless Chromium (translated in the page) | ~13,600–13,900 | ~61% | ~64% |

Translated Wine ran compute-bound code at the same speed as the M1 shims
in M2 (its DLLs are only on the path for system calls; see
[milestone-2.md](milestone-2.md#speed-coremark)). It was not re-measured
here: Wine's DLLs were not built on this container. `coremark.mjs` runs the
Wine tier when they are (`tools/wine/build.sh`).

### What changed and why

* **Irreducible control flow** (`crates/wwt/src/reducible.rs`). GCC's jump
  threading turns CoreMark's state machine (`core_state_transition`) and
  `core_list_mergesort` into loops with several entry blocks. Code
  generation used to fall back to a dispatch loop over *every* block of
  such a function: one `br_table` per branch, and no values kept in
  registers across blocks. Now each multi-entry loop gets a small header
  that dispatches to the entry that was meant, and the rest of the
  function is structured as usual (LLVM's WebAssemblyFixIrreducibleControlFlow
  scheme).
* **Store checks** (`wwt::abi::store_map`, ABI 3). Each guest store used to do a
  bounds check and then a code-page check from a bitmap that called the
  host on a hit. Calls on cold paths are expensive even when not taken: V8
  doesn't know the branch is unlikely, so values live across it get
  spilled. Removing just that call was worth more than all the fault-path
  register write-back. Stores now look up one byte per page: zero means a
  plain store. The null region, the last guest page and everything above
  the guest limit go down a slow path that does the precise check. So do
  pages with translated code, where the translated code invalidates the
  page's lookup entry itself, with no call to the host.
* **Fewer load checks.** A load within 4 KB of an address that already
  passed a check, from the same unchanged base, skips its check. The only
  difference is that such a load, if it lands within 4 KB of the null
  region or the guest limit, reads memory instead of faulting.
* **Narrower operations** (`opt::narrow`). `movsx` from memory becomes one
  signed load, and `imul` (lifted as a 64-bit product for its flags)
  becomes a 32-bit multiply once the flags are dead.
* **Guest limit as a constant.** Hosts that know the guest limit when they
  translate pass it (`wwt translate --guest-limit-mb`, or bits 16–31 of the
  in-browser translator's flags). Memory checks then compare against an
  immediate instead of a global, which frees a register in V8's code
  (about 4%). The module records the limit, and the runtime refuses it
  under another one. Without the option, modules read the limit at run
  time as before. Such modules also address the runtime's tables (address
  lookup, store map) at constant offsets from the limit
  (`wwt::abi::native_layout`), instead of reloading their bases from the
  instance inside loops.
* **Inlining** (`crates/wwt/src/inline.rs`). A translated call costs far
  more than an x86 one. The caller writes back its dirty registers, the
  callee loads what it reads and writes back what it changed, and the
  caller reloads everything live. perf showed 39% of
  `core_state_transition`'s time in that entry and exit code. Small leaf
  functions (no calls, tail jumps or indirect jumps) are now spliced into
  their callers, where the x86 state stays in locals. The return address
  is still pushed, and the callee's `ret` continues inline only when it
  pops the expected address, so x86 semantics and fault addresses are
  unchanged. +8% on CoreMark. `wwt translate --no-inline` turns it off.

### What didn't help (measured with `ab.mjs`)

* Running Binaryen's `wasm-opt` over the output (`wwt translate
  --wasm-opt`): V8 already does the local cleanups it would.
* Treating flags, or `ecx`/`edx`, as dead at `ret` and at calls: no
  measurable change, and it would break hand-written assembly that passes
  values that way.
* Dropping the register write-back on fault paths entirely: no measurable
  change once the code-write call was gone. Fault paths end in
  `unreachable`, and V8 keeps them out of the way.
* Making the store map's address a constant as well: under 1% on its own.
* Inlining callees that make calls themselves: −3% (bigger functions,
  more register pressure in V8).
* An available-checks dataflow across blocks, in place of the per-block
  and dominator-tree facts: it removes about a fifth of the checks in
  `core_bench_state`, but none on hot paths (−1%, noise). The checks that
  remain hot are on pointers that change every iteration (list traversal,
  byte scanning), which no static fact covers.

### Where the rest of the gap is

With every check off, CoreMark would run at ~76% of Emscripten. The checks
themselves cost ~19% (`ab.mjs`: ~12% guest-limit checks on loads, ~2%
store map, the rest their interaction). The other ~24% comes from x86
semantics carried into WebAssembly. `push`/`pop` and arguments go through
guest memory even after inlining. The sort's comparator calls go through
the address lookup and `call_indirect`. Partial-register writes and flags
add work.

### Emscripten's CRC rewrite

Recent LLVM recognizes CoreMark's bit-at-a-time CRC loops and replaces them
with a 256-entry table lookup. GCC doesn't, and neither did LLVM 18. That's
an algorithmic change, not WebAssembly being faster: Emscripten's `crcu32`
has no loop at all. It's worth 7–14% to Emscripten on CoreMark (and is why
Emscripten could beat native GCC). `coremark.mjs` turns it off
(`-mllvm --loop-idiom-crc-strategy=disable`) so the WebAssembly ceiling
runs the same algorithm as the `.exe`; `--emcc-crc` turns it back on.

## 0. Prerequisites

```sh
# Native 32-bit and MinGW compilers (Debian/Ubuntu)
apt-get install -y gcc-multilib gcc-mingw-w64-i686

# Emscripten (coremark.mjs finds it on PATH, in $EMSDK or in ~/emsdk)
git clone --depth 1 https://github.com/emscripten-core/emsdk.git ~/emsdk
~/emsdk/emsdk install latest && ~/emsdk/emsdk activate latest

# Translator, plus the fast-mode translator compiled to WebAssembly
cargo build --release -p wwt-cli
cargo build -p wwt-wasm --target wasm32-unknown-unknown --profile release-wasm
```

Tiers whose tools are missing are skipped with a note. The Wine tier needs
Wine's PE DLLs (`tools/wine/build.sh`).

## 1. Check, benchmark, compare

```sh
node tools/bench/coremark.mjs                 # check checksums, then benchmark every tier
node tools/bench/coremark.mjs --runs 3        # median (and best) of three runs per tier
node tools/bench/coremark.mjs --variants      # + wwt without code-write / memory checks
node tools/bench/coremark.mjs --check         # checksums only (a few seconds)
node tools/bench/coremark.mjs --json out.json # keep the numbers
node tools/bench/coremark.mjs --translate "--no-smc-checks"   # try translator options
```

(`tools/bench/coremark.sh` is the same script.)

The checksum check builds every tier with a fixed iteration count (2000),
so all five values, `crcfinal` included, must match exactly. The
performance builds use `ITERATIONS=0`: CoreMark calibrates itself to run
for at least 10 seconds, and reports "Correct operation validated" only
when its own checksums are right. The translated module is produced ahead
of time, so translation time is never part of a measurement.

Report two ratios:

- **wwt ÷ Emscripten**: the translator's overhead, which is our target.
- **Emscripten ÷ native**: the cost of WebAssembly and V8, which isn't ours
  to fix.

Only portable C can be built with Emscripten, so this comparison is for
programs like CoreMark, not for programs that call Windows APIs.
`long double` differs between the builds: native x87 uses 80 bits, `wwt`
uses f64, Emscripten uses 128-bit software floating point.

## 2. Profile both builds by function

```sh
node tools/bench/profile.mjs                  # Emscripten, then wwt
node tools/bench/profile.mjs --tier wwt --top 40
node tools/bench/profile.mjs --translate "--no-mem-checks --no-smc-checks"
node tools/bench/profile.mjs -- node runtime/node/run.mjs tests/programs/bench/bench.exe

# Machine-code level, with Linux perf and V8's jitdump:
node tools/bench/profile.mjs --perf --tier wwt
node tools/bench/profile.mjs --tier wwt --annotate matrix_mul_matrix@
```

`--annotate` lists the hottest x86-64 instructions V8 generated for the
matching functions. Look for spills (`mov %r11,-0x58(%rbp)` inside a loop),
reloads of values that should stay in registers, and checks
(`lea -0x10000(%reg)`; `cmp $limit`). `perf` comes from `linux-tools`
(`$PERF` points at it if it's not on `PATH`). In a VM without hardware
counters it samples `cpu-clock`, which is enough.

Translated modules carry a WebAssembly name section, so profiles (and the
browser's DevTools) show translated functions as `symbol@address` when the
`.exe` still has its COFF symbol table, which MinGW keeps unless stripped,
or as `x86_address` otherwise. Emscripten builds are made with
`--profiling-funcs`, which keeps names and costs nothing at run time.

Reference split, as of this writing:

| Emscripten | | wwt | |
| --- | --- | --- | --- |
| `core_bench_list` (with `cmp_idx`, `cmp_complex`, list functions inlined) | ~45% | `core_bench_list` | ~24% |
| `calc_func` (with matrix and state work inlined) | ~37% | `core_state_transition` | ~22% |
| `core_state_transition` | ~15% | `core_bench_state` | ~13% |
| crc functions | ~2% | matrix functions | ~17% |
| | | crc functions | ~11% |

GCC and clang inline differently, so compare groups of functions, not
single entries. Multiply each share by the run time per iteration to compare
absolute time: at 21,000 it/s an iteration takes 48 µs, at 9,500 it/s
105 µs.

## 3. A/B test an idea

```sh
node tools/bench/ab.mjs base= nosmc=--no-smc-checks
node tools/bench/ab.mjs --rounds 9 head=@old.wasm now=
node tools/bench/ab.mjs base= "try=MY_EXPERIMENT=1"     # env vars reach the translator
```

Each variant is translated once. Then every variant runs a fixed-iteration
CoreMark in turn, round after round, so drift on a shared machine affects
them all equally. The script reports medians and the change against the
first variant. On the development container, single runs vary by 5–10%.
With 7–9 rounds, a 3% difference is visible. A variant whose CRCs come out
wrong is reported as failed.

## 4. Compare the code of one hot function

```sh
node tools/bench/inspect.mjs core_state_transition crcu32
```

prints, for each matching function in both modules, the number of
instructions by kind (local traffic, constants, loads, stores, branches,
calls, guest-limit and store-map checks), with fault paths counted apart,
because they only run when the program faults. It writes the x86, our IR,
our WebAssembly and Emscripten's WebAssembly to `target/bench/inspect/`.
`wwt wat` prints any `.wasm` file too (`wwt wat target/bench/coremark.emcc.wasm`).

### Checklist for each hot function

| Look for | Usual cause on our side |
| --- | --- |
| `loop` + `br_table` over every block | Fallback for irreducible control flow; should no longer happen (`reducible.rs`) |
| Loads and stores at `esp+N` that Emscripten keeps in locals | x86 register spills and pushes. Fix: promote stack slots to locals |
| Stores of registers to the CPU struct before calls, loads after | Register write-back at calls. Fix: direct calls with a known ABI |
| `(x & 0xff) & 0xff`, `(x & 0xffff0000) \| …` | 8- and 16-bit register writes. V8 folds most of these (`--wasm-opt` doesn't help) |
| Stack spills in V8's code for a loop (see `--annotate`) | Register pressure: x86 registers, the CPU pointer, temporaries |
| `i32.sub 65536; global.get 2` (or `i32.const`); `i32.gt_u; if … fault` | Guest-limit check. Fix: an available-checks dataflow across blocks, hoisting out of loops |
| `global.get 3; i32.add; i32.load8_u; if` | Store-map check |
| A `call` inside a rarely taken `if` | Forces spills around it; keep calls off hot paths (fault paths end in `unreachable`, which V8 treats as cold) |
| `call_indirect` through the two-level lookup | Indirect calls and jumps. Fix: cache them or inline the lookup |
| Leftover flag computations | Flag lowering didn't fuse them |

Count instructions, memory operations and calls in both listings. Most of
the gap comes from a few rows of this checklist.

## Next steps

In rough order of expected gain:

1. **Stack slots as locals.** GCC's x86 code keeps locals and spilled
   registers at `esp+N`. When a function's frame doesn't escape, those
   slots can live in WebAssembly locals, as they do in Emscripten's code.
2. **More inlining, and cheaper calls where it can't happen.** Callees
   that make calls of their own can be inlined too: their calls stay calls.
   Indirect calls through function pointers (CoreMark's sort comparators)
   could be devirtualized from a run-time profile. Calls that remain would
   benefit from per-function summaries of the registers read and written,
   so a call writes back and reloads only what the callee uses.
3. **Load checks in loops.** Range checks hoisted out of counted loops
   (a pointer advancing by a known stride), which is where the remaining
   hot checks are. Cross-block facts alone didn't help (see above).

## Goal

As close to Emscripten as the design allows; 80–90% of Emscripten is a
realistic stretch goal. What can't be removed entirely, given that we run
arbitrary x86 code safely:

- guest memory checks (now one compare per load, one table lookup per store);
- the lookup for indirect calls;
- registers the program can observe at fault points.
