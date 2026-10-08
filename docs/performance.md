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
| `ab.mjs` | A/B test of translator variants: interleaved rounds, medians, ratio to the first variant (`--wine` for any Windows program on translated Wine) |
| `suite.mjs` | The benchmark suite: real programs and Windows API workloads on every tier, checksums compared (see below) |

## The benchmark suite

CoreMark is pure computation, and real programs are not. `suite.mjs` runs
workloads that look like real applications on every tier that can run
them, and checks that every tier prints the same checksums:

| Workload | What it exercises | Tiers |
| --- | --- | --- |
| `coremark` | CoreMark at a fixed iteration count: computation | native, emcc, wine, wwt-wine |
| `sqlite` | SQLite 3.50.4's `speedtest1 --verify --size 50`: a database engine, file I/O through the file system | native, emcc, wine, wwt-wine |
| `lua` | Lua 5.4.7 running `workloads/bench.lua` (recursion, tables, strings, sorting with a comparator, objects, floating point): an interpreter, allocation | native, emcc, wine, wwt-wine |
| `apibench` | `workloads/apibench.c`: heap, malloc, files, seeks, strings and locale, critical sections and TLS, `qsort` calling back into the program, registry | wine, wwt-wine |

The tiers:

- **native**: the C source built with `gcc -m32 -O2` for Linux;
- **emcc**: the C source built with Emscripten, in Node: the WebAssembly
  ceiling;
