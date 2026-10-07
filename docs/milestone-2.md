# Milestone 2 — translated Wine

Status as of October 7, 2026.

## Done-when criteria

| Criterion (from the plan) | Status | Evidence |
| --- | --- | --- |
| A console `hello.exe` prints through translated Wine in a browser, with no interpreter | Done | `node tests/web/browser.mjs tests/programs/hello.exe "Hello from translated x86!" --wine` (headless Chromium, three launches). Every instruction that runs — Wine's ntdll, kernelbase, kernel32, msvcrt and the program — is WebAssembly produced by the translator; the runtime has no interpreter. |
| GCC torture runs pass | Done | `gcc.c-torture/execute` (GCC 13 branch, 1,646 tests) at -O0 and -O2: 3,292 runs on the M1 shims, 3,244 pass, 38 skipped, 10 expected failures, 3,247 pass, 35 skipped and 10 expected failures on translated Wine. No unexpected failures; the expected failures are listed below with the reason for each. |
| Csmith runs pass | Done | On translated Wine: 120 runs, 108 pass, 12 skipped (the native reference does not finish in 10 s), no failures. On the M1 shims: 2,000 runs, no failures. |

## How Wine runs

**Build.** `tools/wine/build.sh` fetches Wine 11.0 and builds only the
Windows side for i386 (`--enable-archs=i386`): ntdll, kernelbase, kernel32,
msvcrt and ucrtbase as ordinary PE DLLs. Wine's Unix side is not built;
the runtime replaces it.

**The boundary is Wine's system call layer**, as the plan says. ntdll's
system call stubs are `mov eax, id; mov edx, __wine_syscall; call edx; ret
N`, and `__wine_syscall` jumps through `__wine_syscall_dispatcher`, a
pointer ntdll exports. The host (`runtime/wine/host.mjs`) stores the address
of a thunk there; the translated `call` lands in JavaScript with the
arguments on the guest stack, which are already valid pointers into the
shared memory. Unix library calls (`__wine_unix_call_dispatcher`) arrive
the same way; ntdll's debug output is the only one used so far.

**Boot.** The host does what Wine's Unix side does before the first
instruction: it maps ntdll and the program, builds the PEB, TEB, process
parameters, `KUSER_SHARED_DATA` and an initial `CONTEXT`, and enters
`LdrInitializeThunk`. From there Wine's own loader runs as translated code:
it resolves imports, maps kernel32 and kernelbase through `NtMapViewOfSection`
(the host maps each image together with its translated module), runs DLL
initialization and calls the entry point. Structure offsets come from
`runtime/wine/layout.json`, which `tools/wine-layout/gen.sh` generates by
running a small program against Wine's headers under this translator.

**System calls** (`runtime/wine/syscalls.mjs`, about 70): virtual memory
(`runtime/wine/vm.mjs`: reserve, commit, protect, query with 4 KB pages and
64 KB granularity), sections and image mapping, handles and events,
process, thread and system information, NLS tables, the registry (empty:
every key is reported missing, which Wine handles), files on a virtual
`C:` drive held in memory plus the console, `NtContinue`, and
`NtRaiseException` (stops the program with a description; SEH dispatch is
M5).

