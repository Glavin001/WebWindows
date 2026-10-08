# Memory traps: bounds traps instead of memory checks

Every guest load and store the translator can't prove valid used to get
an explicit check. An address in the null region (below 64 KB) or above
the guest limit raised an access violation at the exact instruction, with
every register written back. Accesses near one that passed share its
check, but what is left still costs: with no checks at all (unsafe), Lua
runs 15% faster and SQLite 33% faster on translated Wine.

Memory traps (`wwt translate --mem-traps`) drop those checks and let the
WebAssembly engine's own bounds trap catch bad addresses. A fault still
reports the right instruction and the address it accessed. The registers,
though, are as last written back (typically at the last call), not their
exact values.

| Mode | Checks | An access violation reports |
| --- | --- | --- |
| Memory traps (the default where the engine locates traps: Node, Chrome, Firefox) | None: the engine's bounds trap | The instruction; the address, from the registers as last written back; those registers |
| Faithful (Safari; `--no-mem-traps`, `?memtraps=0`) | An explicit compare per checked access | The instruction, the address and every register, exactly |

## How it works

Guest memory starts at address 0 of the WebAssembly memory, so a guest
address is a WebAssembly address. That is what lets Wine's Unix side
(native/wine-unix) follow guest pointers directly. The runtime's own
region (Emscripten's data and heap, the translator's tables) sits above
the guest limit.

A trapping access computes `addr + off - 0x10000` with 32-bit wrapping, as
x86 does, and accesses it with a memarg offset of `0x10000`, which the
engine adds without wrapping:

* a valid address lands on itself;
* one in the null region wraps to 4 GB or more, beyond any 32-bit memory,
  and the engine traps;
* one above the top of memory traps too.

In V8 the check was a `lea`, a compare and a branch (plus a fault path
that kept values live across it); the trapping access is one `lea`:

```
; check, then load                     ; trapping load
lea   ebx, [rax-0xfffc]                lea   ebx, [rax-0xfffc]
cmp   ebx, 0x3ffeffe0                  mov   eax, [rdx+rbx+0x10000]
ja    fault
mov   eax, [rdx+rax+0x4]
```

Accesses near one that passed skip their check in faithful mode. In this
mode they skip the rotation the same way and use their plain address.

The plan was to move the guest so that every invalid address falls
outside the memory. That would put the runtime's region below the guest,
so guest addresses would no longer be WebAssembly addresses, and every
guest pointer Wine's Unix side follows would need translating. So the
layout stays, with one gap: an address between the guest limit and the
top of memory (the runtime's region, a few hundred MB above 2 GB on Wine)
doesn't trap. A read there returns the runtime's data, and a write can
corrupt it. Faithful mode has no such gap.

What keeps its explicit check in either mode:

* stores while code is writable: they go through the store map, whose slow
  path still checks precisely;
* atomics, `rep movs`/`stos` and other bulk copies;
* 64-bit code and 64-bit memories (the trick needs a 32-bit memory and
  32-bit addresses).

### From trap to access violation

Each module translated with traps carries a `wwt.traps` section
(`wwt::abi::TRAPS_SECTION`). It lists the trapping accesses, and the
accesses a nearby trapping access covers, whose plain addresses trap only
above the top of memory. For each access it records:

* the module offset of its load or store, which is the position engines
  report for a trap there;
* its x86 address and whether it writes;
* the instruction's memory operand (base and index registers, scale,
  segment, displacement).

`Machine.run` (runtime/runtime.mjs) catches the `RuntimeError` and reads
the innermost WebAssembly frame of its stack trace: the function's name,
which carries its x86 entry, and the module offset. If that frame is a
trapping access, the runtime raises ACCESS_VIOLATION at the access's
instruction, with the address computed from the operand and the
registers. Any other trap is rethrown, as before.

On Wine, the exception frame goes below the innermost handler registration
(`fs:[0]`) and a further 4 KB. The stack pointer written back last can be
above the faulting function's newest pushes and locals, which a frame
placed there would overwrite.

### Which engines

`trapsMappable()` (runtime/runtime.mjs) runs a one-function module that
loads past the end of its memory. It checks whether the trap's stack
trace has the `wasm-function[N]:0xOFFSET` form the runtime needs. The
runtimes default to memory traps only where it does:

* **V8 (Node, Chrome):** yes. Tested here in Node 22 and headless
  Chromium (`tests/web/traps.mjs`).
* **SpiderMonkey (Firefox):** its traces have the same form
  (`name@url:wasm-function[N]:0xOFFSET`), which `trapSite` parses. Not
  run in Firefox here.
* **JavaScriptCore (Safari):** its traces carry no offsets, so the probe
  fails and Safari stays faithful. In memory-trap mode there, a bad
  address would stop the program instead of raising an access violation.

## Settings

| Where | Default | Settings |
| --- | --- | --- |
| `runtime/node/wine.mjs` | Memory traps for the program, Wine's DLLs and code translated at run time | `--no-mem-traps` or `WWT_MEM_TRAPS=0`; `--mem-traps=prog.exe,ntdll.dll` for some modules only |
| `runtime/node/run.mjs` | Memory traps | `--no-mem-traps`, `--mem-traps` |
| `tests/programs/check.mjs` | Memory traps | `--no-mem-traps` |
| The page | The program and code translated at run time, where `trapsMappable()`; Wine's DLLs as the bundle has them (faithful) | `?memtraps=0` or `?memtraps=1` |
| `runtime/node/wine-bundle.mjs` | Faithful (one bundle serves every browser) | `WWT_MEM_TRAPS=1` |
| `wwt translate` | Faithful | `--mem-traps` |
| In-browser translator | Faithful | Flag bit 4 (16) of `wwt_translate` and `wwt_translate_pe` |
| `tools/bench/suite.mjs` | `wwt-wine` is the default (memory traps) | The `wwt-faithful` tier |
| `tools/bench/history/run.mjs` | | The `faithful` ablation |

