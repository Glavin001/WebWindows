# x86-64 — 64-bit Windows programs alongside 32-bit

Status as of October 7, 2026. This is the first step of the plan's "after
v1" item, "x86-64 on 64-bit WebAssembly memory" ([plan](plan.md#64-bit-programs)):
the translator, the runtime and the test layers for 64-bit programs, on the
M1 shims and on Wine's own x86_64 DLLs, translated, with Wine's Unix side
(wineserver, win32u) built for wasm64. 64-bit Notepad and Minesweeper run
in Node and in the browser page.

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
| Address model | 32-bit memory or 64-bit memory | the runtime: 64-bit memory for 64-bit programs (`--mem32` to opt out; the browser falls back when it has no memory64), 32-bit memory for 32-bit ones (`--mem64` to opt in) |

All four combinations work. x86-64 code on a 32-bit memory keeps every guest
address below 4 GB and wraps x86-64 addresses after a full 64-bit check; it
is the fallback if memory64 turns out too slow, and what Safari can run
until it ships memory64. 32-bit code on a 64-bit memory is the benchmark
lane for the memory64 cost, and would let 32-bit programs share a 64-bit
process later (a single WoW64 stack, as upstream Wine and Hangover do).

**64-bit programs run at their own addresses.** On a 64-bit memory, x86-64
code addresses are 64-bit end to end: translated functions return an i64
next address, eip lives in an 8-byte `RIP` slot of the x86-64 CPU struct,
the module's function list (`wwt.funcs64`) holds u64 entries, and the
lookup's first level has one entry per 4 KB page of the guest region
(clamped to a last entry, pointing at an empty second level, for anything
above it). Images load at their preferred bases (`0x1_4000_0000` for a
MinGW or MSVC executable, `0x1_7000_0000` and up for Wine's DLLs), in an
8 GB guest region by default, with the stack, heap and TEB wherever the
process puts them; data addresses were already full 64-bit values. This is
what the 64-bit Wine needs: its DLLs and its `ntdll` loader assume the real
x86-64 address space.

On a 32-bit memory (the fallback for browsers without memory64) code
addresses stay 32-bit: an image preferred above 4 GB is moved below it by
its relocations, with the high bits of the base dropped (`0x1_4000_0000`
loads at `0x4000_0000`), and indirect jumps, calls and returns send a target
with high bits set to the dispatcher, which faults. Windows itself rebases
x86-64 images under ASLR, and MinGW and MSVC (since about 2010) link them
relocatable; images with no relocation directory that are not marked as
stripped have nothing to fix (x86-64 code is RIP-relative) and move freely.
A fixed-base image above 4 GB runs only on a 64-bit memory.

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
  FS/GS bases, xmm8–15 and RIP after it (832 bytes).
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
  relocations, TLS64, the exception directory, and, for a 32-bit memory,
  the move below 4 GB (`PeFile::image_base` is where the image loads,
  `preferred_base` the file's). Module metadata records the mode and both
  bases. Code addresses are u64 throughout the translator (instructions,
  blocks, discovery maps, call targets, reports).
* **Code generation and kernel:** with 64-bit memory the memory import is
  memory64, the CPU pointer, `lookup_l1`, `guest_limit` and `code_bitmap`
  are i64, the first-level lookup table holds 8-byte pointers, and narrower
  addresses are zero-extended. x86-64 code on it also has 64-bit code
  addresses (`(cpu) -> i64` functions, the `code_pages` global, a third
  kernel, `wwt kernel --code64`); faults then report a 64-bit eip and
  address. With 32-bit memory, x86-64 addresses are checked in i64 against
  the guest limit and then wrapped. 32-bit output is unchanged: the
  generated-WebAssembly snapshots match byte for byte apart from the ABI
  version (3) in the metadata.
* **Runtime:** `Machine({ arch: 'x64', mem64 })`: x86-64 register access,
  `callGuest` in the Windows x64 convention, 64-bit memory with the
  standard JavaScript API (`address: 'i64'`) or Node 22's flagged one
  (`index: 'i64'`), and pointers from translated code normalized (i32
  pointers above 2 GB arrive negative, i64 ones as BigInt). The loader
  applies relocations when an image loads away from its preferred base.
  The M1 Win32 shims serve both architectures: x86-64 calls see their
  register arguments spilled to the home space, so every API reads 8-byte
  stack slots, and the TEB (at `gs:0`), PEB, `struct lconv`, msvcrt's
  `FILE`, `jmp_buf` and `MEMORY_BASIC_INFORMATION` take their 64-bit
  layouts. Guest and native memory are read through `DataView` (and the
  `r32`/`w32` helpers) rather than `u32[a >>> 2]`, so everything works
  above 4 GB; an x86-64 API argument reads as a whole address when it is
  one (below the guest limit) and as its low 32 bits otherwise. x86-64
  programs default to an 8 GB guest region on a 64-bit memory and 3 GB on a
  32-bit one (images fold to 1–2 GB).
* **CLI and in-browser translator:** `wwt translate --mem64`,
  `wwt translate --base <hex>`, `wwt kernel --mem64` and `--code64`;
  `wwt_translate` flags for x86-64 and 64-bit memory, `wwt_kernel64`,
  `wwt_kernel_code64`, and u64 addresses in its C ABI (base, entries,
  known functions, profile). The browser keeps a profile per memory model,
  as float64s.

## Test results

**Layer 1 — instructions**, recorded on a real x86-64 CPU by
`tools/oracle/oracle64.c` (16 registers, rflags, xmm0–15) and replayed
through the translator in optimized mode, fast mode and on a 64-bit memory:

| Group | Instruction forms | Cases | Result |
| --- | --- | --- | --- |
| Integer64 (every form valid in 64-bit user mode except I/O, segments and far branches; REX registers, 64-bit immediates, RIP-relative and 0x67 addressing, full 128-bit `div` dividends, `cmpxchg16b`) | 611 | 14,644 | all pass |
| Fusion64 (flag producer + consumer pairs) | 3,792 pairs | 4,000 | all pass |
| Sse64 (SSE/SSE2 with xmm8–15 and 64-bit general registers) | 305 | 7,320 | all pass |

The 32-bit groups also pass on a 64-bit memory. A larger run (150 cases per
form, 137,000 cases, 30,000 fusion pairs) found nothing else except
`rsqrtps`/`rcpps` on denormal inputs, which the CPU flushes to zero and the
translator does not (32-bit code has the same gap). The suite found and
fixed these x86-64 behaviors: a 32-bit shift, rotate or `shld`/`shrd` by a
masked count of zero still writes (zero-extends) its register; a 64-bit
`rcl`/`rcr` by zero leaves its operand alone; `bsf`/`bsr r32` with a zero
source leaves the 64-bit register alone; a 32-bit `cmpxchg` writes its
register destination only on success and `cmpxchg8b` writes rdx:rax only on
failure; `loop` and string instructions with the 0x67 prefix count in ecx
and use esi/edi (zero-extending them, even with a zero count on this Intel
CPU); `pinsrw` with a 64-bit source; and a lock-prefixed read-modify-write
on a 64-bit memory checked self-modifying code at a clobbered address.
Three of these were recorded on Intel only and may need masking if another
CPU's re-recording differs (failed 32-bit `cmpxchg` destinations,
`bsf`/`bsr` with a zero source, zero-count 0x67 `rep`).

**Layer 2 — programs** (`tests/programs/check.mjs --arch x64`): built with
`x86_64-w64-mingw32-gcc` at -O0 to -Os, the reference built with the native
x86-64 gcc. (Linux is LP64 and Windows LLP64. The hand-written programs do
not depend on `sizeof(long)` and torture tests check themselves; Csmith
programs do, through `L` constants such as `x > -8L` with an unsigned `x`,
so their reference is built with `gcc -m32`, whose `long` is 32-bit as on
Windows.) On a 64-bit memory the programs run at their preferred base,
`0x1_4000_0000`.

| Suite | Memory | Result |
| --- | --- | --- |
| 7 hand-written programs × 5 levels (jump tables, `setjmp`/`longjmp`, x87 and SSE floats, 64-bit integers, strings, varargs, `qsort` callbacks) | 32-bit | 35/35 pass |
| same | 64-bit (at `0x1_4000_0000`) | 35/35 pass |
| 32-bit programs (regression) | 32-bit | 35/35 pass |
| 32-bit programs | 64-bit | 35/35 pass |
| Csmith, 100 programs × -O0/-O2 | 32-bit | 186 pass, 0 fail, 14 skipped (native run timed out) |
| Csmith, 40 more × -O0/-O2 (seeds 7000–7039) | 64-bit | 74 pass, 0 fail, 6 skipped |
| GCC torture (`gcc.c-torture/execute`, 1646 tests), -O0 | 64-bit | 1629 pass, 0 fail, 13 skipped, 4 expected failures |
| same, -O2 | 64-bit | 1618 pass, 0 fail, 20 skipped, 8 expected failures |
| same, -O2, on translated x86_64 Wine (`--wine`) | 64-bit | 1618 pass, 0 fail, 20 skipped, 8 expected failures |

Two expected failures are x86-64 only: MinGW GCC 13 miscompiles
`pr109925` (two arrays share a stack slot while one is live) and
`pr114965` (`main` can only reach `abort()`), the bugs these tests guard
against; `tests/programs/torture-xfail.txt` gives the reasons.

**Browser:** `tests/web/browser.mjs` runs x86-64 programs in headless
Chromium like 32-bit ones: `jumps-O2.exe` (MinGW CRT, `setjmp`/`longjmp`)
is translated in the page, 16 addresses missed ahead of time are
translated at run time and saved to the profile, the second launch
re-translates with them and the third loads the cached module. With
memory64 (Chromium) the program runs at `0x1_4000_0000` in an 8 GB guest
region.

**Layer 5 — snapshots:** unchanged for 32-bit code apart from the
numbering of temporaries in the IR snapshots (they start at v56 now that
there are 56 state vregs), three new metadata fields and the ABI version
(3).

## Measurements

CoreMark (`tools/bench/mem64.sh`, -O2, Node 24.21 / V8 13.6, x86-64 Linux
development machine), score (higher is better). All runs pass
CoreMark's own validation. The machine was shared with other jobs, so
runs vary by several percent; the second run had a test build going.

| | Run 1 | Run 2 | Run 3 |
| --- | --- | --- | --- |
| Native x86 (`gcc -m32`) | 25,010 | 25,226 | 25,756 |
| Native x86-64 | — | 26,928 | 27,579 |
| x86 code, 32-bit memory | 9,286 | 9,994 | 9,703 |
| x86 code, 64-bit memory | 9,029 (−3%) | 8,633 (−14%) | 8,488 (−13%) |
| x86-64 code, 32-bit memory | — | 8,119 | 8,069 |
| x86-64 code, 64-bit memory | — | 8,548 | 8,313 |

Run 3 is after code addresses became 64-bit: x86-64 code on a 64-bit
memory then runs at `0x1_4000_0000` with i64 return addresses and an i64
lookup (Run 2 ran it below 4 GB with 32-bit ones). The difference is within
this machine's run-to-run noise.

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

## 64-bit Wine

64-bit programs run on Wine 11's x86_64 PE DLLs (`ARCH=x86_64
tools/wine/build.sh`, a separate tree in `/opt/wine-build64`), translated
like the i386 ones and loaded at their own bases (`ntdll` at
`0x1_7000_0000`, `kernelbase` at `0x1_7400_0000`, `kernel32` at
`0x1_7800_0000`) on a 64-bit memory with an 8 GB guest region. The
translator handles these DLLs whole: the only instructions it reports
unsupported in `ntdll` are the 258 `syscall`s in the system-call stubs,
which never run (below), and a handful of misdecoded data bytes in
`kernelbase`, `msvcrt` and `ucrtbase`.

`runtime/wine/host.mjs` and `syscalls.mjs` serve both architectures:

* **System calls.** Wine's x86-64 stubs are `mov r10, rcx; mov eax, id;
  test byte [0x7ffe0308], 1; jne 1f; syscall; ret; 1: ...; call
  [0x7ffe1000]; ret`. The host sets `KUSER_SHARED_DATA.SystemCall` and
  stores its thunk at `0x7ffe1000`, so every call arrives as `call
  [0x7ffe1000]`, with the arguments in r10, rdx, r8 and r9 and then at
  rsp+0x30. A 32-bit argument's stack slot can carry garbage in its upper
  half (the caller wrote it with a 32-bit store), so an argument with upper
  bits set counts as a pointer only when it points into mapped memory;
  sign-extended values (pseudo-handles) read as their 32-bit forms.
  `__wine_unix_call_dispatcher` takes the x64 convention.
* **Process setup.** The TEB at `gs:0`, x64 selectors, and the initial
  context for `LdrInitializeThunk` built as `signal_x86_64.c` builds it
  (rcx = entry, rdx = PEB, rip = `RtlUserThreadStart`, the CONTEXT just
  below the stack top, rcx pointing at it).
* **Structures.** `tools/wine-layout` compiles its layout program with
  either MinGW; the 64-bit one (itself a 64-bit program, run through the
  translator) writes `runtime/wine/layout64.json`. The host and the system
  calls take every structure offset from the layout for the process's
  architecture and read and write pointer-sized fields (pointers, handles,
  `SIZE_T`) through `ptr`/`wptr`: `OBJECT_ATTRIBUTES`, `UNICODE_STRING`,
  `IO_STATUS_BLOCK`, `MEMORY_BASIC_INFORMATION`, the process, thread,
  system and section information classes, `EXCEPTION_RECORD`, and the
  x86-64 `CONTEXT` for `NtContinue`. The PE helpers read PE32+ headers and
  apply DIR64 relocations when an image must move.
* **Runner.** `runtime/node/wine.mjs` picks the x86_64 build, a 64-bit
  machine and `--mem64` translation from the program's PE header.

### Wine's Unix side for wasm64

Windowed programs need Wine's Unix side: wineserver, win32u's Unix half and
the parts of ntdll's that talk to the server, compiled with Emscripten into
one module that shares the guest's memory (Milestone 4).
`ARCH=x86_64 native/wine-unix/build.sh` builds it for wasm64
(`-sMEMORY64=1`) into `target/wine-unix64`, linked above an 8 GB guest
region:

* `prepare.py` makes wasm64 a Win64 target with the x86_64 layouts
  (`_WIN64`, the AMD64 `CONTEXT` and `DISPATCHER_CONTEXT`, the AMD64
  machine for wineserver and ntdll), as it makes wasm32 an i386 one.
* `gen-syscalls.py` reads Wine's Win64 table: one 8-byte slot per argument,
  64-bit results (window handles and `LRESULT`s keep their upper half).
* `gen-ntcalls.py` (new, for both builds) generates typed thunks for the NT
  calls the host routes to the module, behind one export,
  `wasm_nt_call(index, slots)`. On wasm64, JavaScript would otherwise have
  to pass each argument as a Number or a BigInt to match its WebAssembly
  type; with the thunks it writes argument slots and C's casts do the rest
  (including dropping a 32-bit argument's garbage upper half).
* The glue's EM_JS bridges take pointers through `ptr()` (pre.js), which
  accepts the negative i32s of wasm32 and the BigInts of wasm64; the NT calls
  it forwards to the host pass pointer-sized slots.
* The host gathers an x64 win32u call's arguments (four registers, then
  the stack) into slots for the module, and runs user callbacks through
  `KiUserCallbackDispatcher` with Wine's x86-64 `callback_stack_layout`
  (arguments at rsp+0x20, length +0x28, id +0x2c, machine frame +0x30, the
  copied data +0x58). The display driver's `INPUT` records take the x64
  layout (the union at offset 8).

Emscripten's memory64 output also uses a 64-bit function table, which V8
supports from Node 24 (and Chrome 133); Node 22's memory64 does not include
it, so 64-bit Wine with the Unix side needs Node 24.

In the browser, `runtime/node/wine-bundle.mjs --arch x64` builds a second
bundle (`target/wine-bundle64`: the x86_64 DLLs, prelinked off MinGW's
shared default base `0x1_8000_0000` and translated, the wasm64 Unix side,
Notepad and Minesweeper). The worker picks the bundle by the program's
architecture, and the page's samples offer `hello.exe` and `hello64.exe` as
console programs and Wine's Notepad and Minesweeper in both builds.

| Test | Result |
| --- | --- |
| `hello64.exe` (kernel32 only) on x86_64 Wine | prints both lines, exit 0 |
| The 7 hand-written programs × 5 levels (MinGW CRT on Wine's msvcrt: printf, `setjmp`/`longjmp`, `qsort` callbacks, x87 and SSE) | 35/35 pass |
| The same on i386 Wine (regression) | 35/35 pass |
| `tests/wine/gui.mjs --arch x64` (a 64-bit `winbasic`, Minesweeper with a click, Notepad with typing), Node 24 | 13/13 checks pass |
| `tests/web/gui.mjs --arch x64` (the same in headless Chromium through the page) | 5/5 checks pass |
| Both GUI tests on the i386 build (regression, with the shared glue changes) | 13/13 and 5/5 pass |

Fixing the layouts also corrected the i386 `SystemBasicInformation`, which
had been written one field off (a 256 KB page size).

### Real programs

`tests/wine/apps.mjs` runs published Windows programs, the 32-bit and the
64-bit build of each where both exist (`sh tools/apps/fetch.sh` downloads
them into `target/apps`), with a folder as `C:\app` (`wine.mjs --dir`, which
writes the files a program creates or changes back to the folder):

| Program | x86 | x64 |
| --- | --- | --- |
| NASM 2.16.03: assemble a file (macros, `%rep`, AVX, RIP-relative), byte-identical to native NASM | pass | pass |
| ndisasm: disassemble it | pass | pass |
| 7-Zip 23.01 `7za`: create an LZMA2 archive, test it, extract it | pass | pass |
| PuTTY 0.85: the configuration dialog (tree view, combo box, list box) | pass | pass |
| plink 0.85 `-V` | pass | pass |
| SQLite 3.53 shell: tables, recursive CTE, indexes, JSON, math | — | pass |
| curl 8.22 (MinGW, LibreSSL, nghttp2/3, libssh2): `-V`, a `file://` download | — | pass |
| trurl: parse a URL | — | pass |

What these needed, on top of the hello-world programs:

* **The API set map.** MSVC-built programs import the Universal CRT as
  `api-ms-win-crt-*`. The host now maps `apisetschema.dll`'s `.apiset`
  section as the PEB's `ApiSetMap`, as Wine's Unix loader does, instead of
  an empty map (both architectures; the bundles ship the DLL).
* **More of Wine:** `ws2_32`, `crypt32`, `dnsapi`, `nsi`, `iphlpapi`,
  `secur32`, `bcrypt`, `normaliz` and `wldap32` (built for both, in the
  bundles). Their Unix libraries (sockets, GnuTLS) are not ported:
  `ws2_32` and `crypt32` get a stub library handle, so they load and fail
  the calls that need the Unix side.
* **File information:** `FileStatInformation` (SQLite) and the
  `FileIdExtdBothDirectoryInformation` listing (7-Zip).
* **64-bit Wine's debug channels** sit at PEB + 0x2000 (PEB + 0x1000 on
  i386), so x64 `err:` messages now show.
