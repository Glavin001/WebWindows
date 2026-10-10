# Game performance on the page

How fast games run on translated Wine in the browser, how to measure it and
where the time goes. Far Cry's demo is the benchmark: 2004's Direct3D 9
(shader model 2 and fixed function), about 1,000 draws a frame in its first
level, its engine in 27 DLLs.

## Measuring

```sh
# Launch to menu, the menu's frame rate, into the Fort level with the mouse,
# the level's frame rate where the game starts, and CPU profiles of both
# workers there. --headed: Google Chrome in a window on the real GPU;
# headless: Chromium with SwiftShader (the guest's CPU time is the same).
node tools/web/bench/farcry.mjs DEMO_DIR [--headed] [--fresh] [--out target/bench/farcry] \
  [--level-secs 20] [--profile-ms 10000] [--syms target/wine-syms]
```

It prints a summary and writes `PREFIX.json` (the numbers, to compare runs),
`PREFIX.log`, the menu and level frames, and the profiles. The browser
profile, with the page's translation cache, is kept next to them, so the
first run is a cold start and later ones are warm (`--fresh` for cold).
Profiling needs Node 22.4 or later (its `WebSocket`).

The same commands drive any program (`tools/web/drive.mjs`):

| Command | What it does |
| --- | --- |
| `profile MS [NAME]` | CPU profiles of the guest worker (`runtime/web/worker.mjs`) and the render worker (`runtime/wine/d3d-worker.mjs`), sampled every 250 µs over the DevTools Protocol: time by category, translated x86 by DLL, top functions; `.cpuprofile` files open in Chrome DevTools (Performance, "Load profile") |
| `fps MS` | frame rate over MS from the render worker's statistics: average, lowest half second, worst frame, draws per frame, render worker busy; also a line `fps {json}` |
| `frame LABEL` | the next presented Direct3D frame as a PNG (canvas and offscreen present, where `shot` sees only the GDI screen) |
| `waitpage EXPR [MS]` | waits until a page expression holds, e.g. `window.webwindows.d3dPerf()?.drawsPerFrame > 300` |

`tools/web/cdp-profile.mjs PROFILE --images IMAGES.json --syms DIR`
summarizes a saved profile again. Translated functions are named
`x86_<address>` or `<export>@<address>` (`crates/wwt/src/codegen.rs`); the
images the program loaded (`window.webwindows.images()`, from the status
samples) turn addresses into DLL+offset, and with `--syms`, a directory of
unstripped PE files (Wine's DLLs from its build tree,
`/opt/wine-build/dlls/*/i386-windows/*.dll`), offsets into function names
and source lines.

The status samples (every 2 s; `drive.mjs` prints them) carry where the
program's thread waited on the render worker, in ms per second: for the
batch slot and for fences (readbacks), with readback MB/s:
`d3d 100/s (waits: slot 0 ms/s, fence 80x 475 ms/s)`.

## Far Cry, measured

Fort level, standing in the boat at the start, MacBook Pro (Apple Silicon),
Chrome 154, headed (Metal), without vsync (the benchmark's default; see
below), each step on top of the ones before:

| Change | fps | Where it was |
| --- | --- | --- |
| Start | 19.8 | 4 readbacks a frame, 47% of the time |
| Colour blits on the GPU, bounds traps for the game's DLLs, x87 registers forwarded within blocks | 64–69 (two runs) | game code 25–37% faster per function from the x87 pass (headless profiles) |
| Wine's DLLs built with SSE math | 84 | wined3d's matrices and the mixer in x87 |
| An exact system time on Wine's Unix side | 92 | DirectInput's polls slept 1 ms each |

Headless (SwiftShader) runs reached 58 fps after the blit fix and then
stopped telling much: with vsync they sit at 60, and without it they wait
for SwiftShader, which draws on the CPU.

Launch to menu: 64 s cold; about 10 s warm with the DLL translation cache.

## What was found

