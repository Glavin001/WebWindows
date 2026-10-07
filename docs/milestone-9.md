# x86-64 — 64-bit Windows programs alongside 32-bit

Status as of October 7, 2026. This is the first step of the plan's "after
v1" item, "x86-64 on 64-bit WebAssembly memory" ([plan](plan.md#64-bit-programs)):
the translator, the runtime and the test layers for 64-bit programs on the
M1 shims. Wine's 64-bit side is the next step (see the end).

## The design: two stacks, one translator

32-bit programs keep everything they have: 32-bit WebAssembly memory, Wine's
i386 DLLs and the pure i386 system-call path, at full speed. 64-bit programs
get a second stack: x86-64 code, Wine's x86_64 DLLs (next step) and, by
default, a 64-bit (memory64) WebAssembly memory, so Wine's Unix side can be
built for wasm64 with the same pointer size as the guest and needs no
structure conversion. The PE header's machine field picks the stack per
process. One translator serves both, through two independent settings:

| Setting | Values | Chosen by |
| --- | --- | --- |
| Mode | x86 (i386) or x86-64 | the PE machine field (`0x14c` / `0x8664`) |
| Address model | 32-bit memory or 64-bit memory (`--mem64`) | the runtime |

All four combinations work. x86-64 code on a 32-bit memory keeps every guest
address below 4 GB and wraps x86-64 addresses after a full 64-bit check; it
is the fallback if memory64 turns out too slow, and what Safari can run
until it ships memory64. 32-bit code on a 64-bit memory is the benchmark
lane for the memory64 cost, and would let 32-bit programs share a 64-bit
process later (a single WoW64 stack, as upstream Wine and Hangover do).