The setting is part of every translation cache key (wine.mjs's flags, the
page's `-traps` key suffix). ABI 9 brings the `wwt.traps` section, so a
runtime from before it refuses these modules instead of crashing at their
first bad address.

CI runs its tests with the default (memory traps). It also runs the
programs at -O0 and -O2, and the exception tests (`seh`, `unhandled`),
with faithful checks.

## Results (2026-10-08)

Raw outputs are in [results/2026-10-08/memory-traps](results/2026-10-08/memory-traps).
In those files, faithful is the default and `wwt-traps` is the
memory-trap tier, because they predate the change of default.

### Speed: the suite

`tools/bench/suite.mjs --rounds 3 --pin 2 --tiers native,emcc,wwt-wine,wwt-traps`
(`suite.json`, `suite.txt`). Seconds, lower is better; the program and
Wine's DLLs are translated with traps.

| Benchmark | Faithful | Memory traps | Change |
| --- | --- | --- | --- |
| SQLite speedtest1 | 20.44 | 16.59 | +23% |
| Lua fib | 0.567 | 0.481 | +18% |
| Lua sort | 1.943 | 1.656 | +17% |
| Lua float | 1.769 | 1.553 | +14% |
| Lua objects | 3.992 | 3.534 | +13% |
| Lua tables | 1.430 | 1.277 | +12% |
| Lua strings | 1.055 | 1.005 | +5% |
| CoreMark | 3.349 | 3.556 | −6% |
| apibench (8 benchmarks, each under 2 s) | | | −6% to +19% |
| Geometric mean, % of native | 22% | 25% | +14% |

Checksums match in every lane. The suite pins everything to one core
here, so its percentages of native are lower than unpinned runs; the
comparison between the two modes is what it measures.

### Speed: A/B runs

`tools/bench/ab.mjs`: variants alternate within each round, median of
rounds. `nochecks` is `--no-mem-checks`: no checks at all, unsafe, as an
upper bound.

| Workload | Rounds | Memory traps | No checks |
| --- | --- | --- | --- |
| Lua on Wine, total | 5 | +10% | +15% |
| … fib / tables / strings / sort / objects / float | 5 | +30% / +4% / +13% / +5% / +13% / +11% | +51% / +16% / +14% / +10% / +13% / +16% |
| SQLite speedtest1 on Wine | 5 | +23% | +33% |
| CoreMark (JavaScript Win32 shims, no Wine), pinned | 15 | −0.9% | +8.3% |
| CoreMark, pinned | 21 | −3.4% | +6.5% |

Between memory traps and no checks remain the address adjustment and the
checks atomics and bulk copies keep.

### Why CoreMark is slower

A V8 CPU profile of CoreMark (three runs per mode) puts the loss in its
list and CRC loops:

| Function | Faithful (ms) | Memory traps (ms) | Change |
| --- | --- | --- | --- |
| `core_bench_state` | 836 | 861 | +3.0% |
| `core_bench_list` | 583 | 645 | +10.5% |
| `matrix_test` | 531 | 548 | +3.3% |
| `crc16` | 149 | 163 | +9.4% |
| `core_list_mergesort` | 142 | 155 | +9.2% |

A check sits beside its load, and the branch is predicted, so it costs
throughput but no latency. The adjusted address adds an addition before
the load. That matters when each load's address comes from the previous
load, as when walking a list.

Keeping explicit checks for addresses that loads define (and their
copies) left 161 of CoreMark's 758 checked accesses trapping:

| | Memory traps | Checks kept on loaded addresses |
| --- | --- | --- |
| CoreMark | −3% | +0.4% |
| Lua total | +10% | +2% |
| SQLite | +23% | 0% |

Lua's and SQLite's hot accesses are the same kind, so that variant is not
kept: every access the translator would check uses the trap.

The first version adjusted every guest access, including those a nearby
check would have covered. CoreMark was 3.8% slower than faithful, so
covered accesses keep their plain address, as above.

### Correctness

All with memory traps, against the same checks in faithful mode:

| Suite | Result |
| --- | --- |
| Hand-written programs (`check.mjs`, -O0 to -Os) | 45/45 (`programs.log`) |
| … on translated Wine (`--wine`) | 45/45 |
| Csmith, 40 programs at -O0 and -O2 (seed 7000) | 74 pass, 0 fail, 6 skipped (the native build crashes or times out) |
| GCC torture, -O0 and -O2 | 3244 pass, 0 fail, 38 skipped, 10 expected failures (`torture.log`) |
| … on translated Wine, -O2 | 1618 pass, 0 fail, 22 skipped, 6 expected failures |
| Windows test programs (`tests/wine/win32.mjs`: SEH, threads, timers, audio, DirectDraw, DirectInput, unhandled exceptions), -O0 and -O2 | 16/16, the recorded output |
| A null read in headless Chromium (`tests/web/traps.mjs`) | ACCESS_VIOLATION at the same instruction and address as faithful |
| Wine's kernel32, gdi32 and user32 tests (`winetest.log`, `*_test-*.json`) | 69 of 71 units the same as faithful; none worse than the recorded baseline |
| `cargo test --workspace` | Pass; faithful code unchanged (the only snapshot change is the ABI version) |

The SEH test (`tests/wine/win32/seh.c`) reports the same codes,
instructions, fault addresses and registers in both modes. Its faults
come right after the registers are written back, which is not always so.
Before the memory operand was recorded, the fault address was reported
as 0.

`timers` (in the Windows test programs) can fail its "periodic ticks in
300 ms" line when the machine is busy: it did once in each mode while
the benchmark programs were building on every core, and passed when
rerun.

Wine's conformance tests, per suite. "The same" counts units with the
same outcome and failure count in both modes; failures are summed over
all units, including those that crash or time out:

| Suite | Units | The same | Not finishing (each mode) | Failures, faithful | Failures, memory traps |
| --- | --- | --- | --- | --- | --- |
| kernel32 | 33 | 31 | 13 | 12035 | 12196 |
| gdi32 | 14 | 14 | 1 | 150 | 150 |
| user32 | 24 | 24 | 8 | 181 | 181 |

The two kernel32 units that differ:

* `sync` had 2084 failures once against faithful's 2083, then 2083 with
  the same failures on a second run (and once timed out). It is
  timing-dependent.
* `virtual` stops in both modes: in this run it timed out in faithful mode
  (6115 failures) and ran out of memory in memory-trap mode (6275). Run
  alone for 150 s, both run out of memory at about 9 GB (6295 and 6275
  failures), growing the function table. It crashes in the recorded
  baseline too.

Those two account for the 161 extra failures. Every unit that doesn't
finish here (in either mode) doesn't finish in the recorded baseline
either.