**Four readbacks a frame (47% of the time).** The guest worker's profile
had 44% of its time in `d3d.mjs:unixCall` itself: the program's thread
blocked in `Atomics.wait`. The wait counters put it on fences: 80 a second,
about 6 ms each. wined3d's WebGPU adapter did every colour blit on the CPU
(its blitter passed them to wined3d's CPU blitter, which reads source and
destination back, blits and uploads), and Far Cry copies the back buffer for
its screen effects every frame. Each readback waits for the render worker's
whole queue and the GPU. Colour blits between GPU textures now go to the
core's `STRETCH_RECT` (`wgpu_blit_colour` in
`native/wined3d-wgpu/adapter_wgpu.c`); colour keys, mirrored rectangles,
volumes and destinations that are not render targets stay on the CPU. Fence
waits went to zero and the frame rate tripled. Wine's rendering tests: D3D9
133/133 and D3D8 60/60 functions without failures, as before; DirectDraw 7
803 failures (from 808: `test_surface_format_conversion_alpha` 51, from 56).

**x87 registers in memory.** The translator keeps x87 registers in the CPU
struct, addressed through the stack top at run time: every `fld`, `fmul`
or `fstp` loads and stores there and rewrites the tag word. Far Cry's
engine is x87 code (2004's MSVC), so is all of Wine's float code built with
MinGW's defaults. `opt::forward_x87` follows the stack top within a block
(the block's starting top plus a known amount), so a load of a register the
block already loaded or stored becomes that value, and a store that a later
store replaces, with nothing in between that reads it or can fault, goes.
In the hottest culling function native loads went from 685 to 91.

**wined3d's float code was x87.** Wine's i386 DLLs are built with MinGW's
default `-mfpmath=387`. `tools/wine/build.sh` now builds them with
`-msse2 -mfpmath=sse`: the translator keeps SSE values in WebAssembly
locals. wined3d's per-draw fixed-function matrices (`invert_matrix`,
`multiply_matrix`) left the top of the profile and the mixer got 40%
cheaper.

**1 ms sleeps in DirectInput's polls.** With the idle time broken down by
what the threads waited for (status samples, `idle`), 85 ms a second went
to `polling (asked 1 ms)` under `dinput8.dll`: Wine's DirectInput calls
`MsgWaitForMultipleObjectsEx` with a zero timeout every time a game reads a
device. win32u turns the timeout into an absolute time from
`NtQuerySystemTime`, which on Emscripten came from `Date.now()` in
nanoseconds as a double, past 2^53, so up to a tick (100 ns) off; when it
came out a tick ahead of wineserver's clock (from `gettimeofday`, exact)
the poll waited for a timer, rounded up to a millisecond.
`native/wine-unix/prepare.py` now has `NtQuerySystemTime` compute integer
milliseconds times 10,000.

**Vsync.** A Direct3D 9 game presents with vsync unless it asks otherwise,
and the render worker waits for the display's next frame then. To measure
how fast a game runs, the page's `vsync` box (`?d3dvsync=0`) presents
without waiting; the benchmark does that unless `--vsync`.

## Where the time goes now

Headed, at 92 fps, the guest worker is 96% translated x86, without idle
time in the level. The biggest single functions are wined3d's state
application per draw (`wined3d_device_apply_stateblock` 5%,
`adapter_wgpu_draw_primitive` 3.5%, `wgpu_cmd_alloc` 1.3%), DirectSound's
mixer (4%) and Far Cry's own culling and rendering code
(`cry3dengine.dll`, `xrenderd3d9.dll`).

## Next

* **wined3d per draw.** It runs translated, about a quarter of a frame;
  running it (and d3d9) as native WebAssembly, or a direct Direct3D 9 path
  to the d3dgpu core, removes most of it.
* **The translator's code.** About 86 bytes of WebAssembly per x86
  instruction (`xrenderd3d9.dll`: 44 MB); x87 stores at block ends and the
  tag word could go too (registers in locals across blocks).
* **Mixing.** `DSOUND_MixToPrimary` as native code, as ntdll's heap and the
  string functions are (`docs/performance.md`).
* **Cold start.** The translator takes 14 s for `xrenderd3d9.dll` natively
  too (about 37,000 instructions a second), and sometimes overflows its
  stack on Wine's `opengl32.dll` (recursion whose depth varies run to run).
