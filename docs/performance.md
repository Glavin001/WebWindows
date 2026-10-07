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

## Results

Development container (4 vCPU Xeon, Node 22), CoreMark 1.0, `-O2`. Runs on
this shared machine vary by 5–10%, native included; ratios are taken
within one `coremark.mjs` run.

| Build | Iterations/s | vs. native | vs. Emscripten |
| --- | --- | --- | --- |
| Native, `gcc -m32 -O2` | ~19,500–21,900 | 100% | — |
| Emscripten `-O2`, Node | ~21,300–22,700 | ~97–116% | 100% |
| `wwt`, M1 shims, Node: before this work | ~7,700 | ~35% | ~36% |
| … with irreducible loops made reducible | ~8,300–9,000 | ~40% | ~40% |
| … and the store map with inline invalidation | ~9,400–10,000 | ~45% | ~44% |
| … no memory or code-write checks at all (`--variants`, for reference) | ~12,000 | ~55–60% | ~53% |

Translated Wine runs compute-bound code at the same speed as the M1 shims
(its DLLs are only on the path for system calls; see
[milestone-2.md](milestone-2.md#speed-coremark)).

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
* **Store checks** (`wwt::abi::store_map`). Each guest store used to do a
  bounds check and then a code-page check from a bitmap that called the
  host on a hit. Calls on cold paths are expensive even when not taken: V8
  doesn't know the branch is unlikely, so values live across it get
  spilled. Removing just that call was worth more than all the fault-path
  register write-back. Stores now look up one byte per page: zero means a
  plain store. The null region, the last guest page and everything above
  the guest limit go down a slow path that does the precise check. So do
  pages with translated code, where the translated code invalidates the
  page's lookup entry itself, with no call to the host.

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
```

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

## 3. Compare the code of one hot function

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
| `i32.sub 65536; global.get 2; i32.gt_u; if … fault` | Guest-limit check on loads. Fix: merge checks with the same base |
| `global.get 3; i32.add; i32.load8_u; if` | Store-map check |
| A `call` inside a rarely taken `if` | Forces spills around it; keep calls off hot paths (fault paths end in `unreachable`, which V8 treats as cold) |
| `call_indirect` through the two-level lookup | Indirect calls and jumps. Fix: cache them or inline the lookup |
| Leftover flag computations | Flag lowering didn't fuse them |

Count instructions, memory operations and calls in both listings. Most of
the gap comes from a few rows of this checklist.

## Goal

As close to Emscripten as the design allows; 80–90% of Emscripten is a
realistic stretch goal. What can't be removed entirely, given that we run
arbitrary x86 code safely:

- guest memory checks (now one compare per load, one table lookup per store);
- the lookup for indirect calls;
- registers the program can observe at fault points.
