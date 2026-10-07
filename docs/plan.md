# Windows Games in the Browser — Translator Plan

Oct 7, 2026 · @Glavin

## Goal and target

We run unmodified 32-bit Windows games in a browser by translating both the game and Wine's own DLLs from x86 to WebAssembly, caching the result, and running it on a thin native layer that maps Windows onto browser APIs and WebGPU.

- **Local only.** The user points the browser at a folder holding their own copy of the game. Translation, caching and play all happen on their machine; nothing is uploaded.
- **Rosetta-style translation.** Ahead of time first. Code the first pass missed is translated on the spot when it runs, then folded into the cache for next launch.
- **Wine is translated, not ported.** Wine's Windows-side DLLs are ordinary x86 DLLs, so the same translator converts them once at build time and we ship the result. Only Wine's narrow Unix side is rewritten or compiled natively.
- **One shared memory.** The game, translated Wine and the native layer share one WebAssembly memory laid out like a Windows process, so a pointer means the same thing everywhere.
- **Scope for v1:** 32-bit, single-player, no DRM, Chromium first. 64-bit programs follow on a second stack ([64-bit programs](#64-bit-programs)). Graphics covers normal window and GDI rendering only; Direct3D is disabled for now (problem 11).
- **Delivery order:** a local command-line toolkit that turns an .exe into a web app comes first; the same translator library then moves into the browser.

## How everything connects

Wine's DLLs are translated once on our CI and the game on first launch; at run time both live as translated code in the guest region, and only Wine's system calls cross the dashed line into the native region, which talks to the browser.

&#91;embedded content: system architecture · build time, first launch, runtime, browser\]

## Layers of conversion

One Rust translator library turns x86 machine code into WebAssembly in seven layers. It runs as a command-line tool (build time and early milestones) and inside the browser (user's machine), with two modes: full optimization ahead of time, and a fast mode for code discovered while running.

&#91;embedded content: translation pipeline · 7 layers and the run-time loop\]

1. **Load.** Parse the PE file (.exe or .dll): sections, imports, exports, relocations, TLS callbacks. Place it at its preferred address in the shared memory map.
2. **Discover code.** Walk from the entry point, exports and TLS callbacks; follow calls and jumps; use the relocation table to find code pointers stored in data (vtables, jump tables). Output: a list of functions and their basic blocks.
3. **Decode.** `iced-x86` turns bytes into instructions.
4. **Lift to our intermediate form.** Each instruction becomes explicit operations on registers, flags and memory, in a small typed form we own. This is where x86 semantics live, ported from v86 and checked against real hardware.
5. **Optimize.** Remove flag calculations nobody reads, fuse compare-and-branch, keep registers in WebAssembly locals for the whole function, fold address arithmetic, drop redundant memory checks.
6. **Generate WebAssembly.** One WebAssembly function per x86 function; branches and loops rebuilt as structured control flow; calls between known functions become direct calls; everything else goes through a lookup table. Emitted with `wasm-encoder`; in ahead-of-time mode, Binaryen's optimizer runs over the result.
7. **Cache and link.** One module per translated binary, stored as bytes in the game folder or browser storage. At launch, modules are compiled, given the shared memory and function table, and their functions registered under their x86 addresses.

At run time, a jump to an x86 address with no registered function triggers the fast mode for that code, registers it, and records the address so the next ahead-of-time pass includes it.

## Runtime: shared memory and boundaries

All game and Wine code runs in Web Workers over one shared WebAssembly memory whose low region is the Windows process, mapped one-to-one: x86 address `0x401000` is WebAssembly address `0x401000`.

| Address range | Holds | Written by |
| --- | --- | --- |
| `0x00000000`–`0x0000FFFF` | Nothing. Any access is a null-pointer access violation | No one |
| `0x00010000`–guest limit | Windows process: game image, Wine DLL images, heaps, thread stacks, TEB and PEB | Translated code; native layer through Wine's memory calls |
| Guest limit–top | Native runtime: Emscripten data, heap and stacks, CPU state per thread, translator and caches | Native layer only |

The guest limit is configurable. 32-bit Windows gives programs at most the low 2 GB (the top half belongs to the kernel), so 2 GB is the ceiling; start at 1 GB to keep the browser's memory reservation small, and raise it per game.

**The boundaries, and what crosses each:**

- **Game → Wine.** No boundary. Both are translated x86 code in the same memory; imports are resolved at translation time where possible, otherwise through the address lookup table.
- **Wine → native layer.** Wine's own system-call layer: NT calls from `ntdll` (files, memory, threads, synchronization), `win32u` calls (windows, drawing), and per-DLL "unix calls" (audio, graphics). Arguments are already valid pointers into the shared memory, and both sides are 32-bit, so no structure conversion is needed.
- **Native layer → Wine and game.** Callbacks such as window procedures and exception dispatch use Wine's existing user-mode callback path into translated code.
- **Worker → main thread.** For DOM work, the worker posts a request and blocks on `Atomics.wait`; the main thread does the work and wakes it. Buffers pass by pointer, without copies (the pattern Theseus uses).
- **Worker → GPU.** WebGPU runs inside the worker through `OffscreenCanvas`, so frames never wait on the main thread.

## The hard problems

Eleven problems decide whether this works; the first eight live in the translator, the rest at the edges. Each lists why it is hard, our approach, and where to learn from.

**Confidence at a glance.** High means proven techniques that already ship elsewhere; medium means proven techniques with an unconfirmed fact or an unmeasured cost; low means new ground. Sections marked *not yet verified* rest on design reasoning and still need sourced research.

| # | Problem | Confidence | Biggest unknown | Known by |
| --- | --- | --- | --- | --- |
| 1 | Finding all the code | High (medium for first-launch coverage) | Share found statically in old games | M3 |
| 2 | Control flow | High | Speed of indirect tail calls | M1 |
| 3 | Registers | High (medium for speed) | Cost of write-backs | M7 |
| 4 | Flags | High (medium for speed) | Flag work left after elimination | M1 |
| 5 | Memory layout and self-modifying code | Medium | Browser shared-memory size; Emscripten above the guest limit | M1 |
| 6 | x87 and SIMD | Medium-high | Per-game precision bugs | M6 |
| 7 | Faults and exceptions | Medium-high | Cost of explicit checks | M5 |
| 8 | Wine boundary and wineserver | Medium | Size of the port; wineserver speed | M2, M4 |
| 9 | Threads and memory ordering | Medium-high (medium on ARM) | Strict-mode cost on ARM | M5 |
| 10 | Module size and caching | Medium | Caching compiled code for generated modules | M3 |
| 11 | Graphics | Deferred (medium-low) | Synchronous readback; scale of new code | When Direct3D is re-enabled |

### 1. Finding all the code

**Why hard:** x86 binaries mix code and data, and jump tables, vtables and function pointers hide targets. No static analysis is guaranteed to find everything.

- Walk from entry point, exports and TLS callbacks; recognize compiler jump-table patterns.
- Use the PE relocation table to find code addresses stored in data. DLLs always have one; many old .exe files had it stripped.
- Anything missed is caught at run time, translated in fast mode, and added to the next ahead-of-time pass. This replaces the per-program manual help Theseus accepts.
- **Learn from:** [Theseus](https://neugierig.org/software/blog/2026/04/theseus.html) (static translation, its limits); [v86](https://github.com/copy/v86/blob/master/docs/how-it-works.md) (its interpreter records call and indirect-jump targets).

**Solution detail**

- **Game executables usually have no relocation table.** MSVC links .exe files with `/FIXED` by default, which omits it ([Microsoft](https://learn.microsoft.com/en-us/cpp/build/reference/fixed-fixed-base-address)); `/DYNAMICBASE` only became the default around Visual Studio 2010 ([Microsoft](https://devblogs.microsoft.com/cppblog/dynamicbase-and-nxcompat/)). For game .exe files we combine:
  - recursive descent from the entry point, exports and TLS callbacks;
  - MSVC switch-table patterns with bounded index ranges, as rev.ng does ([CC'17](https://www.nebelwelt.net/files/17CC.pdf));
  - scanning `.rdata` and `.data` for values that land on valid instruction starts (vtables, callback tables), each confirmed by decoding.
- **DLLs, including all of Wine's, keep relocations,** so every relocation pointing into code is a seed (Chrome's Courgette uses the same trick).
- **Misses are expected, and cheap.** Ghidra finds only 41% of function entries in fully optimized MSVC binaries, 83% unoptimized ([SoK, S&P'21](https://arxiv.org/pdf/2007.14266)). As in FX!32 and Rosetta 2, missed code runs through fast mode, is written to a per-game profile, and seeds the next ahead-of-time pass ([FX!32](https://web.stanford.edu/class/cs343/resources/fx32.pdf), [Rosetta 2](https://dougallj.wordpress.com/2022/11/09/why-is-rosetta-2-fast/)).
- **Packed executables need nothing extra:** unpacked code reaches fast mode. Theseus's author did this by hand ([Theseus unpacking](https://neugierig.org/software/blog/2026/04/theseus-unpack.html)).

**Confidence: high** that all executed code gets translated, since fast mode guarantees coverage; **medium** on how much the first ahead-of-time pass finds, which decides first-launch stutter. **We'll know in M3** by measuring statically found code across 20 games, before and after one profile round.

### 2. Arbitrary jumps to structured WebAssembly

**Why hard:** WebAssembly has only nested blocks and loops, no goto, no jump to a computed address, and control can only move between functions by calling.

- One WebAssembly function per x86 function; branches and loops rebuilt with a stackifier (Binaryen ships a relooper).
- Known calls become direct calls. Indirect calls and jumps look up the x86 address in a table, then use `call_indirect`.
- `ret` becomes a return after checking the return address on the x86 stack matches; on mismatch (push/ret tricks, `longjmp`, exceptions), fall back to a dispatcher loop.
- Jumps between functions use WebAssembly tail calls (standardized) or the dispatcher.
- **Learn from:** [CheerpX, Extreme WebAssembly 1](https://labs.leaningtech.com/blog/extreme-webassembly-1-pushing-browsers-to-their-absolute-limits) (direct jumps to control flow, calls to calls); the stackifier write-up linked from v86's doc; [v86](https://github.com/copy/v86/blob/master/docs/how-it-works.md) on why one giant `br_table` per page made browsers struggle.

**Solution detail**

- **Binaryen's Relooper builds each function's structure.** Loops with several entry points get a label variable and a dispatch, without duplicating code; LLVM's WebAssembly backend does the same ([Relooper](https://github.com/WebAssembly/binaryen/blob/main/src/cfg/Relooper.h), [LLVM pass](https://github.com/llvm/llvm-project/blob/main/llvm/lib/Target/WebAssembly/WebAssemblyFixIrreducibleControlFlow.cpp)).
- **Tail calls ship everywhere we target:** Chrome 112, Firefox 121, Safari 18.2 ([feature table](https://webassembly.org/features/)). Jumps between functions and fast-mode blocks use `return_call`/`return_call_indirect`, so chained code never grows the stack.
- **Function size is capped,** splitting huge x86 functions at block boundaries, since v86 found very large functions strain browser compilers.
- **Returns check their target:** on mismatch with the expected x86 return address, fall back to an address lookup, keeping `push`/`ret` tricks, `longjmp` and unwinding correct.

**Confidence: high.** Every piece already ships. **Open:** the speed of `return_call_indirect` per engine, unpublished; M1 includes a micro-benchmark.

### 3. Registers and CPU state

**Why hard:** WebAssembly locals vanish at function boundaries, so x86 registers can't stay in host registers across calls.

- CPU state lives in a per-thread struct in the native region.
- Inside a function, registers live in locals; they are written back only at calls, exits and fault points, guided by liveness from the optimizer.
- **Learn from:** v86 (state kept in memory, same limitation); Theseus (its `ctx` register struct in generated code).

**Solution detail** *(design reasoning; not yet researched)*

- Inside a function, registers live in WebAssembly locals, which engines put in host registers. They are written back to the CPU-state struct only at calls, exits and possible fault points, using liveness from the optimizer.
- Later option: pass the hottest registers (`esp`, `eax`, `ecx`, `edx`) as function parameters and return them with multi-value returns, so calls between translated functions skip memory entirely.

**Confidence: high** for correctness (v86 already keeps state in memory); **medium** for speed. **We'll know in M7** by profiling write-back traffic.

### 4. Flags

**Why hard:** most arithmetic sets up to six flags, including parity. Computing them all is the biggest single overhead, and some flags are "undefined" and differ between Intel and AMD.

- Compute only flags a later instruction reads; fuse compare-and-branch into one WebAssembly comparison.
- When flags escape a function, store the operation and operands and compute flags lazily.
- Tests mask undefined flags per instruction.
- **Learn from:** v86 (lazy flags, compare-and-branch fusion, open work on eliding updates); Theseus (lets the compiler remove unneeded flag work).

**Solution detail**

- **Whole-function liveness, ahead of time:** compute only flags something reads. QEMU removes unused flag assignments this way ([Bellard 2005](https://www.usenix.org/legacy/event/usenix05/tech/freenix/full_papers/bellard/bellard_html/index.html)); FEX tracks flag liveness across blocks ([FEX pass](https://github.com/FEX-Emu/FEX/blob/main/FEXCore/Source/Interface/IR/Passes/RedundantFlagCalculationElimination.cpp)).
- **Compare-and-branch becomes one WebAssembly comparison.**
- **Escaping flags are stored lazily** (operation plus operands, QEMU's `CC_OP` scheme) where live at calls, indirect jumps, returns or possible faults.
- **Cheap encodings from FEX:** parity kept as the result's low byte, auxiliary carry as the XOR of the operands ([FEX Flags.cpp](https://github.com/FEX-Emu/FEX/blob/main/FEXCore/Source/Interface/Core/OpcodeDispatcher/Flags.cpp)).

**Confidence: high** for correctness, **medium** for speed. FEX gained 17.6% on Geekbench, and over 2× in some games, just from keeping flags in ARM's flag register ([FEX-2312](https://fex-emu.com/FEX-2312/)); WebAssembly has no flag register, so elimination matters even more, and no WebAssembly measurements exist. **We'll know in M1** by counting surviving flag computations in Csmith programs.

### 5. Memory, segments and self-modifying code

**Why hard:** WebAssembly has no page protection or `mmap`. Windows uses the FS segment for per-thread data (the exception chain lives at `FS:[0]`). Programs can write code and then run it.

- One-to-one mapping makes a guest load a plain WebAssembly load at the same address; unaligned access and little-endian order match x86.
- FS-prefixed accesses add a per-thread base address.
- Null-region checks only where the translator can't prove an address is valid.
- A bitmap marks pages that hold translated code. Stores that might hit those pages are checked; a hit discards the translation and falls back to fast mode.
- **Learn from:** v86 (marks code pages to invalidate on write; its TLB path is what our mapping removes); Theseus's [WebAssembly notes](https://neugierig.org/software/blog/2026/05/theseus-wasm.html) (it keeps x86 memory separate and calls one-to-one mapping optional; we need it so native Wine code can read guest pointers directly). Page protection in WebAssembly is still a [Phase 1 proposal](https://github.com/webassembly/proposals), so don't wait for it.

**Solution detail** *(not yet verified by research)*

- Declare one shared memory with a fixed maximum (guest limit plus native region), and link Emscripten's data, stack and heap above the guest limit with a custom base address. Pointers above 2 GB have historically needed care in Emscripten's JavaScript glue.
- Self-modifying code: one bit per 4 KB page marks pages holding translated code. Only stores the translator can't prove are to the stack or data check the bitmap; a hit discards that page's translations.
- Null checks are emitted only where an address can't be proven valid.

**Confidence: medium.** The techniques are standard, but two facts are unconfirmed: how large a shared memory each browser allows, and whether Emscripten can be placed entirely above 1–2 GB. **Both are M1 spikes,** first because the whole memory design rests on them.

**Sourced update.** Chrome allows WebAssembly memories up to 4 GB, the most 32-bit pointers can reach, but Emscripten caps at 2 GB unless built with `-sMAXIMUM_MEMORY=4GB`, and its JavaScript glue must use unsigned shifts so addresses above 2 GB don't turn negative ([V8](https://v8.dev/blog/4gb-wasm-memory)). Staying on 32-bit memory is also right for speed: browsers reserve the full 4 GB so bounds checks vanish, while 64-bit memory runs 10% to over 100% slower ([SpiderMonkey](https://spidermonkey.dev/blog/2025/01/15/is-memory64-actually-worth-using.html)). Still open: Safari's practical limit for large shared memories, and whether Emscripten's data, stack and heap can all be placed above the guest limit.

### 6. x87 floating point and SIMD

**Why hard:** x87 is an 80-bit register stack with its own precision and rounding modes; WebAssembly has only 32- and 64-bit floats.

- Run x87 on 64-bit floats by default; Direct3D 9 normally switches the FPU to single precision anyway.
- Keep an 80-bit SoftFloat path as a per-game setting.
- Track the x87 stack top statically inside a function where possible.
- MMX and SSE/SSE2 map onto WebAssembly's 128-bit SIMD.
- **Learn from:** v86 (SoftFloat x87, SSE); Theseus (FPU and MMX in a working game); FEX later for AVX.

**Solution detail**

- **Direct3D 9 games mostly run the FPU in single precision:** unless a game passes `D3DCREATE_FPU_PRESERVE`, Direct3D 9 sets single precision, round-to-nearest ([Microsoft](https://learn.microsoft.com/en-us/windows/win32/direct3d9/d3dcreate)). Default x87 math uses 64-bit floats, like FEX's reduced-precision mode, with 80-bit SoftFloat per game ([FEX-2206](https://fex-emu.com/FEX-2206/)).
- **WebAssembly always rounds to nearest,** with no rounding control, exception flags or flush-to-zero ([spec](https://webassembly.github.io/spec/core/exec/numerics.html)). So `FIST`/`FISTP` and `CVTSS2SI`/`CVTSD2SI` branch on the current rounding bits and return x86's "integer indefinite" when out of range; the rare function doing arithmetic in another rounding mode falls back to SoftFloat.
- **Bit-exact copies stay exact.** `FILD`/`FISTP` of 64-bit integers (an old copy trick) loses bits in a 64-bit float, which pushed felix86 to SoftFloat by default ([felix86](https://felix86.com/felix86-26-08/)). We tag such stack slots as raw integers until arithmetic touches them; same for 80-bit `FLD`/`FSTP`.
- **SIMD** ships in Chrome 91, Firefox 89, Safari 16.4. Gaps ([Emscripten](https://emscripten.org/docs/porting/simd.html)): `RCPPS`/`RSQRTPS` give exact results (tests allow tolerance; Intel and AMD differ too); `PSADBW`, `PMULHW`, `CVTPS2DQ` need short sequences; `MXCSR` rounding is handled like x87.

**Confidence: medium-high.** All techniques are proven; the risk is per-game precision bugs, handled by per-game settings and caught by screenshot tests in M6.

### 7. Faults and Windows exceptions

**Why hard:** Windows exception handling depends on CPU faults (access violation, divide by zero, breakpoint, illegal instruction). WebAssembly doesn't fault on a bad in-range address, and aborts outright on integer divide by zero.

- The translator emits explicit checks: a zero test before `div`/`idiv`, null-region checks, and direct raises for `int3` and `ud2`.
- Raising saves CPU state into a Windows `CONTEXT` record and calls translated Wine's exception dispatcher, which walks the `FS:[0]` handler chain exactly as on Windows.
- Resuming at a handler unwinds WebAssembly frames with WebAssembly exception handling (standardized), back to the dispatcher, which continues at the handler's x86 address.
- **Learn from:** Wine's i386 exception code in `ntdll`; [Hangover](https://github.com/AndreRH/hangover) and Wine's WoW64 path for exceptions raised from emulated code.

**Solution detail** *(not yet verified by research)*

- WebAssembly traps (divide by zero, `unreachable`) can't be caught inside WebAssembly, so the translator must test before every `div`/`idiv` and raise the Windows exception itself.
- Raising: save state into a Windows `CONTEXT`, then call translated Wine's own dispatcher, which walks the `FS:[0]` chain exactly as on Windows. Unwinding to a handler uses WebAssembly exception handling, which all major browsers support.
- The Wine side of this lives in its i386 architecture code (`signal_i386.c`), which our port replaces; see problem 8.

**Confidence: medium-high.** Wine's dispatcher does the hard part unchanged. **We'll know in M5** by passing Wine's exception tests.

**Sourced update.** The modern form of WebAssembly exception handling (`exnref`) ships in Chrome 137, Firefox 131 and Safari 18.4 ([Can I use](https://caniuse.com/wf-wasm-exnref-exceptions)), so unwinding to a Windows handler has native support in every target browser.

### 8. The boundary to native code, and callbacks

**Why hard:** every crossing must agree on calling convention (stdcall, cdecl, thiscall, fastcall), structure layout and variadic arguments, and callbacks run in the opposite direction.

- Put the boundary at Wine's own system-call layer: a few hundred narrow, well-defined entry points. Game-to-Wine calls never cross it.
- Generate thunks from Wine's system-call tables rather than writing them by hand.
- Callbacks reuse Wine's user-mode callback mechanism.
- When a hot DLL is later compiled natively (graphics first), generate its thunks from headers.
- **Learn from:** Hangover (leaves emulation at the Win32 and Wine system-call level); [FEX thunks](https://newreleases.io/project/github/FEX-Emu/FEX/release/FEX-2208) (guest versus host pointers, variadic calls); [retrowin32](https://neugierig.org/software/blog/2022/10/retrowin32.html) (marker addresses for API calls); Theseus (resolves the import table at translation time); [Boxedwine](https://github.com/danoon2/Boxedwine) (puts the boundary at Linux system calls, the lower, wider boundary we avoid).

**Solution detail** *(not yet verified by research)*

- **Port Wine to a new host architecture.** Wine keeps CPU-specific code (syscall dispatcher, user callbacks, exception raising, thread context, thread start) in per-architecture files such as `signal_i386.c`. We write the equivalent for our translator; everything else in Wine's Unix side compiles with Emscripten.
- **Newly identified: wineserver.** Wine keeps handles, synchronization objects, processes, threads and window state in a separate server process, reached over Unix sockets with file-descriptor passing. In the browser it runs in its own worker, compiled with Emscripten, and our browser OS layer must provide those sockets and fd passing between workers. Boxedwine already runs the real wineserver inside its emulated Linux, which shows the approach works once that layer exists.
- Thunks are generated from Wine's syscall tables, not written by hand.

**Confidence: medium.** Every piece has a precedent, but this is the largest body of new code, and wineserver round-trips may cost speed. **We'll know in M2** (first syscalls) **and M4** (windows and messages).

### 9. Threads, blocking and memory ordering

**Why hard:** the browser's main thread can't block, but Windows code blocks constantly (`GetMessage`, `WaitForSingleObject`, `Sleep`). x86 guarantees strong memory ordering; ordinary WebAssembly loads and stores don't.

- Every Windows thread is a Web Worker on the shared memory; no guest code runs on the main thread.
- Blocking uses `Atomics.wait`; the main thread only serves DOM, input and audio setup.
- `lock`-prefixed instructions and `xchg` become WebAssembly atomic operations; plain accesses stay plain, with a per-game strict mode that makes them atomic.
- Requires the page to be served cross-origin isolated (COOP and COEP headers).
- **Learn from:** [Theseus WebAssembly notes](https://neugierig.org/software/blog/2026/05/theseus-wasm.html) (workers, `Atomics.wait`, zero-copy buffers; Rust's standard library needs a nightly rebuild for atomics); Emscripten pthreads; Boxedwine's multithreaded web build; FEX, which defaults to [conservative ordering emulation](https://ostechnix.com/fex-emu-run-x86-and-x86-64-apps-on-arm64-linux-devices/) at a speed cost.

**Solution detail** *(not yet verified by research)*

- Threads use Emscripten pthreads (one worker each) with a pre-started worker pool, so `CreateThread` doesn't wait on worker startup.
- **Memory ordering is fine on x86 machines** (plain WebAssembly accesses compile to plain x86 moves), **but not on ARM machines** such as Apple silicon Macs, where racy plain accesses can reorder. The per-game strict mode makes guest accesses atomic there, at a speed cost; FEX makes the same trade-off on ARM.

**Confidence: medium-high** on x86 PCs; **medium** on ARM machines for multithreaded games. **We'll know in M5** by running threaded stress tests on both.

### 10. Module size, compile time and caching

**Why hard:** browsers compile every module before it runs, generation is slow, and each module costs memory; v86 found it can't make more than a few thousand.

- One large module per translated binary, compiled with streaming compilation and cached as bytes.
- Wine's DLLs are pre-translated and shipped; each is loaded only when the game first needs it.
- Code found at run time goes into small batched modules, merged into the next ahead-of-time build.
- **Learn from:** [v86](https://github.com/copy/v86/blob/master/docs/how-it-works.md) (module limits); the [QEMU WebAssembly backend](https://lists.libreplanet.org/archive/html/qemu-arm/2025-04/msg00205.html) (evicts old instances to avoid browser errors); a [unicorn.js discussion](https://github.com/AlexAltea/unicorn.js/issues/16) of Chrome reserving 4 GiB per module.

**Solution detail** *(not yet verified by research)*

- Cache module bytes on disk and lean on the browser's fast baseline compiler for startup; optimized tiers compile in the background.
- Translated Wine DLLs ship as static files, so the browser's normal compiled-code cache can apply to them.

**Confidence: medium.** Whether a page can cache compiled code for modules it generates itself is unconfirmed; if not, every launch pays baseline compile time for game modules. **We'll know in M3** by measuring launch time for a large game.

**Sourced update.** Chrome caches compiled WebAssembly only for modules loaded with `compileStreaming`/`instantiateStreaming` from an HTTP fetch, keyed by URL, for files of 128 kB or more, after the optimizing compiler finishes, with about 150 MB of cached code at most ([V8](https://v8.dev/blog/wasm-code-caching)). So shipped Wine DLLs get cached for free, while game modules we generate locally probably don't; serving them through a service worker under a stable URL is the experiment to try in M3. Compiled code is 5–7× the size of the module, so the 150 MB cap matters for big games.

### 11. Graphics and shaders (deferred: Direct3D disabled)

**Status: out of current scope.** Current scope is normal rendering: windows, GDI drawing, and 2D DirectDraw through `wined3d`'s software renderer. Direct3D stays disabled until we pick this up; the notes below are the plan for then.

**Why hard:** DirectX's state machine and shader bytecode must map onto WebGPU, which has no geometry shaders, tessellation or fixed-function pipeline.

- Add a WebGPU backend to Wine's `wined3d`, compiled natively to WebAssembly.
- Shaders: Direct3D bytecode to SPIR-V with `vkd3d-shader`, then SPIR-V to WGSL with Naga or Tint.
- Fixed-function rendering becomes generated shaders, as `wined3d` already does for its other backends.
- **Learn from:** `wined3d`'s Vulkan backend; DXVK for state tracking and pipeline caching.

**Solution detail** *(not yet verified by research)*

- `wined3d` already funnels all rendering through one command-stream thread, which maps cleanly onto one worker that owns the WebGPU device (WebGPU objects can't be shared between workers).
- Missing WebGPU features are emulated: point sizes with quads, triangle fans converted to lists, unsupported 16-bit and paletted texture formats converted on upload, alpha test and fog in shaders (as `wined3d` already does).
- **The hard one: synchronous readback.** Direct3D 9's `Lock` on render targets and occlusion queries block, but WebGPU's readback is asynchronous. The rendering worker can block the caller with `Atomics.wait` while it services the promise, or use JSPI where available.

**Confidence: medium-low,** the lowest in this plan: it is the most new code, with no existing WebGPU backend for Direct3D 9 to learn from. **We'll know in M6,** or sooner with a readback spike once Direct3D is back in scope.

**Sourced update.** Synchronous WebGPU waits from WebAssembly already exist: Dawn's Emscripten bindings implement `wgpuInstanceWaitAny` on top of Emscripten's async support ([Dawn change](https://dawn.googlesource.com/dawn/+/63cbc06bd56d57488b5234269eee41414e2583fd)), and Flax Engine moved its WebGPU readback from Asyncify to JSPI ([Flax commit](https://git.flaxengine.com/Flax/FlaxEngine/commit/a5ec8565e4bdd2408a3cdeee587c8f65364018a9)). That lifts readback from an open question to a known pattern.

## Reference map

We fork upstream Wine and build our own translator; everything else is a reference, a test oracle or a library, never a base to fork.

| Project | Use it for | Don't use it for |
| --- | --- | --- |
| Wine (upstream) | The Windows API, translated; its Unix side, compiled; its conformance tests | Proton's fork as a base (Linux-specific, harder to track) |
| [v86](https://github.com/copy/v86/blob/master/docs/how-it-works.md) (BSD) | Instruction semantics, lazy flags, SSE, x87, its generated instruction tests | Its architecture: emulated paging, page-sized JIT units, one giant dispatch function |
| [Theseus](https://github.com/evmar/theseus) | Static translation design, import resolution at translation time, the worker and `Atomics` host pattern | Forking: it emits Rust (needs a compiler), and no license is listed |
| [retrowin32](https://github.com/evmar/retrowin32) | How API calls are intercepted; lessons on async versus blocking | API coverage |
| [CheerpX blog](https://labs.leaningtech.com/blog/extreme-webassembly-1-pushing-browsers-to-their-absolute-limits) | Mapping x86 control flow to WebAssembly | Code (closed source) |
| [Hangover](https://github.com/AndreRH/hangover) | Where to cut between emulated and native code; exceptions and callbacks across it | Direct reuse (targets native ARM) |
| FEX-Emu | Optimization passes, x86-64 and AVX behavior, memory-ordering options, its assembly tests | Code reuse in the browser (native ARM JIT) |
| [Boxedwine](https://github.com/danoon2/Boxedwine) (GPLv2) | Browser host tricks; a baseline to beat | Its architecture (interprets all of Wine below a fake Linux kernel) |
| `iced-x86` | Decoding and disassembly, 32- and 64-bit | — |
| `wasm-encoder`, Binaryen | Emitting and optimizing WebAssembly; Binaryen's relooper | — |
| Real x86 hardware (CI) | The source of truth for every test | — |
| Unicorn | Fast per-instruction comparison while developing | Final truth (QEMU-based, has gaps) |
| Remill + LLVM | Later: a performance ceiling and second opinion | A byte-identical target; anything in the browser |
| [QEMU WebAssembly backend](https://lists.libreplanet.org/archive/html/qemu-arm/2025-04/msg00205.html) | Practical lessons on module and instance limits | Code generation design (JIT-first, lightly optimized) |
| Csmith, YARPGen, GCC torture tests, `cvise` | Test programs and automatic shrinking of failures | — |
| `wined3d`, DXVK, `vkd3d-shader`, Naga or Tint | Milestone 6 graphics and shader translation | Vulkan code paths directly |

## Verification pipeline

Correctness is judged by behavior, never by matching WebAssembly bytes: two correct translators emit different code, so what must match is registers, flags, memory and output after running.

**Sources of truth, strongest first:**

1. **Real x86 CPUs.** CI runners on x86 (GitHub Actions' Windows and Linux runners) run every test natively and record the expected results as fixtures. Development then works on any machine, including Apple silicon.
2. **Unicorn,** for quick per-instruction checks while coding. Real hardware wins any disagreement.
3. **Remill + LLVM compiled to WebAssembly,** added later as a speed ceiling, not a correctness oracle.

| Layer | What runs | Compared against | Catches |
| --- | --- | --- | --- |
| 1. Instructions | Random inputs for every instruction form | Results recorded on real CPUs, undefined flags masked | Wrong semantics for single instructions |
| 2. Programs | GCC torture tests; Csmith and YARPGen random programs; built with MinGW, clang-cl and old MSVC at every optimization level | The native run's output and checksum | Optimizer and control-flow bugs |
| 3. Windows API | Wine's conformance tests through translated Wine | Wine's expected results, which also run on real Windows | Boundary, threading and exception bugs |
| 4. Real software | `winemine`, `notepad`, demos, then games | Screenshots and frame timing from native Windows | Integration bugs, speed regressions |
| 5. Our own output | The translator on a fixed set of binaries | Stored snapshots (`insta`) | Unintended code-generation changes |

**When a test fails, the pipeline does the debugging:**

1. A debug build dumps CPU state at every basic block; the same trace is recorded on the reference; the diff names the first wrong instruction.
2. `cvise` shrinks the failing C program to the smallest one that still fails.
3. The shrunk program becomes a permanent regression test.
4. Separately, `cargo-fuzz` feeds random code to the decoder and translator, with Unicorn or hardware as the oracle.

CI records speed for every test too, so a slowdown fails the build like a wrong answer does.

## Milestones

Each milestone builds a permanent part of the target and adds its test layer; M2 is the one that proves the architecture, so get there fast.

1. **M1 — Translator core and test harness.** Rust workspace; `iced-x86` → intermediate form → `wasm-encoder`; lazy flags; one WebAssembly function per x86 function. Test layers 1 and 5, with CI recording results on x86 runners. A spike confirming Emscripten can keep its data, heap and stacks above the guest limit.
   - **Done when:** a tiny hand-built .exe runs correctly in Node, and the instruction suite passes for everything MinGW emits.
2. **M2 — Wine boots, translated.** Translate Wine's `ntdll`, `kernelbase` and `kernel32`; implement the minimal NT calls they need (memory, files in an in-memory file system, one thread). Test layer 2.
   - **Done when:** a console `hello.exe` prints through translated Wine in a browser with no interpreter, and GCC torture and Csmith runs pass.
3. **M3 — Fully in the browser.** Translator compiled to WebAssembly; folder picker; fast-mode translation for missed code; cache. Test layer 3 starts with Wine's `kernel32` tests.
   - **Done when:** picking a folder in Chrome runs a console program, and the second launch skips translation.
4. **M4 — Windows on screen.** `win32u`'s Unix side compiled with Emscripten; a browser display driver; keyboard and mouse.
   - **Done when:** Wine's `winemine` and `notepad` run, and `user32` and `gdi32` test pass rates are tracked.
5. **M5 — Threads, audio, timing, exceptions.** A worker per Windows thread; synchronization; DirectSound and `winmm` on `AudioWorklet`; high-resolution timers; full exception dispatch.
   - **Done when:** a 2D DirectDraw game is playable with sound.
6. **M6 — Direct3D 9 on WebGPU (deferred; Direct3D disabled until then).** `wined3d` WebGPU backend compiled natively; shader translation. Test layer 4 with screenshot comparison.
   - **Done when:** the flagship game is playable. Set the M7 frame-rate target here.
7. **M7 — Speed.** Profile, then register promotion, flag elimination, SIMD, and native builds of the hottest Wine DLLs.
   - **Done when:** the flagship game meets the frame-rate target set in M6.
8. **M8 — Product.** Installer handling, a compatibility database, per-game settings, a hosted mode for games whose licenses allow it, Firefox and Safari fallbacks.

After v1: x86-64 on 64-bit WebAssembly memory (below, started early), then DirectX 10 and 11.

## 64-bit programs

**Decision: two stacks, one translator.** 32-bit programs keep their stack unchanged (32-bit memory, Wine's i386 DLLs, the pure i386 system-call path). 64-bit programs get a second one: x86-64 code, Wine's x86_64 DLLs translated, Wine's Unix side built for wasm64, and a 64-bit (memory64) WebAssembly memory, so both halves of Wine share a pointer size and no structure conversion is needed. The PE machine field picks the stack per process. Status: [milestone-9.md](milestone-9.md).

**Why not the alternatives.**

| | Two stacks (chosen) | One WoW64 stack | x86-64 in a 32-bit memory |
| --- | --- | --- | --- |
| How | Separate 32-bit and 64-bit Wine; one translator with a mode | Upstream Wine's and [Hangover](https://github.com/AndreRH/hangover)'s model: one 64-bit Wine, 32-bit programs through `wow64.dll` with our translator as its CPU backend | Every guest address below 4 GB in today's memory |
| 32-bit speed | Unchanged | Pays the memory64 cost | Unchanged |
| 64-bit speed | memory64 cost | memory64 cost | Same as 32-bit |
| New work | A second Wine build and Unix side | Replaces the working M1–M4 path | A hand-written 64-to-32-bit conversion at every system call, Unix call and graphics call |

[Boxedwine64](https://github.com/0x07C0/Boxedwine64) makes the same choice: 32-bit unchanged, 64-bit behind a switch on `-sMEMORY64`.

**Memory64 cost decides the follow-ups.** Browsers bounds-check every memory64 access: V8 traps on a compare with a constant (13.0 and later), SpiderMonkey checks explicitly, reported at 10% to over 100% ([SpiderMonkey](https://spidermonkey.dev/blog/2025/01/15/is-memory64-actually-worth-using.html)). Chrome 133, Firefox 134 and Node 24 ship it; Safari has it behind a flag. The translator therefore keeps the address model separate from the mode: x86-64 code also runs on a 32-bit memory (guest below 4 GB, addresses checked and wrapped), and 32-bit code on a 64-bit memory. `tools/bench/mem64.sh` measures all four. If the cost is small, 32-bit programs can later move onto the 64-bit stack through WoW64 (one Wine); if it is large, the 32-bit-memory model becomes the default for 64-bit programs that fit in about 3.5 GB, with a 64-to-32-bit conversion layer at the Wine boundary.

**Code stays below 4 GB.** Images preferred above 4 GB are moved by their relocations (64-bit executables are relocatable unless linked with `/FIXED`), so x86 code addresses, the lookup table and the dispatcher stay 32-bit; data addresses are 64-bit.

**What x86-64 changes.** Easier: `.pdata` lists nearly every function (a discovery seed), one calling convention, 16 registers, SSE instead of x87. Harder: 8-byte pointers in every structure that crosses a boundary, and table-based exceptions, whose unwinder needs the stack pointer and saved registers exactly where the unwind data says at every call and possible fault (translated code keeps the guest stack real, so this holds as long as prologue saves are never optimized away).

| # | Milestone | Done when |
| --- | --- | --- |
| M9 | x86-64 translator and runtime on the M1 shims | x86-64 instruction suite and program tests pass on both memory models (done; [milestone-9.md](milestone-9.md)) |
| M10 | 64-bit Wine, console | `hello64.exe` prints through translated x86_64 Wine with its Unix side on wasm64 |
| M11 | 64-bit Wine, windows | x86-64 `winemine` and `notepad` run; a 64-bit program can start a 32-bit one (each in its own stack, one wineserver: its protocol already uses 64-bit pointer fields) |
| M12 | Exceptions and threads for x86-64 | C++ and SEH exceptions through `.pdata` unwinding; `cmpxchg16b` atomic; `RtlAddFunctionTable` honored by fast mode |
| M13 | 64-bit graphics, with the Direct3D workstream | A 64-bit game renders through the Direct3D layer built for wasm64 |
| M14 | Conditional: the 32-bit-memory model as the default for 64-bit programs that fit | Only if `mem64.sh` shows memory64 costs too much |

## Non-goals and risks

**Not doing:** forking Boxedwine or Theseus; porting Wine's Windows side to WebAssembly (Boxedwine's author [tried compiling Wine for Emscripten directly](https://groups.google.com/g/emscripten-discuss/c/4Qw8OOgTvu0/m/dPETA4AWBgAJ) and found it too much for one person); a WebGL path; a JIT-first design; byte-identical comparisons; and, in v1, 64-bit games, modern AAA titles, anti-cheat or getting around DRM.

| Risk | Checked in | Fallback |
| --- | --- | --- |
| Browsers refuse a shared memory as large as guest limit plus native region | M1 | Lower the guest limit; 1 GB covers most games of the era |
| Emscripten can't place its data, heap and stacks above the guest limit | M1 | Custom linker layout and allocator for the native layer |
| The Wine version we pin no longer supports its pure 32-bit system-call path | M2 | Reuse Wine's WoW64 thunk layer for structure conversion |
| Translated Wine DLLs are too large or slow to compile | M2 | Load each DLL on first use; cache compiled bytes |
| Translated Wine runs too slowly | M4 | Compile the hottest DLLs natively (planned for M7 anyway) |
| Folder picker exists only in Chromium browsers | M3 | Import the folder into browser storage elsewhere |
| Legal exposure of hosted mode | M8 | Local-only by default; check each game's license; legal advice before public launch |

## Sources

- [v86: how it works](https://github.com/copy/v86/blob/master/docs/how-it-works.md)
- [Theseus, a static Windows emulator](https://neugierig.org/software/blog/2026/04/theseus.html) · [Theseus: translating win32 to wasm](https://neugierig.org/software/blog/2026/05/theseus-wasm.html) · [Theseus repository](https://github.com/evmar/theseus)
- [retrowin32 introduction](https://neugierig.org/software/blog/2022/10/retrowin32.html) · [retrowin32 repository](https://github.com/evmar/retrowin32)
- [CheerpX: Extreme WebAssembly 1](https://labs.leaningtech.com/blog/extreme-webassembly-1-pushing-browsers-to-their-absolute-limits)
- [Hangover](https://github.com/AndreRH/hangover) · [Boxedwine](https://github.com/danoon2/Boxedwine) · [Wine in Emscripten thread](https://groups.google.com/g/emscripten-discuss/c/4Qw8OOgTvu0/m/dPETA4AWBgAJ)
- [FEX-2208 release notes (thunks)](https://newreleases.io/project/github/FEX-Emu/FEX/release/FEX-2208) · [FEX-Emu overview](https://ostechnix.com/fex-emu-run-x86-and-x86-64-apps-on-arm64-linux-devices/)
- [QEMU WebAssembly TCG backend patch](https://lists.libreplanet.org/archive/html/qemu-arm/2025-04/msg00205.html) · [unicorn.js issue on per-module limits](https://github.com/AlexAltea/unicorn.js/issues/16)
- [WebAssembly proposals tracker](https://github.com/webassembly/proposals)
