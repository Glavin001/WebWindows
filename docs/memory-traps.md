# Memory traps: bounds traps instead of memory checks

Every guest load and store the translator can't prove valid gets a check:
an address in the null region (below 64 KB) or above the guest limit
raises an access violation at the exact instruction, with every register
written back. Accesses near one that passed share its check, but what is
left still costs: with no checks at all (unsafe), Lua runs 15% faster and
SQLite 33% faster on translated Wine.

Memory-trap mode (`wwt translate --mem-traps`) drops those checks and lets
the WebAssembly engine's own bounds trap catch bad addresses. A fault
still reports the right instruction and the address it accessed, but the
registers are as last written back (at the last call, typically), not
their exact values.

| Mode | Checks | Access violation reports |
| --- | --- | --- |
| Faithful (default) | Explicit compare per checked access | Instruction, address, every register |
| Memory traps (`--mem-traps`) | None: the engine's bounds trap | Instruction, address from the registers as last written back, those registers |

## How it works

Guest memory starts at address 0 of the WebAssembly memory: the guest's
address is the WebAssembly address, which is what lets Wine's Unix side
(native/wine-unix) use guest pointers directly. The runtime's own region
(Emscripten's data and heap, the translator's tables) sits above the guest
limit.

A trapping access computes `addr + off - 0x10000` with 32-bit wrapping, as
x86 does, and accesses it with a memarg offset of `0x10000`, which the
engine adds without wrapping:

* a valid address lands on itself;
* one in the null region wraps to 4 GB or more, beyond any 32-bit memory:
  the engine traps;
* one above the top of memory traps too.

In V8 this is one `lea` where a check was a `lea`, a compare and a branch,
and it needs no fault path, which kept values live across every check.

Moving the guest so that every invalid address falls outside the memory
would need the runtime's region below the guest, so guest addresses
would no longer be WebAssembly addresses. Every guest pointer Wine's Unix
side follows would then need translating. So the layout stays, with one
gap: an address between the guest limit and the top of memory (the
runtime's region, a few hundred MB above 2 GB on Wine) doesn't trap. A
read there returns the runtime's data, and a write can corrupt it.
Faithful mode doesn't have this gap.

What keeps its explicit check in either mode:

* stores while code is writable: they go through the store map, whose slow
  path still checks precisely;
* atomics, `rep movs`/`stos` and other bulk copies;
* 64-bit code and 64-bit memories (the trick needs a 32-bit memory).

### From trap to access violation

Each module translated with traps carries a `wwt.traps` section
(`wwt::abi::TRAPS_SECTION`). It lists the trapping accesses, and also the
accesses a nearby trapping access covers, whose plain addresses trap only
above the top of memory. For each access it records:

* the module offset of its load or store, which is the position engines
  report for a trap there;
* its x86 address and whether it writes;
* the instruction's memory operand (base and index registers, scale,
  segment, displacement).

`Machine.run` (runtime/runtime.mjs) catches the `RuntimeError` and reads
the innermost WebAssembly frame of its stack trace (function name and
module offset, in V8's and SpiderMonkey's formats). If that frame is a
trapping access, it raises ACCESS_VIOLATION at the access's instruction,
with the address computed from the operand and the registers. Any other
trap is rethrown, as before. JavaScriptCore's stack traces carry no
offsets, so on Safari a trap is not recognized and stops the program.

On Wine, the exception frame goes below the innermost handler registration
(`fs:[0]`) and a further 4 KB. The stack pointer written back last can be
above the faulting function's newest pushes and locals, which a frame
placed there would overwrite.

## Using it

| Where | Setting |
| --- | --- |
| `runtime/node/run.mjs`, `tests/programs/check.mjs` | `--mem-traps` |
| `runtime/node/wine.mjs` | `--mem-traps` (the program and Wine's DLLs), or `--mem-traps=prog.exe,ntdll.dll` for some modules |
| The page | `?memtraps=1` (the program; Wine's DLLs as the bundle has them) |
| `runtime/node/wine-bundle.mjs` | `WWT_MEM_TRAPS=1` (the bundle's DLLs) |
| In-browser translator | flag bit 4 (16) of `wwt_translate` and `wwt_translate_pe` |
| `tools/bench/suite.mjs` | the `wwt-traps` tier |

The setting is part of every translation cache key (wine.mjs's flags, the
page's `-traps` key suffix). ABI 9 brings the `wwt.traps` section, so a
runtime from before it refuses these modules instead of crashing at their
first bad address.
