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
| Threads | Done (below) |
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

## Threads

Windows threads run in the one JavaScript thread that runs the program
(the page's worker, or Node's main thread), switched by a scheduler in
`runtime/wine/threads.mjs`. This differs from the plan's worker per thread:

* A thread's state is all in the shared memory (its registers in its CPU
  state, its frames on its guest stack), and translated frames can be
  abandoned at any time, the way exceptions and `longjmp` leave them. A
  thread that blocks in a system call made straight from guest code
  *yields*: the system call returns a yield address, the translated frames
  return to the dispatcher loop, and the scheduler runs another thread.
  The blocked one resumes at its return address once its wait is
  satisfied.
* Waits that cannot unwind (win32u's message waits, which wait inside
  Wine's Unix side, and waits inside a window procedure, which run under the
  host's callback) run the other threads on top of themselves until
  something changes, then check again. A thread run that way can still
  yield back to them.
* Threads switch when one blocks, and at system calls once a thread has run
  for 20 ms while others are ready. Code that spins without system calls,
  waiting for another thread, is not preempted.
* Programs of the era were written for one processor, so running one
  thread at a time costs them little, and nothing has to be shared between
  workers: the host's state, Wine's Unix side (whose C code is not
  thread-safe) and the function table all stay in one place.

What each piece does:

* **Thread creation** (`NtCreateThreadEx`): the host builds the TEB, stack,
  CPU state and initial context as Wine's Unix side does; the in-process
  wineserver gets a thread through `new_thread` and `init_thread`
  (`native/wine-unix/inproc/client.c`), each thread with its own wait pipe.
  The scheduler tells Wine's Unix side which thread is current
  (`NtCurrentTeb()` and the thread server requests come from).
* **Waits** on server objects (events, mutexes, semaphores, threads) are
  polled through Wine's own `NtWaitFor*` with a zero timeout, and block in
  the scheduler. `NtWaitForAlertByThreadId` and `NtAlertThreadByThreadId`
  (critical sections, SRW locks, condition variables and `WaitOnAddress` in
  Wine's ntdll), keyed events (`RtlRunOnce`), `Sleep` and
  `NtYieldExecution` are the scheduler's own.
* **Exit**: `NtTerminateThread` on itself tells the server, which wakes
  threads waiting on it, and frees the thread's TEB, stack and CPU state.
  Suspend and resume go through the server and the scheduler.
* **The clock**: Wine reads the tick count and the interrupt and system time
  from `KUSER_SHARED_DATA` without a system call. The host never updated
  them; a small worker (`runtime/wine/ticker.mjs`) now writes them every
  millisecond, as wineserver does, so `GetTickCount` moves while a program
  runs.

Test: `tests/wine/win32/threads.c` (four threads with a critical section and
interlocked counter, exit codes, thread-local storage, a producer and
consumer on two semaphores, a suspended start, events with and without
timeouts, a mutex between two threads, and waiting for the first of three
sleeping threads).

Not done: asynchronous procedure calls (alertable waits return without
running them), the contexts of threads blocked in a system call, and
threads that block inside nested waits while another thread waits for them
(for example, a worker thread sending a message to a window whose thread is
itself waiting inside a window procedure).
