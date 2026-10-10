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
Chrome 154:

| | Headless (SwiftShader) | Headed (Metal) |
| --- | --- | --- |
| Before | — | 19.8 fps, 990 draws/frame |
| GPU colour blits | 58.1 fps, 986 draws/frame | |

Launch to menu: 64 s cold; 13 s warm with the DLL translation cache (10 s
headless, 11 s headed).

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

## Where the time goes now

Headless, after the fix, the guest worker is 84% translated x86:

| Share | What |
| --- | --- |
| 22% | `wined3d.dll`: state application (`wined3d_device_apply_stateblock`), `adapter_wgpu_draw_primitive`, fixed-function matrices (`invert_matrix`, `multiply_matrix`, `get_texture_matrix`), its mutex |
| 18% | `cry3dengine.dll` (one function at +0x1a8a2 is 6%) |
| 11% | `xrenderd3d9.dll` |
| 14% | `threads.mjs:idle`: every guest thread waiting (the game sleeps) |
| 6% | `dsound.dll` `DSOUND_MixToPrimary`: Wine's software mixer |
| 3% | `d3d9.dll` |

## Next

* **wined3d per draw.** It runs translated, about 20% of a frame; running it
  (and d3d9) as native WebAssembly, or a direct Direct3D 9 path to the d3dgpu
  core, removes most of it.
* **The game's own code.** Its DLLs get explicit memory checks (only the
  .exe gets bounds traps, `memTraps && path === exeDos` in
  `runtime/web/worker.mjs`), and translate to about 86 bytes of WebAssembly
  per x86 instruction (`xrenderd3d9.dll`: 44 MB).
* **Idle time.** Why the game's threads all wait 14% of the time.
* **Mixing.** `DSOUND_MixToPrimary` as native code, as ntdll's heap and the
  string functions are (`docs/performance.md`).
* **Cold start.** The translator takes 14 s for `xrenderd3d9.dll` natively
  too (about 37,000 instructions a second), and sometimes overflows its
  stack on Wine's `opengl32.dll` (recursion whose depth varies run to run).
