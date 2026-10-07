# Milestone 5 — threads, audio, timing, exceptions

Status as of October 7, 2026: in progress.

## Done-when criterion

A 2D DirectDraw game is playable with sound. The target is the original
freeware Cave Story (DirectDraw 7, DirectSound, its own music engine on a
multimedia timer), run from the folder picker; the test programs below
build up to it.

| Part | Status |
| --- | --- |
| Exceptions | Done (below) |
| Threads | In progress |
| Timers | To do |
| Audio (`winmm`, DirectSound on an `AudioWorklet`) | To do |
| DirectDraw (Wine's `ddraw` on `wined3d` with 3D off) | To do |

## Exceptions

A fault in translated code becomes a Windows exception that the program's
own handlers see, as on Windows:

* **Faults.** The translator checks before `div`/`idiv`, checks addresses
  (null region, above the address space), and raises for `int3`, `ud2`,
  privileged instructions, `int n` and far transfers. The check writes the
  registers back and calls the host's `fault` import, which now returns the
  address to continue at (it used to stop the program).
* **Dispatch.** The host (`runtime/wine/exceptions.mjs`) does what Wine's
  Unix side does for a signal on i386: it saves the registers (and the x87
  and SSE state) as a `CONTEXT`, puts the `EXCEPTION_RECORD` and the
  `CONTEXT` on the guest stack below the faulting frame, and continues at
  ntdll's `KiUserExceptionDispatcher`. Wine's dispatcher, translated like the
  rest of ntdll, calls the vectored handlers and walks the `FS:[0]` chain.
  The translated frames below are abandoned: a translated call that does not
  return to its return address hands the address up to the dispatcher loop,
  the same mechanism `longjmp` uses.
* **Continuing and unwinding** go through `NtContinue`, which loads every
  part of the `CONTEXT` its flags name (integer, control, x87, SSE, debug
  registers). `RtlRaiseException` arrives as `NtRaiseException`; the second
  chance (no handler) ends the process with the exception code, after
  kernel32's unhandled exception filter ran.
* **Precise faults.** The `CONTEXT` must show the state before the faulting
  instruction. `pop [mem]` raised esp before its store; it now keeps the old
  esp until the store succeeded. Segment register loads check the selector
  (the flat selectors Windows gives user code load; others raise a general
  protection fault).
* **Jumping to nothing.** A jump to memory that is not committed raises an
  access violation (execute) instead of translating garbage; code the
  translator cannot decode raises an illegal instruction exception.

Tests: `tests/wine/win32/seh.c` (each fault kind, with the exception code,
parameters, address and registers checked, then continued with a changed
eax; a handler frame left with `longjmp`, which unwinds through
`RtlUnwind`; `RaiseException`) and `unhandled.c` (a top-level filter, then
the process exit code). `node tests/wine/win32.mjs` builds them with MinGW at
-O0 and -O2 and runs them on translated Wine against the recorded output;
CI runs the same executables on real Windows.

Wine's own `ntdll_test.exe exception` unit runs further than before; what
still fails there:

* Single-stepping (the trap flag), hardware breakpoints (debug registers are
  kept and reported, but do not fire) and x87 exceptions unmasked in the
  control word are not emulated. Programs of the era rarely depend on them.
* No-execute protection is not enforced (programs of the era run code from
  data pages).
* Segment overrides other than `fs` are ignored, so a load through a null
  `es` or `gs` reports the address instead of a general protection fault.
* Arithmetic flags in a fault's `CONTEXT` are those the translated code
  last wrote back: faults write back registers, not the lazily computed
  flags, which would keep every flag computation alive.
* The rest needs threads, child processes or a debugger.

Faults no longer stop a program, so kernel32's tests that crashed now report
"Unhandled page fault" through Wine's own handler; no unit did worse, and
`profile` now finishes.