- **wine**: the same `.exe` on Wine running natively (Ubuntu's `wine32`),
  i.e. Wine without translation: the fair reference for Windows API work;
- **wwt-wine**: the `.exe` translated, on translated Wine, in Node: what we
  ship.

```sh
apt-get install wine wine32:i386          # after dpkg --add-architecture i386
node tools/bench/suite.mjs                 # all workloads, 3 rounds
node tools/bench/suite.mjs --only lua,sqlite --rounds 5 --json out.json
```

Sources are pinned and checked by SHA-256 (SQLite's amalgamation and
`speedtest1.c`, Lua's release tarball). The translation cache is warmed by
one untimed run, so the numbers are warm starts; a run's first launch,
which translates, is slower.

To try a change against the suite's programs, A/B it:

```sh
node tools/bench/ab.mjs --rounds 7 --file tools/bench/workloads/bench.lua=C:\\bench.lua \
  --wine "target/bench/suite-lua.exe C:\\bench.lua 4" base= noosr=--no-osr
node tools/bench/ab.mjs --wine "target/bench/suite-apibench.exe 2" \
  "old=WWT=/path/to/old/wwt" new=           # two translator builds
```

Results (`suite.mjs`, median of 3, seconds; shared 4-vCPU container, Node
22; the reference is native for portable workloads, native Wine for
apibench). "Before" is the suite's first run (commit `e2032ca`), "after"
adds the call ABI work, the native heap and relocation-only discovery:

| Workload | native | emcc | wine | wwt-wine before | wwt-wine after | after vs ref |
| --- | --- | --- | --- | --- | --- | --- |
| coremark | 1.38 | 1.39 | 1.31 | 2.32 | 2.23 | 62% |
| sqlite/speedtest1 | 4.40 | 4.58 | 31.0 | 11.50 | 10.19 | 43% |
| lua/fib | 0.041 | 0.076 | 0.037 | 0.232 | 0.192 | 21% |
| lua/tables | 0.363 | 0.560 | 0.297 | 1.067 | 1.038 | 35% |
| lua/strings | 0.199 | 0.240 | 0.209 | 0.854 | 0.746 | 27% |
| lua/sort | 0.448 | 0.590 | 0.449 | 1.523 | 1.263 | 35% |
| lua/objects | 0.705 | 1.094 | 0.857 | 4.363 | 3.129 | 23% |
| lua/float | 0.412 | 0.938 | 0.400 | 1.455 | 1.290 | 32% |
| apibench/heap | — | — | 0.062 | 0.190 | 0.039 | 159% |
| apibench/malloc | — | — | 0.060 | 0.168 | 0.050 | 120% |
| apibench/files | — | — | 1.006 | 0.187 | 0.129 | 780% |
| apibench/seek | — | — | 0.178 | 0.135 | 0.133 | 134% |
| apibench/strings | — | — | 0.819 | 3.801 | 3.146 | 26% |
| apibench/sync | — | — | 0.063 | 0.168 | 0.132 | 48% |
| apibench/qsort | — | — | 0.227 | 0.987 | 0.849 | 27% |
| **geometric mean** | | | | **38%** | **54%** | |

(Wine running natively is slow on SQLite and the file workloads because of
its real file system; the translated tier's files live in memory. The
registry workload is not counted: the runtime has no registry yet.)

After merging Milestone 5 (threads, exceptions, sound, DirectDraw and
Direct3D) and the native string functions, one run of the whole suite
(same container; native itself ran 5–25% slower than in the table above,
so compare the ratios):

| Workload | native | emcc | wine | wwt-wine | vs ref |
| --- | --- | --- | --- | --- | --- |
| coremark | 1.29 | 1.35 | 1.25 | 2.14 | 60% |
| sqlite/speedtest1 | 4.65 | 5.03 | 44.8 | 11.51 | 40% |
| lua/fib | 0.047 | 0.079 | 0.043 | 0.231 | 20% |
| lua/tables | 0.363 | 0.637 | 0.376 | 1.063 | 34% |
| lua/strings | 0.231 | 0.252 | 0.254 | 0.810 | 29% |
| lua/sort | 0.609 | 0.609 | 0.511 | 1.657 | 37% |
| lua/objects | 0.824 | 1.226 | 0.961 | 3.333 | 25% |
| lua/float | 0.395 | 0.866 | 0.385 | 1.479 | 27% |
| apibench/heap | — | — | 0.067 | 0.037 | 181% |
| apibench/malloc | — | — | 0.067 | 0.042 | 160% |
| apibench/files | — | — | 1.307 | 0.145 | 901% |
| apibench/seek | — | — | 0.192 | 0.268 | 72% |
| apibench/strings | — | — | 0.881 | 1.251 | 70% |
| apibench/sync | — | — | 0.058 | 0.132 | 44% |
| apibench/qsort | — | — | 0.250 | 0.985 | 25% |
| **geometric mean** | | | | | **56%** |

Lua and SQLite run at the same speed before and after the merge when the
two builds are timed back to back (`ab.mjs`); the lower ratios are the
machine. System calls got slower with the thread scheduler: apibench's
`seek` (a system call every few hundred instructions) went from 0.51 s to
0.78 s at scale 8. Two changes took it back to 0.62 s: a lone thread past
its time slice starts a new slice instead of polling the other threads at
every system call, and the system call path looks up its handlers once
per call number and checks the time slice against the tick count the
ticker keeps in shared memory (a load) instead of calling
`performance.now()`.

What running real programs found that CoreMark could not:

- **The Wine runtime's file system** wrote to the wrong offset, copied a
  whole file on every extending write, never deleted files, had no locks
  and no `FindFirstFile` for Wine 11, and made directories as empty files.
  SQLite failed ("database is locked"); now its verification hash matches
  native.
- **Long-running functions stayed in V8's baseline code.** V8 switches a
  function to optimized code only for later calls; Lua runs a whole program
  inside one `luaV_execute` call. Translated functions now re-enter
  themselves at loop headers (`crates/wwt/src/osr.rs`): Lua's `float` 2.2x
  faster.
- **Interpreters' dispatch** made functions irreducible (the compiler copies
  the jump table into every handler); merging identical jump tables gives
  one dispatch loop.
- **Compile time matters**: compiling everything optimized up front made
  SQLite slower, not faster. Smaller modules start faster: shared fault
  blocks and keeping single-use values on the WebAssembly stack made
  modules 26% smaller and SQLite's short run ~8% faster.
- **`memmove`/`memset`** (12% of SQLite's time) now use WebAssembly's bulk
  memory operations (`crates/wwt/src/builtin.rs`).
- **Calls are where API-heavy code loses time.** Every hop (program →
  msvcrt → kernel32's `jmp [import]` thunk → kernelbase → ntdll) writes
  registers back and reloads them. A microbenchmark put a `TlsGetValue`
  call at 7.5x native, and replacing the address lookup with a direct call
  changed little: the cost is the state traffic, not the call. Three
  changes cut it (`CallAbi` in `crates/wwt/src/ir.rs`):
  - `mov edi, edi`, the hot-patch prologue of every Windows API function,
    was lifted as a copy, so every API call wrote `edi` back.
  - **Wine's own DLLs follow the C calling convention**: no caller reads
    the flags after a call, and callees preserve ebx, esi, edi and ebp.
    The translator recognizes them by the "Wine builtin DLL" signature in
    their DOS stub, and then neither writes the lazy flag state back at
    calls and returns (so the flag computations before them become dead
    code) nor reloads the preserved registers after calls. Constant TEB
    offsets (`fs:[0x18]`) skip the memory check.
  - **Programs are checked, not trusted** (`translate::prove_flags_abi`):
    Delphi's runtime returns comparisons in the flags and MSVC's
    `_aulldvrm` returns in ebx:ecx, so an `.exe` gets flag-free calls and
    returns only if no code after any call in it reads the flags, and
    calls skip the flags only for callees shown not to read them on entry
    (imports likewise, except `_chkesp`). SQLite, Lua, CoreMark and
    apibench all pass. Liveness had to become strong liveness (a dead
    `shl eax, cl` no longer keeps the old flags alive), and flag-setting
    instructions now define all of the lazy flag state.
  - **Import thunks are resolved in the lookup** (`runtime/wine/thunks.mjs`):
    once the loader has filled an image's import table, each
    `jmp [import]` thunk's lookup entry names its target's translation, so
    a call through kernel32's `HeapFree` lands in ntdll directly (a
    hot-patch of the thunk clears the entry like any code write). And a
    program's direct calls to its own import stubs call through the slot.
    apibench heap +30%, sync +25%, Lua +4%; Lua +3% more for the stubs.
  - **Interlocked instructions are plain loads and stores**: guest threads
    never run in parallel, so `lock xadd`/`lock cmpxchg` need not be
    sequentially consistent WebAssembly atomics (`wwt translate --atomics`
    restores them). apibench sync +13%.
- **Debug information made functions of every statement.** The data scan
  for code pointers read DWARF line tables (an address per statement) as
  function entries in programs built with `-g`, splitting functions at
  every statement with a full write-back at each split (Wine's
  `kernel32_test.exe`: 116,143 functions, now 1,324). Images with
  relocations are no longer scanned at all (their relocations list every
  code pointer), and discardable sections never are.
- **Memory checks** cost 13% of Lua (38% of `tables`; measured with
  `--no-mem-checks`). Check elimination now follows a base plus a bounded
  index (an interpreter's `base + (insn >> 3 & 0xff0)`), values copied
  into registers across blocks, and a 32 KB window (a load near a checked
  address can only land in the 64 KB null region or the 4 MB lookup table
  above the guest limit): Lua's module has 14% fewer checks, Lua +4%.
- **Wine's heap** (`RtlAllocateHeap` and friends) was 15–17% of Lua's time:
  handle checks, the LFH front end, critical sections and free lists, all
  as translated x86 with a register write-back at every internal call. It
  is now native WebAssembly (below): `HeapAlloc`/`HeapFree` 5x faster,
  `malloc`/`free` 3x, faster than Wine running natively.
- **Wine's string comparison** (`CompareStringW`, building sort keys a
  byte at a time) was half of apibench's strings workload. It, the hot C
  string functions and the TLS lookups on their paths are now tried as
  native WebAssembly first, falling back to Wine's translated code for
  anything they do not handle exactly (below): apibench/strings 2.7x
  faster.

### ntdll's heap as native WebAssembly

`crates/wwt-heap` implements the whole set of ntdll heap functions
(`wwt::builtin::NATIVE_HEAP`: create, destroy, allocate, free, reallocate,
size, validate, lock, walk, process heaps, heap information, user
values and flags, and ntdll's internal `heap_thread_detach`), so a heap
handle is never seen by both implementations. `wwt translate --native-heap`
gives these functions in ntdll a body that is a tail call to an import of
the same name (`Term::Native`); the import receives the CPU state, reads its
stdcall arguments from the guest stack, sets `eax`, pops the return address
and arguments, and returns the address to continue at, like any translated
function. `HEAP_GENERATE_EXCEPTIONS` continues in ntdll's `RtlRaiseStatus`.

The module is Rust compiled to `wasm32-unknown-unknown`, working directly in
the machine's memory (its memory import is made shared when it is loaded,
`runtime/wine/heap.mjs`). Heaps live in guest memory, in regions from the
Wine host's virtual memory (one call per segment: 1 MB first, doubling to
16 MB, as Wine grows its heaps); its few process-wide values sit in the
guest's null region, where guest code cannot reach. Blocks have an 8-byte
header (exact requested size, region, size class, flags), so pointers are
8-aligned as on 32-bit Windows and `HeapSize` is exact. Requests up to
512 KB round up to one of 75 size classes, each with a LIFO free list;
allocation and free are a few loads and stores. Larger blocks get a region
of their own. The heap handle starts with the Windows-compatible header
(`0xffeeffee`, flags, force flags). Status codes and last errors follow
Wine's `heap.c`; unit tests (`cargo test -p wwt-heap`) call the functions
as translated code does.

`runtime/node/wine.mjs` uses it whenever it is built
(`cargo build -p wwt-heap --target wasm32-unknown-unknown --profile
release-wasm`), translating ntdll with `--native-heap`; `WWT_NATIVE_HEAP=0`
keeps Wine's heap. The Wine bundle ships it the same way for the browser.

Interleaved A/B (`ab.mjs --wine`, `wineheap=WWT_NATIVE_HEAP=0` against
`native=`), medians, shared 4-core machine:

| Workload | Wine's heap | Native heap | Speed |
| --- | --- | --- | --- |
| apibench/heap (`HeapAlloc`/`HeapFree`, 7 rounds) | 0.219 s | 0.041 s | 5.3x |
| apibench/malloc (7 rounds) | 0.187 s | 0.061 s | 3.1x |
| apibench/files (7 rounds) | 0.252 s | 0.187 s | +35% |
| apibench, all eight (7 rounds) | 5.90 s | 5.35 s | +10% |
| lua/objects (7 rounds) | 4.22 s | 3.62 s | +16% |
| lua, all six (7 rounds) | 9.39 s | 8.79 s | +7% |
| sqlite/speedtest1 (5 rounds) | 12.40 s | 11.61 s | +7% |

Wine running natively takes 0.058 s and 0.060 s for apibench's heap and
malloc. Checksums are unchanged (`suite.mjs`). What it does differently
from Wine's heap:

- Blocks are never split or merged, except in heaps created with a maximum
  size, which take exact-size blocks from what is left and split larger
  free blocks when no size class fits. A program that frees many blocks of
  one size and then allocates another size gets new memory, not the freed
  blocks.
- Memory is never decommitted or returned before `HeapDestroy`, except
  large blocks, which are released when freed.
- No debugging modes: the tail and free checking flags, `HEAP_VALIDATE*`
  and Wine's `WINEDEBUG=+heap` change nothing.
- `RtlCreateHeap` with caller-provided memory allocates its own instead.
- `HeapWalk` lists the same kinds of entries as Wine (a region, its
  blocks, its unused rest and an uncommitted range, then large blocks),
  but freed blocks stay separate blocks. Wine's `kernel32_test heap`
  fails four of those layout checks that pass with Wine's own heap (8
  failures against 4, before both stop at the same `GlobalFlags` crash);
  the other `kernel32_test` units give the same results.