**Images that cannot load at their preferred base** (many of Wine's DLLs
share MinGW's default, 0x10000000) are rebased by the host: it applies the
base relocations to a copy, updates `ImageBase` so Wine's loader sees the
image as already in place, and translates the rebased copy.

**Memory.** The guest limit is 2 GB for Wine, since Wine's DLLs load
near the top of the lower 2 GB (ntdll at 0x7bc00000, kernel32 at
0x7b800000); the runtime's own data lives above it.

## In the browser

`runtime/node/wine-bundle.mjs` builds `target/wine-bundle`: Wine's DLLs,
their ahead-of-time translations, the NLS tables and a manifest. The page
(`runtime/web/`, "on Wine") fetches the bundle and compiles the DLL modules
with streaming compilation, which browsers cache by URL. It translates the
`.exe` in the browser with the translator compiled to WebAssembly and stores
the translation in the origin private file system, so later launches skip
translation. Code missed ahead of time is translated at run time (fast mode),
as in M1.

| On the development machine, headless Chromium | |
| --- | --- |
| Load the Wine bundle (5 DLLs, compiled) | ~320 ms |
| Translate `hello.exe` in the browser | ~30 ms |
| Process ready (boot through `LdrInitializeThunk`), first launch | ~420 ms |
| Same, cached translation | ~340 ms |

## Test results

* **Instructions**: the M1 groups plus SSE/SSE2/MMX (286 forms, 6,864
  cases, on WebAssembly SIMD), all passing in optimized and fast mode.
  m80 loads now draw special values (infinities, NaN, zeros) a quarter of
  the time: GCC torture found that an 80-bit infinity loaded as a NaN.
* **Programs**: the 7 hand-written programs at 5 optimization levels pass
  on both the shims and translated Wine (35/35 each).
* **GCC torture**: `node tests/programs/check.mjs --torture DIR [--wine]`,
  with `tools/torture/fetch.sh` fetching the tests at a pinned commit.
  Tests MinGW cannot build, or that fail natively with `gcc -m32`, are
  skipped. On the shims, runs that stop at an msvcrt function the M1 shims do
  not implement are skipped too; translated Wine has the real msvcrt. On
  Wine, tests that require a C99 runtime are skipped: msvcrt.dll is not one
  (it ignores `%hhd`, for example), on Windows or in Wine.

Expected failures (`tests/programs/torture-xfail.txt`), each checked by
reading the MinGW binary:

| Test | Reason |
| --- | --- |
| `signed1bitfield-1` (-O1 and up), `pr108789`, `pr111151`, `pr90348` (-O1 and up) | MinGW GCC 13 miscompiles them: they are regression tests for compiler bugs fixed in later GCC releases, and the `.exe` fails on Windows too. |
| `pr39228` | x87 registers are f64 (the plan's default), so `LDBL_MAX` rounds to infinity and `1.01L * LDBL_MAX > LDBL_MAX` is false. |
| `return-addr` | Prints stack addresses, which differ from the native run. |

## Speed: CoreMark

`tools/bench/coremark.sh` builds CoreMark 1.0 (EEMBC) with `gcc -m32 -O2`
and with MinGW `-O2` (the same GCC 13), runs the native build, and runs the
`.exe` translated. CoreMark validates its results and runs at least 10 s.
On the development machine (Xeon @ 2.1 GHz, Node 22 / Chromium):

| | Iterations/s | vs. native |
| --- | --- | --- |
| Native, `gcc -m32` | ~25,000 | 100% |
| Translated, M1 shims, Node | ~9,100–9,800 | ~37–39% |
| Same, without memory and code-write checks | ~9,900 | ~40% |
| Translated Wine, Node | ~9,300–9,400 | ~37% |
| Translated Wine, Chromium (browser page) | ~8,900–9,500 | ~36–38% |

Wine adds no measurable cost to compute-bound code: its DLLs are only on
the path for system calls. Raising this ratio is M7 work.

**Geekbench.** Geekbench 5 and 6 ship only 64-bit (and ARM64) benchmark
binaries, outside the plan's 32-bit scope. Geekbench 3 and 4 include 32-bit
Windows builds: Geekbench 3's translates (424,000 instructions in about
4 s) and loads into translated Wine with 24 of Wine's DLLs (several of
them rebased, which the host now does as Windows would), but user32's
initialization needs win32u's system calls (M4), its workloads run on
threads (M5), and tryout mode uploads results instead of printing them,
which needs network access from the guest.

## Known limitations and next steps

* Threads, SEH dispatch and window messages are M5 and later; a program
  that raises an exception stops with a description.
* The registry is empty, and files a program writes live in memory for the
  life of the process only.
* Wine's DLLs are translated ahead of time with no profile, so their
  callbacks and computed jumps reach fast mode the first time they run. The
  bundle should carry a profile collected from test runs.
* The lookup table costs 16 KB per code page, about 6 MB for the code of
  the four DLLs a console program loads; a sparser structure is still to do
  (M3).
