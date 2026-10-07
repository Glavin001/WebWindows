# Milestone 4 — windows on the screen

Status as of October 7, 2026.

## Done-when criteria

| Criterion (from the plan) | Status | Evidence |
| --- | --- | --- |
| win32u's Unix side compiled with Emscripten | Done | `native/wine-unix/build.sh` compiles Wine 11.0's `dlls/win32u` Unix side (with its DIB engine and FreeType font backend), wineserver and the parts of ntdll's Unix side they need into one module, `wine_unix.wasm`, that shares the machine's memory. |
| A browser display driver | Done | `native/wine-unix/driver/browser.c` is a win32u user driver: every top-level window gets a 32-bit window surface; its flushes go to `runtime/wine/display.mjs`, which composes the windows in stacking order into an RGBA screen, which the page draws into a canvas. |
| Keyboard and mouse | Done | The page puts canvas pointer, wheel and key events (`KeyboardEvent.code` mapped to virtual keys and scan codes) into a ring in shared memory; the driver sends them to wineserver as hardware input. |
| Wine's winemine and notepad run | Done | `node tests/web/gui.mjs`: in headless Chromium, Minesweeper draws its board, a click on the canvas uncovers squares; Notepad opens, and text typed on the canvas appears in it. `node tests/wine/gui.mjs` checks the same headless in Node, with scripted input, and saves screenshots. |
| user32 and gdi32 test pass rates are tracked | Done | `node tests/wine/winetest.mjs` runs every unit of `user32_test.exe` and `gdi32_test.exe`; results are tracked against `tests/wine/baseline/{user32,gdi32}_test.json`, and CI fails when a unit gets worse. |

No Direct3D or OpenGL is involved: windows are drawn by win32u's DIB
engine in WebAssembly, and the page only copies finished pixels.

## How a window gets to the canvas

```
 program ─► user32/gdi32 (PE, translated) ─► win32u.dll (PE) ─► system call 0x1000+
     ─► wine_unix.wasm: win32u Unix side ─► DIB engine draws into a window surface
     ─► browser driver: flush(rect) ─► display.mjs composes the screen (SharedArrayBuffer)
     ─► page: requestAnimationFrame copies changed frames into the canvas
```

* **One module for Wine's Unix side.** wineserver runs in the process
  (requests are function calls; waits run the server's main loop one step
  and then block on `Atomics.wait`); ntdll's Unix `sync.c`, `registry.c`,
  `env.c` and `security.c` are compiled unchanged; win32u's Unix side calls
  the rest of ntdll through the JavaScript host. win32u's system calls
  (0x1000 and up) are generated thunks (`gen-syscalls.py`) because
  WebAssembly checks indirect calls' signatures. User-mode callbacks
  (window procedures) re-enter the translated code through
  `KiUserCallbackDispatcher`.
* **Fonts.** FreeType (Emscripten's port) reads Wine's own TrueType fonts
  and the bitmap fonts Wine's build generates (`tools/wine/build.sh
  fonts`), from Wine's data directory, which the module sees as drive Z:.
* **Input.** The worker running the program cannot receive messages while
  it runs, so input goes through a ring in shared memory
  (`runtime/wine/input-ring.mjs`). When a wait in the program finds no work,
  it sleeps on the ring until its timeout or the next event, so idle
  programs use no CPU. The page sends physical keys as a US keyboard
  reports them; capitals come from Shift as on real hardware. The page is
  the only window manager: the driver makes the active window the
  foreground window, which is where wineserver sends keyboard input.
* **Side-by-side assemblies.** Programs whose manifests ask for Common
  Controls 6 (Notepad, Minesweeper) get the window classes from
  `comctl32_v6.dll`. The host installs assemblies as wineboot does: it reads
  `WINE_MANIFEST` resources and puts the manifests and DLLs into
  `C:\windows\winsxs` (`runtime/wine/sxs.mjs`).
* **Prelinking.** Most of Wine's DLLs are linked at the same base, so all
  but one would be relocated at load time and translated again in the
  browser. `wine-bundle.mjs` gives each its own base first and translates
  it there, so the browser compiles the shipped modules (streaming, cached)
  and translates only the program.

## Wine's user32 and gdi32 tests (baseline)

See `tests/wine/baseline/user32_test.json` and `gdi32_test.json`.

| | user32 | gdi32 |
| --- | --- | --- |
| Units | 24 | 14 |
| Finished | 15 (10 with no failures) | 12 (8 with no failures) |
| Crashed or timed out | 9 | 2 |
| Checks executed in finished units | 283,848 | 1,664,240 |
| Failed | 100 (99.96% pass) | 152 (99.99% pass) |

The units that do not finish:

* **Threads** (M5): `clipboard`, `cursoricon`, `dde`, `input`, `msg`, `win`,
  `winstation` (user32) and `gdiobj` (gdi32) start threads and wait for
  them.
* **Exceptions** (M5): `class` passes bad pointers on purpose and expects
  an exception to be caught.
* **Prefix set-up**: `sysparams` expects settings in the registry, and
  gdi32's `dib` hashes images with CryptoAPI, which needs the `rsaenh`
  provider registered. wineboot does both in a real Wine prefix.

Most of `monitor`'s failures come from display modes: the driver offers
one mode, the page's size, so `ChangeDisplaySettings` to 640×480 fails.
Fullscreen games switch modes, so the driver will need to offer the usual
modes and scale the screen in the page.

kernel32's baseline did not get worse with M4's changes; its `profile`
unit now finishes.

## Found and fixed on the way

* **Hardware input coordinates.** Absolute mouse input from a display
  driver is in screen pixels; only `SendInput` scales from 0–65535. Clicks
  were landing at the top-left corner.
* **Sections over whole files.** `NtCreateSection` with a maximum size of
  zero means "the whole file"; the host created empty sections, so ntdll
  could not read manifests from `C:\windows\winsxs`.
* **Case tables.** Wine's Unix-side `wcsicmp` uses case tables that
  `init_environment` loads from `l_intl.nls`; the in-process start-up never
  loaded them, so window class names only matched with their registered
  capitalization (`CreateWindow("static")` failed, `"Static"` worked).
* **Debug channels.** ntdll reads `WINEDEBUG`'s channels from the page after
  the PEB. The host now puts them there (`WINEDEBUG=+actctx node
  runtime/node/wine.mjs ...`); before, ntdll parsed part of the TEB as
  channel names.
* **Sleep.** ntdll's `NtDelayExecution` sleeps with `select()`, which
  Emscripten implements with thread machinery that failed outright in this
  module. It now sleeps through the host like other waits (`Atomics.wait`),
  and input that arrives meanwhile is queued for the program.
* **Directory listings.** The host lists directories of the virtual C:
  drive (`NtQueryDirectoryFile`), which assembly lookup needs.
* **Fonts in CI.** A fresh Wine build relinked `sfnt2fon` without FreeType
  before the bitmap fonts were generated; the build script now keeps make
  from replacing the FreeType-enabled one.
* **Reserved NOPs.** `0F 0D` with a register operand is a NOP on some x86
  CPUs and #UD on others (GitHub's runners); the fixture check skips that
  model-specific form.

## Not yet done

* Child processes (`explorer.exe` for the desktop, programs started from
  others) and threads arrive with M5; win32u logs that it could not start
  explorer and creates the desktop in the process.
* The cursor is the browser's arrow; window cursors are not shown yet.
* Keyboard layouts other than US, and IME input.
* Display mode changes (above).
* Several test units still crash or time out (above); most use threads,
  child processes or structured exception handling (M5).
