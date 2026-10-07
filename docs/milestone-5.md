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
| Timers | Done (below) |
| Audio (`winmm`, DirectSound on an `AudioWorklet`) | Done (below) |
| DirectDraw (Wine's `ddraw` on `wined3d` with 3D off) | Done (below) |
| DirectInput | Done (below) |
| Cave Story | Intro, title and a new game in Node, with music (below); the browser: to check |

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
running them) and the contexts of threads blocked in a system call.

**Message waits yield.** win32u waits for messages inside Wine's Unix side,
which cannot unwind, so a thread waiting in `GetMessage` used to wait
nested, with other threads running on top of it; a thread sending it a
message then waited on top of the very thread that had to answer. The host
now handles win32u's waiting calls itself (`WIN32U_WAITS` in
`runtime/wine/thread-syscalls.mjs`): `GetMessage`,
`MsgWaitForMultipleObjectsEx` and `WaitMessage` poll the queue with a zero
timeout (`MWMO_INPUTAVAILABLE`) and block in the scheduler like any other
wait. Two fixes came with it: wineserver's clock is set from the host's on
every request (it trailed it, so a zero-timeout poll could wait a moment),
and a poll never runs other threads.

**Ending the process.** `ExitProcess` first terminates the other threads
(`NtTerminateProcess(0)`); a thread waiting nested under the exiting one is
marked and ends when its wait returns. Its end no longer replaces the exit
on its way out: `audio.exe` used to hang at exit, with the main thread left
running above a thread that had ended.

## Timers

Games pace their frames with `timeGetTime`, `QueryPerformanceCounter`,
`Sleep` and winmm's multimedia timers. Those all work on what the thread
work above provides: the clock in `KUSER_SHARED_DATA`, waits with timeouts
in the scheduler, and wineserver's timers (waitable timers and `WM_TIMER`),
which the scheduler runs whenever it polls or idles
(`wasm_server_run`). winmm's `timeSetEvent` runs its own thread that
sleeps until the next timer is due, and calls back on it.

A thread whose wait has a deadline that has come is preempted for at the
next system call of the running thread, so a timer thread fires on time
even while a game's main loop never blocks.

Test: `tests/wine/win32/timers.c` (`Sleep` measured by `timeGetTime` and
the performance counter, a periodic 10 ms multimedia timer on its own
thread, one-shot timers setting an event, `WM_TIMER` and a waitable timer;
counts are checked in ranges so the output is the same on Windows).

## Audio

Wine's sound stack runs unchanged down to its audio driver: `winmm` and
`dsound` play through `mmdevapi`, which loads a driver and calls its Unix
side. The host provides that Unix side (`runtime/wine/audio.mjs`):

* **The driver.** `winepulse.drv` is a stub DLL (`native/audio/winepulse.c`,
  built by `tools/wine/build.sh winepulse.drv`); when it asks for its Unix
  functions (`NtQueryVirtualMemory(MemoryWineUnixFuncs)`), the host answers
  with its own handle, and every `__wine_unix_call` on it goes to
  `BrowserAudio`. It implements the whole of Wine's audio driver interface
  (`unix_funcs` in `dlls/mmdevapi/unixlib.h`): endpoints (one, "Speakers",
  48 kHz float stereo), streams with their ring buffers in guest memory,
  render and capture buffers, volumes, clocks and positions.
* **Time.** A stream's position advances with `performance.now()` while it
  plays; each period the driver takes the frames due from the stream's
  buffer, converts them (8, 16, 24 and 32-bit integer, float; mono to
  stereo; any rate) to float stereo at the sink's rate and writes them out.
  The driver's timer thread (`timer_loop`) blocks in the scheduler until the
  next period and signals the stream's event, which is how `mmdevapi`'s
  clients (winmm's and DirectSound's mixing threads) know to refill.
* **The page.** The sink is a ring of float frames in a
  `SharedArrayBuffer` (`runtime/wine/audio-sink.mjs`) that an
  `AudioWorklet` (`audio-worklet.js`) reads at the `AudioContext`'s rate,
  playing silence when it runs dry. The page creates the context when a
  program starts (a user gesture). In Node, `wine.mjs --audio-out F.wav`
  writes what was played to a WAV file.