**Code addresses stay 32-bit.** Every image is loaded below 4 GB: a 64-bit
image preferred above it (`0x1_4000_0000` for executables, `0x1_7000_0000`
and up for Wine's DLLs) is moved by its relocations, with the high bits of
the base dropped (`0x1_4000_0000` loads at `0x4000_0000`). Windows itself
rebases x86-64 images under ASLR, and MinGW and MSVC (since about 2010) link
them relocatable; images with no relocation directory that are not marked
as stripped have nothing to fix (x86-64 code is RIP-relative) and move
freely. Keeping code below 4 GB keeps the translator's 32-bit code
addresses, its two-level lookup table and the dispatcher unchanged; indirect
jumps, calls and returns check that the target's high 32 bits are zero.
Data addresses are full 64-bit values. A fixed-base image above 4 GB is
refused for now (VS2005/2008-era executables built without
`/DYNAMICBASE`); widening code addresses is the fallback.

## What changed

* **IR:** state vregs for r8–r15 and xmm8–15. In x86-64 code the general
  registers, addresses and lazy flag operands are i64. Operations of 8 to
  32 bits keep their i32 forms (a register read below 64 bits wraps it, a
  32-bit write zero-extends as the hardware does, 8/16-bit writes merge),
  so only 64-bit operand sizes take new paths. The optimizer folds the
  `wrap(extend(x))` pairs this produces.
* **Flags:** width code 3 is 64 bits; the flag formulas are built with i64
  value operations for it (every flag still comes out as an i32 0/1), and
  narrower kinds in x86-64 code wrap their i64 operands into the existing
  32-bit formulas. New helpers `Eflags64`/`EvalCond64` handle unknown kinds.
* **CPU struct:** `abi::cpu64` keeps every field of the 32-bit struct that
  does not widen at the same offset (EIP, flag kind, x87, MMX, xmm0–7,
  MXCSR, fault information) and adds the 64-bit registers, lazy operands,
  FS/GS bases and xmm8–15 after it (832 bytes).
* **Lifter:** 64-bit operand sizes, REX byte registers, RIP-relative
  operands folded to constants (trusted when they point into image data),
  8-byte stack slots, `cdqe`/`cqo`/`movsxd`/`jrcxz`/`cmpxchg16b`/
  `pushfq`/`popfq`/`endbr64`, string instructions on RCX/RSI/RDI and their
  `q` forms, 128-bit `mul`/`imul` (an inline 4-multiply sequence) and
  `div`/`idiv` (WebAssembly division when the dividend fits in 64 bits, a
  128/64 helper otherwise), 64-bit `bswap`, shifts, rotates, `shld`/`shrd`,
  `rcl`/`rcr`, bit scans and tests, `cpuid` with long mode, SSE on
  xmm8–15, `movq` and conversions with 64-bit general registers, and
  `fxsave` with 16 registers. A 32-bit `cmpxchg` leaves RAX's upper half
  alone on success.
* **Discovery:** the 64-bit decoder; `.pdata` function starts as seeds
  (chained entries skipped); x86-64 jump tables (`jmp [table + i*8]`, and
  `lea b, [rip+X]; mov/movsxd e, [b + i*4 + D]; add r, b; jmp r` for both
  MSVC's image-base-relative and GCC/Clang's table-relative entries);
  8-byte pointer scans; `call [rip+slot]` for the setjmp return-site rule.
* **PE32+:** 8-byte import thunks with the bit-63 ordinal flag, DIR64
  relocations, TLS64, the exception directory, and the move below 4 GB
  (`PeFile::image_base` is where the image loads, `preferred_base` the
  file's). Module metadata records the mode and both bases.
* **Code generation and kernel:** with 64-bit memory the memory import is
  memory64, the CPU pointer, `lookup_l1`, `guest_limit` and `code_bitmap`
  are i64, the first-level lookup table holds 8-byte pointers, and narrower
  addresses are zero-extended. With 32-bit memory, x86-64 addresses are
  checked in i64 against the guest limit and then wrapped. 32-bit output is
  unchanged: the generated-WebAssembly snapshots match byte for byte.
* **Runtime:** `Machine({ arch: 'x64', mem64 })`: x86-64 register access,
  `callGuest` in the Windows x64 convention, 64-bit memory with the
  standard JavaScript API (`address: 'i64'`) or Node 22's flagged one
  (`index: 'i64'`), and pointers from translated code normalized (i32
  pointers above 2 GB arrive negative, i64 ones as BigInt). The loader
  applies relocations when an image loads away from its preferred base.
  The M1 Win32 shims serve both architectures: x86-64 calls see their
  register arguments spilled to the home space, so every API reads 8-byte
  stack slots, and the TEB (at `gs:0`), PEB, `struct lconv`, msvcrt's
  `FILE` and `jmp_buf` take their 64-bit layouts. x86-64 programs default
  to a 3 GB guest region (images fold to 1–2 GB).
* **CLI and in-browser translator:** `wwt translate --mem64`,
  `wwt translate --base <hex>`, `wwt kernel --mem64`; `wwt_translate` flags
  for x86-64 and 64-bit memory, `wwt_kernel64`.

## Test results

**Layer 1 — instructions:** in progress: a 64-bit oracle (`tools/oracle/oracle64.c`), 64-bit case generation and the first `integer64`/`sse64` fixtures; results to follow.

**Layer 2 — programs** (`tests/programs/check.mjs --arch x64`): built with
`x86_64-w64-mingw32-gcc` at -O0 to -Os, the reference built with the native
x86-64 gcc. (Linux is LP64 and Windows LLP64; these programs and Csmith's
fixed-width types do not depend on `sizeof(long)`, and torture tests check
themselves.)

| Suite | Memory | Result |
| --- | --- | --- |
| 7 hand-written programs × 5 levels (jump tables, `setjmp`/`longjmp`, x87 and SSE floats, 64-bit integers, strings, varargs, `qsort` callbacks) | 32-bit | 35/35 pass |
| same | 64-bit | 35/35 pass |
| 32-bit programs (regression) | 32-bit | 35/35 pass |
| 32-bit programs | 64-bit | 35/35 pass |
| Csmith, 100 programs × -O0/-O2 | 32-bit | 186 pass, 0 fail, 14 skipped (native run timed out) |

**Layer 5 — snapshots:** unchanged for 32-bit code apart from the
numbering of temporaries in the IR snapshots (they start at v56 now that
there are 56 state vregs) and three new metadata fields.

## Measurements

CoreMark (`tools/bench/mem64.sh`, -O2, Node 24.21 / V8 13.6, x86-64 Linux
development machine), score (higher is better). All six runs pass
CoreMark's own validation. The machine was shared with other jobs, so
runs vary by several percent; the second run had a test build going.

| | Run 1 | Run 2 |
| --- | --- | --- |
| Native x86 (`gcc -m32`) | 25,010 | 25,226 |
| Native x86-64 | — | 26,928 |
| x86 code, 32-bit memory | 9,286 | 9,994 |
| x86 code, 64-bit memory | 9,029 (−3%) | 8,633 (−14%) |
| x86-64 code, 32-bit memory | — | 8,119 |
| x86-64 code, 64-bit memory | — | 8,548 |

What this says so far: in V8 the memory64 cost on this benchmark is well
under the 10–100% the plan feared (V8 traps out-of-bounds memory64
accesses with a single compare since 13.0), and x86-64 code is a little
slower than x86 code for now (its 64-bit values and wrap/extend pairs, which
the optimizer only partly folds). Firefox checks memory64 bounds explicitly
and is expected to cost more; it needs the same measurement before the
decision in the plan is settled. (Run 1's x86-64 numbers are missing: the
first version of the script built CoreMark's "simple" port, which keeps
pointers in a 32-bit integer; the script now builds a 64-bit copy of it.
Translated and native x86-64 runs of that build produce identical CRCs.)

## Not done yet

* **Wine for 64-bit programs.** The next step, in the plan's order:
  Wine's x86_64 PE DLLs translated (`--enable-archs=x86_64`, a second build
  tree); the x86-64 system-call boundary in `runtime/wine/host.mjs`
  (Wine 11's stub is `mov r10, rcx; mov eax, id; test byte
  [0x7ffe0308], 1; jne; syscall; ret; ... call [0x7ffe1000]`, so the host
  commits 0x7ffe1000 with the dispatcher thunk and leaves
  `KUSER_SHARED_DATA.SystemCall` clear; arguments in r10, rdx, r8, r9 and
  then the stack from rsp+0x30); `KiUserCallbackDispatcher`'s fixed frame;
  structure layouts generated with x86_64 MinGW; `syscalls.mjs` on
  pointer-size accessors; and Wine's Unix side built for wasm64 with
  Emscripten (`-sMEMORY64`, with `prepare.py` mapping wasm64 to the
  x86_64 layout as it maps wasm32 to i386 today).
* **Exceptions:** x86-64 exceptions are table-based (`.pdata` unwind
  information). Translated code keeps the guest stack real, so Wine's
  `RtlVirtualUnwind` works on it as long as prologue saves and stack
  adjustments are never optimized away (they are stores, which the
  optimizer keeps). Dispatch itself arrives with Milestone 5, for both
  architectures; fast mode must then honor `RtlAddFunctionTable`.
* **Threads:** `cmpxchg16b` is two 8-byte accesses, not atomic; it needs a
  lock-based emulation before Milestone 5's threads.
* **AVX:** not lifted; `cpuid` does not report it. 256-bit operations
  would split into pairs of 128-bit ones.
* **Fixed-base images above 4 GB** are refused (see above).