* **Jump tables.** curl's translation was 83 MB with single functions of
  several MB, enough for V8's optimizing compiler to run out of zone memory:
  table reads ran on into the next table or into code. Discovery now reads
  a table only to its bound: a `cmp`/`ja` before the jump, or a guard of
  the same index register on a branch to the dispatch block (`cmp ecx, N;
  jbe dispatch`); without one, the table is deferred until the rest of the
  image is explored and then stops at the function's end (`.pdata` on x64),
  at the next function or referenced datum, or at a target inside another
  instruction. curl is now 30 MB (largest function 358 KB), the x64
  `msvcrt` went from 9.9 MB to 4.3 MB, and the Wine DLLs on both
  architectures still translate with no unsupported instructions outside
  `ntdll`'s and `win32u`'s `syscall` stubs (which never run).

Two limits found here belong to work in progress elsewhere: 7-Zip
extracting into a subfolder needs directory creation, and updating an
existing archive needs file rename and delete (the Wine-runtime file
system work).

### Wine's conformance tests on x86-64

The x86_64 builds of Wine's kernel32, user32 and gdi32 tests run in CI
against their own baselines (`tests/wine/baseline/*_test64.json`), as the
i386 ones do:

| Tests | i386: units finished (failures) | x86-64: units finished (failures) |
| --- | --- | --- |
| kernel32 | 14 of 33 (8982) | 18 of 33 (12923, over 2.7× the tests) |
| user32 | 15 of 24 (338) | 16 of 24 (350) |
| gdi32 | 12 of 14 (153) | 12 of 14 (130) |

Getting there fixed, for x86-64: the 17th argument of
`NtUserCreateWindowEx` (`ansi`), which the host's 16 argument slots
dropped, so every window a 64-bit program made through the ANSI API was
Unicode (user32's `edit` test crashed calling a winproc handle); and, for
both, `NtQuerySystemInformation` leaving the length unset for a class it
lacks (kernel32's `version` crashed on x86-64). user32's tests also import
`setupapi`, which CI did not build: the test program could not start and
the baseline check passed on no units; it now builds it, and a unit the
baseline finished that does not run counts as a regression.

The one unit worse on x86-64, gdi32's `metafile`, writes to a file named
by `GetTempFileName` in `C:\windows\temp`, which does not exist yet on
either architecture (directories come with the Wine-runtime file system
work): the test goes on with an uninitialized name, so what happens next
depends on the stack's leftovers.

## Not done yet

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
* **Fixed-base images above 4 GB** are refused on a 32-bit memory (see
  above).