* **COM registration.** DirectSound and winmm create `mmdevapi`'s device
  enumerator with `CoCreateInstance`, which needs its class in the registry.
  On Windows (and in Wine's prefix creation) DLL registration writes it; the
  host now reads the registrar scripts Wine's DLLs carry (`WINE_REGISTRY`
  resources) at start-up and writes their keys (`runtime/wine/registry-setup.mjs`).

Test: `tests/wine/win32/audio.c` (a `waveOut` buffer completes after about
its length and the position reaches its end; a DirectSound secondary
buffer plays, its play cursor moves within the first second, and stops).
CI's Windows machines have no sound device, so this one is not compared
with a native run (`native: skip`).

## DirectDraw

Wine's `ddraw` runs on `wined3d`, which has a renderer without 3D
(`renderer=no3d`): surfaces live in system memory, blits and color fills
are done on the CPU, and the primary surface reaches the screen with GDI
(`BitBlt` into the window, which the browser display driver shows). The
host selects it (`WINE_D3D_CONFIG=renderer=no3d,csmt=0`); 3D is another
milestone's work.

* `wined3d` imports `opengl32`, whose Unix side the host stubs: it loads,
  and every OpenGL call fails.
* `csmt=0` keeps `wined3d`'s command stream on the program's thread. With
  its own thread, presents ran late, and that thread spins waiting for work,
  which a cooperative scheduler only stops at the end of a slice.
* Display mode changes work without the display driver knowing: win32u
  emulates modes on a display that has one (the screen stays 800x600; a
  640x480 game is scaled to it). 16 and 32-bit modes both work.
* Memory status (`GlobalMemoryStatusEx`, which `wined3d` asks for) now has
  answers: `SystemPerformanceInformation` and `ProcessVmCounters`.

Tests: `tests/wine/win32/ddraw.c` (windowed: a primary surface with a
clipper, offscreen surfaces, color fills, a color-keyed blit, `GetDC` on a
surface, a blit to the window, read back with GDI) and `ddraw-fullscreen.c`
(exclusive mode, 640x480 in 16 and 32 bits, a flipping primary surface with
a back buffer, pixels written through `Lock`, and the desktop's mode back).

Known gap: in a 16-bit mode, a back buffer written through `Lock` and then
flipped still shows its previous contents on screen (color fills and blits
show; the primary surface reads back right, so it is lost in `wined3d`'s
converting blit to the 32-bit front buffer). The fullscreen test checks
`Lock` in 32 bits only.

## DirectInput

Wine's `dinput` works as it is (it needs `hid` and `setupapi`, both built
now): the keyboard and mouse devices read the window's input, and there are
no joysticks. Test: `tests/wine/win32/dinput.c`.

## The web bundle

Sound, DirectDraw and DirectInput add DLLs the page does not need for other
programs (`wined3d` alone translates to 25 MB of WebAssembly), so they form
a group the worker fetches only when the program, or a DLL in its folder,
imports one of them. Their debug information is stripped in the bundle.

## Cave Story

The freeware Cave Story (the Aeon Genesis English translation; not in the
repository) runs in Node with `wine.mjs --folder` (its directory as
`C:\app`, as the page's folder picker gives it). Checked so far: the intro
and the title screen, windowed at both sizes (320x240 and 640x480) and
fullscreen in 16 and 32 bits; a new game starts from the keyboard
(`--input "30000: keydown KeyZ; 30300: keyup KeyZ"`); its music plays
through DirectSound (`--audio-out` captures it). It keeps up in real time:
50 seconds of play took 25 seconds of CPU. Still to check: playing further
in, and the browser.

Two fixes it needed:

* **Section slack.** The English patch put the window title in the bytes
  between the end of `.rdata`'s virtual size and the next page, which
  Windows maps from the file. The loader copied only the virtual size, so
  the window class had an empty name and `RegisterClassEx` failed.
* **Busy frame loops.** In fullscreen the game never waits for real (some
  thread is always ready), so `--run-for` is now checked between scheduler
  slices too, not only in idle waits.