- The low-fragmentation heap is reported (`HeapQueryInformation`) after a
  growable heap's 17th new block, approximately when Wine enables it.

### String and locale functions as native WebAssembly

apibench's strings workload ran at 26% of native Wine's speed, and half of
its time was kernelbase's `CompareStringW` machinery: `compare_string`
builds sort keys a byte at a time (`append_sortkey`, `append_weights`), each
byte a translated call with its register traffic. `crates/wwt-strings`
implements such functions natively, chosen by profile:

| Function | DLLs | Declines (runs Wine's code) when |
| --- | --- | --- |
| `CompareStringEx` (so also `CompareStringW`/`A`, `lstrcmp`, `lstrcmpi` for the user's locale) | kernelbase | a named locale; flags it rejects; version/reserved/handle set; NULL strings; keys beyond the scratch memory (strings of ~9,000 characters) |
| `strlen`, `wcslen`, `memcmp`, `strcmp`, `strchr`, `wcschr`, `memchr`, `strcspn` | ntdll, msvcrt, ucrtbase | a pointer it would read is in the null region or the last 64 KB below the guest limit, or beyond |
| `TlsGetValue` | kernelbase, kernel32's import stub | an expansion slot (index ≥ 64) |
| `msvcrt_get_thread_data` (fetched by every locale-aware C runtime call) | msvcrt, ucrtbase | the thread has no data yet |

**Native first, translated body as the fallback.** Unlike the heap, these
are not replaced: `wwt translate --native-strings` gives each of them
(`wwt::builtin::NATIVE_TRY`, by export or COFF symbol) a new entry block,
`Term::NativeTry`, that calls the import with the CPU state written back.
The import either does the whole x86 function (arguments from the guest
stack, `eax`, the return address and stdcall arguments popped) and returns
the next address, which leaves the function, or returns 0 having changed
nothing, and the function continues in its translated body, which is
still there, so whatever the native code does not handle exactly runs
Wine's own code. Such functions are never inlined into callers.

**Faults.** Translated code faults on accesses to the null region and at
or above the guest limit, nowhere else. The native functions check every
range they would read and decline instead of reading a byte that could
fault (scans for a terminator decline when they reach the end of the
readable range), so a bad pointer faults in the translated body at the
same instruction and address as before. None of them writes guest memory
except the TEB's last error, so the store map is not involved.

**Same results.** `compare_string` is ported function by function, down to
Wine's byte arithmetic and the length limit of each key, and reads the
tables kernelbase loaded from `sortdefault.nls`: the translator passes the
addresses of kernelbase's static `sort` and `current_locale_sort` (COFF
symbols) as constant arguments of the import, and the native code checks
that the current sort is one of the table's. One Wine quirk is declined
rather than reproduced: a single character with more than 32 bytes of
primary weights outgrows Wine's static buffer, which Wine then compares
stale. `tests/wine/strings.mjs` builds `tests/wine/strings.c` and runs it
with and without the native functions (`WWT_NATIVE_STRINGS=0`); the
outputs must be identical: CompareStringEx on 128 hand-picked strings
(expansions, kana, old Hangul, digits, punctuation, Hebrew/Arabic, PUA,
surrogates) and thousands of random near-miss pairs under every flag; the
same with `current_locale_sort` set to each of the 75 sorts in turn (the
runtime has no registry, so every user locale gets the default sort);
explicit lengths, long strings, error cases; the C functions of each DLL
on all byte values and bad pointers; TLS. Then 57 faulting calls, one per
run, must stop at the same instruction and address. Each check was
mutation-tested: perturbing the port's compressions, expansions, kana
weights, digit weights, punctuation positions, reversed diacritics or
Turkish casing exceptions makes outputs differ.

Interleaved A/B (`ab.mjs --wine`, `wine=WWT_NATIVE_STRINGS=0` against
`native=`), medians, shared 4-core machine:

| Workload | Wine's code | Native | Speed |
| --- | --- | --- | --- |
| apibench/strings (7 rounds) | 3.731 s | 1.379 s | 2.7x |
| apibench/sync (`TlsGetValue`, 7 rounds) | 0.181 s | 0.157 s | +15% |
| apibench, all eight (7 rounds) | 5.35 s | 3.00 s | +78% |
| lua/strings (7 rounds) | 0.985 s | 0.890 s | +11% |
| lua/tables (7 rounds) | 1.229 s | 1.123 s | +9% |
| lua, all six (7 rounds) | 9.65 s | 9.36 s | +3% |
| sqlite/speedtest1 (7 rounds) | 12.90 s | 12.78 s | +1% (noise) |

Native Wine takes 0.82–0.94 s for apibench/strings: translated Wine went
from 26% to 60–70% of it. In SQLite the C functions' share of the profile
(`memcmp`, `strcspn`, `strlen`) about halved, to 2.7%, a gain A/B runs
cannot separate from the noise. Per call
(`tools/bench/workloads/crtbench.c`, 1M calls each on 8- and 100-byte
strings, 5 rounds):

| Function | Wine's code | Native | Function | Wine's code | Native |
| --- | --- | --- | --- | --- | --- |
| `strlen` | 0.058 s | 0.039 s | `strchr` | 0.083 s | 0.069 s |
| `wcslen` | 0.060 s | 0.048 s | `wcschr` | 0.070 s | 0.059 s |
| `memcmp` | 0.041 s | 0.032 s | `memchr` | 0.064 s | 0.046 s |
| `strcmp` | 0.099 s | 0.072 s | `strcspn` | 0.098 s | 0.096 s |
| `lstrcmpiW` (250K) | 2.887 s | 0.491 s | `CompareStringA` (250K) | 2.967 s | 0.496 s |

Short C string calls are dominated by the call itself (the program's
call, msvcrt's import, the native-try entry), so the gains there are
modest; `strcspn` is about even. Checksums are unchanged (`suite.mjs`),
and Wine's `kernel32_test` gives the same results with and without, except
where a test prints uninitialized memory (`version`, `path`: stack contents
differ when less translated code runs).

`runtime/node/wine.mjs` uses the module whenever it is built
(`cargo build -p wwt-strings --target wasm32-unknown-unknown --profile
release-wasm`), translating ntdll, kernel32, kernelbase, msvcrt and
ucrtbase with `--native-strings`; `WWT_NATIVE_STRINGS=0` keeps Wine's
code. The Wine bundle ships it for the browser. The module's data and
stack sit in the guest's null region above the native heap's; its scratch
memory for sort keys (256 KB) is in the native region above the guest
limit. `WWT_LOCALE` sets the user's default locale (an LCID) for tests.


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

* Simplifying masks by known zero bits (`setg al; movzx eax, al` lifts to
  `((eax & ~0xff) | flag) & 0xff`): cleaner IR, but V8 already folds it
  (CoreMark, Lua, apibench all within noise).
* Inlining small functions that make calls (up to 150 IR instructions):
  Wine's string functions +9%, but kernelbase's module 48% larger, which
  costs compile time; at 60 instructions, no change.
* Re-entry at nested loops too (`osr.rs` re-enters only at outermost
  loops). Lua's `tables` stays in V8's baseline code for its whole run,
  because its hot loop is the interpreter's dispatch loop, nested in the
  loop around calls; re-entry there needs a dispatcher in front of every
  enclosing loop header (to keep loops single-entry) and a counter in
  every nested loop. `tables` +21-30%, but the counters and dispatchers
  cost everywhere else: `fib` -6-12%, SQLite -6%, CoreMark -1.4% (counter
  in a local or in the CPU struct, resume number in a local or read from
  memory: all alike).
* Replacing the address lookup with direct calls: a cross-module call
  costs its state traffic, not the lookup (forcing in-module calls through
  the lookup changed a microbenchmark by under 1 ns per call).

* Running Binaryen's `wasm-opt` over the output (`wwt translate
  --wasm-opt`): V8 already does the local cleanups it would.
* Treating flags, or `ecx`/`edx`, as dead at `ret` and at calls: no
  measurable change on CoreMark, and it would break hand-written assembly
  that passes values that way. (On call-heavy programs the flags do
  matter; see the suite section for the checked version.)
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
