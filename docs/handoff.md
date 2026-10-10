# Handoff: graphics on translated Wine (October 2026)

Knowledge transfer for whoever continues the graphics work: Direct3D 8/9,
DirectDraw and OpenGL in the browser, and the games on top of them. Read this
first, then the design docs it links. Everything here was true at commit
`5843cba` on `claude/sweet-brown-19o2ox` ([PR #12]).

[PR #12]: https://github.com/Glavin001/WebWindows/pull/12

## 1. Where things stand

**Open PR:** [PR #12] (base `main`) has about 77 commits. Its description is
current and lists every change, the known issues and the test results. main
is already merged in, so the branch merges cleanly.

- **CI:** the last full runs were green except for jobs that never got past
  installing Ubuntu packages (see §9). `5843cba` adds a `.deb` cache
  (`.github/actions/apt`). Its first run was still in progress at handoff:
  the builds and the deploy passed, the tests were running.
- **Merging:** the PR merges once CI is green. Nothing else blocks it.
- **Previews:** every PR push deploys a Vercel preview. A bot comment on the
  PR, starting `<!-- webwindows-preview -->`, is updated in place with one
  link per sample program (`tools/site/links.mjs`).

**What works** (headless Chromium uses SwiftShader, so no real GPU):

| Area | State | Measured by |
|---|---|---|
| Direct3D 9 | Wine's `visual.c`: 133/133 functions pass, as on native Wine | `tests/web/d3d9-visual.mjs` |
| Direct3D 8 | Wine's `visual.c`: 60/60 | `… --module d3d8 --test visual` |
| DirectDraw 7 | `ddraw7.c`: all 119 functions run; 803 failures against native's 45 | `… --module ddraw --test ddraw7` |
| OpenGL 2.1 | WebGL 2 through gl4es; `gltri`, `glbench` and `gl2test` (17/17) pass | `tests/web/gui.mjs` |
| Far Cry demo | menu, new game and the Fort level render and play, headless and in headed Chrome on Apple Metal | `tools/web/drive.mjs` |
| UT2004 demo | menus, Instant Action, a DeathMatch | `drive.mjs` |
| Quake II demo | `ref_gl` on WebGL 2 at about 50 fps; `ref_soft` too | `drive.mjs` |
| Direct3D 10/11 | **not yet**: DLLs build, the adapter stops at feature level 9_3 | see §6.2 |

**Known open problems,** in priority order: §6.2 (Direct3D 10/11), §6.3
(DirectDraw failures), then §6.4 and §6.5. (§6.1, Far Cry on a real GPU, is
solved.)

## 2. Mental model

```
program.exe (x86) ──wwt──► wasm ──┐
Wine's PE DLLs (x86) ──wwt──► wasm ┤  one page worker runs every guest thread
                                   │  (runtime/wine/threads.mjs schedules them)
Wine's Unix side (wineserver,      │
win32u, ntdll unix) ──emcc──► wasm ┘
          │ unix calls (__wine_unix_call) by handle:
          ├─ WINED3D_UNIXLIB 0x5000 → runtime/wine/d3d.mjs ─► render worker
          │      wined3d's WebGPU adapter (native/wined3d-wgpu/adapter_wgpu.c)
          │      encodes d3dgpu protocol batches → crates/d3dgpu-core on WebGPU
          ├─ GL_UNIXLIB      0x4000 → runtime/wine/webgl.mjs (WebGL 2, same worker)
          └─ AUDIO_UNIXLIB, WS2_32_UNIXLIB, …
```

- **Direct3D 8/9 and DirectDraw:** the path is d3d9.dll (or d3d8/ddraw) →
  wined3d → `adapter_wgpu.c` (our backend, patched into Wine 11.0 by
  `tools/wine/build.sh`) → the protocol (`crates/d3dgpu-proto`) → the render
  worker (`runtime/wine/d3d-worker.mjs`) → `d3dgpu-core` (Rust on wgpu,
  compiled to wasm) → WebGPU.
- **Shaders:** wined3d hands over D3D bytecode, and `crates/d3dgpu-shader`
  (SM1–3) or `crates/d3dgpu-dxbc` (SM4/5) translates it to WGSL. Fixed
  function comes as HLSL that wined3d compiles itself, so the core sees
  ordinary shaders.
- **OpenGL:** the path is `opengl32.dll` (gl4es + `native/opengl32-webgl`)
  → one unix call per OpenGL ES call → `WebGLBridge`
  (`runtime/wine/webgl.mjs`) → a WebGL 2 context on an OffscreenCanvas in the
  same worker. Node has no WebGL, so Node uses the older `native/opengl32`
  (OpenGL 1.1 over d3d9).
- **Design docs:**
  - [docs/d3d-webgpu.md](d3d-webgpu.md): the whole Direct3D design, including
    the adapter and its "Not done yet" list;
  - [docs/opengl.md](opengl.md): the OpenGL design;
  - [docs/games.md](games.md): per-game notes and fixes;
  - [docs/plan.md](plan.md): the project plan;
  - [docs/memory-traps.md](memory-traps.md);
  - [docs/performance.md](performance.md).

## 3. Environment

The container used so far had these, which are not in git:

| Path | What |
|---|---|
| `/opt/wine-src/wine-11.0` | Wine source. `tools/wine/build.sh` fetches it and applies `native/wined3d-wgpu/wined3d-wgpu.patch` (re-applied when the patch changes) |
| `/opt/wine-build`, `/opt/wine-build64` | i386 and x86_64 PE build trees (`WINE_BUILD`). `/opt/wine-build/gl4es` is the pinned gl4es build (§5.3) |
| `/opt/wine-native/build/wine` | native Wine with WoW64, for `--native` baselines. Needs `WINEPREFIX=/root/.wine-wow64` (made with `WINEDLLOVERRIDES="mscoree,mshtml="`) and an X server: `Xvfb :99 -screen 0 1280x1024x24 &`, `DISPLAY=:99` |
| `/opt/emsdk` | Emscripten, for `native/wine-unix/build.sh` (Wine's Unix side) |
| `/opt/pw-browsers` | Playwright's Chromium. Don't run `playwright install` |
| `target/games/` | game files (not committed): `farcry-x/far-cry-demo`, `ut2004-x`, `q2`. Where to download them: [docs/games.md](games.md) |
| `target/wine-bundle` | the browser bundle (`node runtime/node/wine-bundle.mjs`) |

**Rebuild after changes:**

```sh
sh tools/wine/build.sh <dll ...>          # e.g. wined3d d3d9 opengl32 d3d11 dxgi ddraw/tests
node runtime/node/wine-bundle.mjs         # browser bundle (needed after any DLL change)
cargo build --release -p wwt-cli          # the translator (Node runs use target/release/wwt)
sh runtime/d3dgpu/build.sh                # d3dgpu-core for the render worker, after Rust changes
```

## 4. Everyday commands and the expected results

```sh
node runtime/web/serve.mjs 8080           # http://localhost:8080/runtime/web/ (COOP/COEP headers)

# Regression suites (expected results at 5843cba):
node tests/programs/check.mjs --wine --jobs 4          # 45/45
node tests/wine/win32.mjs --jobs 4                     # 28/28
node tests/web/gui.mjs                                 # 25/25 browser GUI checks
node tests/wine/winetest.mjs --jobs 4 --timeout 120 --baseline tests/wine/baseline/user32_test.json \
  /opt/wine-build/dlls/user32/tests/i386-windows/user32_test.exe   # no regressions vs baseline
cargo test --workspace && cargo fmt --all --check

# Wine's rendering tests in headless Chromium (batches of 10, each in its own page):
node tests/web/d3d9-visual.mjs                         # D3D9: 133/133
node tests/web/d3d9-visual.mjs --module d3d8 --test visual
node tests/web/d3d9-visual.mjs --module ddraw --test ddraw7 --timeout 120
node tests/web/d3d9-visual.mjs --module d3d11 --test d3d11     # once §6.2 starts
#   --only A-B,C   --baseline FILE   --write-baseline FILE   --native (needs DISPLAY/WINEPREFIX above)
#   results: target/wine-tests/<module>-<test>/results.json, batch-A-B.txt per batch
```

Baselines live in `tests/web/baseline/`: `d3d9_visual*.json`,
`d3d8_visual*.json`, `ddraw_ddraw7*.json` and `d3d11_d3d11_native.json`
(native: 165 functions, 0 failures). The harness numbers each test call in
`START_TEST`, including `queue_test(...)` and `run_for_each_device_type(test_x)`,
so `--only` and the per-function results line up.

**Driving a program in the browser:**

```sh
# Commands are read from <out>.cmd (and lines appended to it later).
printf 'waitfor s.screen.colours > 200 300000\nwait 8000\nshot menu\nquit\n' > target/drive/x.cmd
node tools/web/drive.mjs target/games/farcry-x/far-cry-demo FarCry.exe --out target/drive/x \
  [--present gdi|canvas|offscreen] [--args "..."] [--video DIR]
```

- `shot` screenshots the GDI screen. With `--present offscreen`, read the
  presented frame instead:
  `js window.webwindows.debugReport().then(r=>"FRAME "+r.d3dFramePng)`,
  then decode the base64 from the log.
- **The `.cmd` file must match `--out`:** `--out target/drive/x` reads
  `target/drive/x.cmd`. With a mismatched name the run idles, waiting for
  commands.
- **Far Cry's keyboard menu navigation is not deterministic.** Enter, Tab ×5,
  Enter sometimes lands on "AI Auto Balance" or Options instead of Start.
  `-DEVMODE "\map fort"` did **not** start the level either: it stayed on the
  menu. A mouse click on Start, or finding the right console syntax, is still
  to do.
- A `waitfor s.screen.*` condition never fires in offscreen/canvas modes,
  because the GDI screen stays grey. Use `wait N` there.

## 5. What this branch changed (pointers)

Details are in the PR description and the commit messages, which explain the
why. The non-obvious parts:

### 5.1 Direct3D 8/9 (`crates/d3dgpu-shader`, `native/wined3d-wgpu/adapter_wgpu.c`)

- **D3D9:** everything listed in the PR. Two places emulate D3D9 hardware
  rather than WebGPU behaviour: cube maps filter within one face
  (`d3d_cube_dir`), and the projective divide rounds toward zero
  (`d3d_tdiv`).
- **D3D8 shaders have no `dcl`:** `reflect.rs` synthesizes vertex inputs
  from D3DVSDE register numbers.
- **D3D8 scalar ops read `.w`:** `rcp`, `rsq`, `exp`, `log` and their
  variants read the `.w` component when there is no replicate swizzle.
- **D3D8 declaration constants:** `D3DVSD_CONST` values are wined3d local
  constants, and `wgpu_shader_id` prepends them as `def` tokens.

### 5.2 Scheduler deadlocks (`runtime/wine/thread-syscalls.mjs`)

Waits are nested on the JavaScript stack. So **a wait inside Wine's Unix side
(wasm) can't yield**, and a thread that waits there pins every wait below it
until it returns. Two such deadlocks are fixed:

- **A blocked `GetMessage`'s check** ran Wine's `GetMessage`, which can wait
  in the Unix side, and a check (`threads.probe`, `this.polling`) can't run
  other threads. That hung DirectDraw's fullscreen tests. The check now uses
  `PeekMessage(PM_REMOVE)`, which never waits.
- **A synchronous `NtNotifyChangeKey`** (mmdevapi's device watcher) waited in
  the module. It is now registered with an event and waited for in the
  scheduler. Before the fix, `audio.exe` hung about one run in three.

Any new hang probably has this shape. Diagnose with `WWT_THREAD_DUMP` (§7).

### 5.3 OpenGL (`native/opengl32-webgl`, `runtime/wine/webgl.mjs`)

Read [docs/opengl.md](opengl.md) first.

- **gl4es:** commit `ec16bedd…`, built by `tools/wine/build.sh` with two
  `sed` patches, guarded by a stamp file (`gl4es/.built`). Bump
  `gl4es_stamp` when you add a patch.
- **Generated table:** `runtime/wine/gles-table.mjs` is generated by
  `gen-gles.mjs host` and committed. The build warns if it drifts from
  gl4es's header.
- **Which `opengl32` ships where:** the browser bundle ships
  `opengl32-webgl.dll` as `opengl32.dll` (`wine-bundle.mjs`). Node keeps the
  D3D9-based one.
- **Debugging:** `?unixtrace=gl` (also the "unixtrace" box under Debugging
  options on the page) logs every ES call, its GL error, and the client-side
  data each draw copies.

### 5.4 Page, CI, previews

- **Settings rule:** every setting must be both a URL parameter and a control
  on the page. `runtime/web/app.mjs` `SETTINGS` keeps them in sync. **Keep
  that rule**; the project owner asked for it explicitly.
- **Sample URLs:** running a sample puts `?exe=` in the address.
- **Debug report button:** saves a JSON file with the build, GPU, logs,
  status, the folder's summary, listing and config files, the paths the
  program looked for and did not find, its directory listings, and frames.
  Users send these, so learn to read them (§6.1 shows how).

## 6. Open work

### 6.1 Far Cry blurred on a real GPU (solved: an incomplete folder)

**Report:** the owner's Mac (Apple Metal, Chrome 154). After "Start new
game" the level showed as blue haze: smooth sky gradients, the pistol as a
dark blob, no HUD.

**Cause:** the page read the chosen folder only three levels deep
(`readDir` in `runtime/web/app.mjs`), so the folder picker gave Far Cry 552
of the demo's 2,427 files. Its shader folders are four and five levels down
(`Shaders/HWScripts/Declarations/CGPShaders`, `CGVShaders`,
`Techniques/Templates`, `Scripts/CryShaders/System`), so it ran with 5 of 9
shader files and none of its hardware shaders (the report's `log.txt`:
"5 Shader files found", "Compile System Shader 'SunFlares'...Fail").
`tools/web/drive.mjs` loads folders through the page's file input, which
had no limit, so headless runs always had every file and never reproduced
it. The guesses this section used to list (Metal LOD and derivatives,
timing, presentation) were all wrong.

**Fix:** both ways of choosing a folder now go through one loader
(`setFolder`) and read every level; the page shows the folder's file count,
size and depth, and `tests/web/picker.mjs` picks a folder with a file seven
levels down through the real `showDirectoryPicker` code path.

**What would have found it in the report,** and is in reports now:

- `folderSummary`: `{ files, bytes, depth }` of the folder the page read.
- `run.dirListings`: every directory listing the program took and how many
  entries it found (`c:\app\shaders\hwscripts\declarations\*.*` found 3
  of 5, and the shader folders below it were never listed).
- `run.missingFiles`: every path the program opened or queried that did not
  exist, in order. Most are ordinary probes (DLLs looked for in `c:\app`
  first, optional overrides); compare with a local run of the same program
  (`drive.mjs` … `record FILE`) to find the ones that matter.

### 6.2 Direct3D 10/11 (next feature; started, nothing committed)

**Goal:** Wine's `d3d11.c` and the `d3d10core` tests in the browser, against
native (165/165).

**Done locally, not committed:** `sh tools/wine/build.sh d3d11 dxgi d3d10core
d3d10 d3d10_1 d3d11/tests d3d10core/tests` builds all of them.

**What already exists:** the core has a complete D3D11-style command set
(`d3dgpu_proto.h` opcodes `0x0100`–`0x0139`, and
`crates/d3dgpu-core/src/d3d11/`):

- buffers, textures, views, samplers, blend/depth/rasterizer states, input
  layouts and SM4/5 shaders;
- bindings, render targets, viewports, scissors;
- draws, indexed and instanced draws, dispatch, clears, copies,
  `UpdateSubresource` and `ReadSubresource`.

It is tested by the scenes in `crates/d3dgpu-scenes` and `tests/web/d3dgpu.mjs`.
Values are D3D11's own (DXGI formats, `D3D11_BIND_*`, …). D3D11 objects are
separate types in the core (`Object::Buffer11`, `Texture11`, `View11`) from
the D3D9 ones.

**Plan:**

1. **Build and bundle.**
   - Add `d3d11 dxgi d3d10core d3d10 d3d10_1` (and the test directories) to
     `WINE_TARGETS` in `.github/actions/wine/action.yml`.
   - Add them to `MEDIA_DLLS` in `runtime/node/wine-bundle.mjs`, with
     `--no-smc-checks` in `TRANSLATE_FLAGS`, and mirror the flags in the
     Node translate path in `runtime/node/wine.mjs`.
   - Add the names to `names3d` in `runtime/web/worker.mjs`, so programs that
     load d3d11 by name get the media group and the render worker.
2. **A D3D11 mode in `adapter_wgpu.c`.** A device created at feature level 10
   or 11 (`device->cs->c.state->feature_level >= WINED3D_FEATURE_LEVEL_10`)
   uses the D3D11 commands for everything:
   - resources: `CREATE_BUFFER11` and `CREATE_TEXTURE11`;
   - uploads: `UPDATE_SUBRESOURCE`;
   - readback: `READ_SUBRESOURCE`, like the existing `wgpu_wait` fence path;
   - copies: `COPY_SUBRESOURCE_REGION`;
   - views: `CREATE_VIEW`, for wined3d RTV, DSV, SRV and UAV objects;
   - samplers: `CREATE_SAMPLER`;
   - blend, depth-stencil and rasterizer states, created lazily from the
     wined3d state objects' descriptors, cached by pointer.

   D3D9 devices keep the current path.
3. **Draws.**
   - **Shaders:** when the bound shaders are SM4+, send `CREATE_SHADER11`
     with the DXBC (`shader->byte_code`), once per shader id. Today
     `wgpu_shader_id` refuses SM4+ ("Shader model %u shaders are not
     supported yet").
   - **Input layouts** come from `wined3d_vertex_declaration`. Its elements
     carry `output_slot` (a register number), while the protocol wants
     semantic names. Resolve them with the bound vertex shader's input
     signature.
   - **Bindings per stage:** constant buffers (`state->cb`), SRVs, samplers
     and UAVs. Then `DRAW11` or `DRAW_INDEXED11`, with instancing.
   - **Clears** of RTVs and DSVs through the blitter, and `clear_uav`.
4. **Formats.** wined3d's `enum wined3d_format_id` is **not** in DXGI order.
   Write a mapping, for example by inverting
   `wined3dformat_from_dxgi_format` in `dlls/dxgi/utils.c`.
5. **Caps.** Raise `d3d_info->feature_level` (it is 9_3 today, in
   `wgpu_init_d3d_info` and `wined3d_gpu_from_feature_level`) and the shader
   caps (VS/PS/GS/HS/DS/CS 5) only once steps 2–3 work. d3d9 clamps its own
   caps to shader model 3.
6. **Present** from a `Texture11` back buffer: check that the core's
   presenters accept one.
7. **The rest:** queries, compute dispatch, stream output and geometry
   shaders (gaps listed in docs/d3d-webgpu.md, "Not done yet").

**Test** with `node tests/web/d3d9-visual.mjs --module d3d11 --test d3d11
--timeout 120`, which uses `queue_test` numbering. Add `d3d10core` the same
way.

### 6.3 DirectDraw 7 failures (803 vs native's 45)

Per function, from `tests/web/baseline/ddraw_ddraw7.json` (native has 0 in
each, apart from `test_palette_gdi`, 1):

| Function | Failures |
|---|---|
| `test_yuv_blit` | 592 |
| `test_compressed_surface_stretch` | 61 |
| `test_surface_format_conversion_alpha` | 56 |
| `test_depth_readback` | 48 |
| `test_user_memory` | 24 |
| `test_depth_blit` | 8 |
| `test_device_load` | 6 |
| `test_pixel_format` | 4 |
| `test_color_fill` | 4 |
| `test_palette_gdi` | 2 |
| `test_cross_device_blt` | 2 |
| `test_caps` | 1 |

They are mostly CPU-side format conversion and blits in the adapter's
blitter (`wgpu_blitter_*`, `wgpu_download`, the YUY2 path). Depth readback
(D24/D32) is a known gap in docs/d3d-webgpu.md. In Node, ddraw7 also exits
with code 5 after `test_vb_desc`, because Node has no 3D device. The browser
is the measurement that counts.

### 6.4 OpenGL follow-ups

- **Present without readback.** A swap currently reads the WebGL frame back
  and draws it with GDI: 7.5 ms of `glbench`'s frame time. Present it to the
  page canvas instead, as Direct3D does (`d3dpresent` modes).
- **Translated gl4es is 14 MB of wasm.** Look into lazy loading, or trimming
  gl4es features.
- **x86-64:** the `GL_UNIXLIB` branch for x64 in `host.mjs` still fails every
  call, and only i386 is wired.
- **Wine's `opengl32` tests:** mostly about WGL, so of little value.
- **Unknown extensions:** `wglGetProcAddress` for extensions gl4es lacks
  returns NULL.

### 6.5 Other

- **Task "cut per-draw CPU cost"** (d3d9 + wined3d + adapter) is open.
  Profiling notes are in docs/performance.md.
- **x86-64 Direct3D:** the bridge serves i386 only.
- **Quake on FTEQW** exits before its window opens (docs/games.md).

## 7. Debugging playbook

| Symptom | Tool |
|---|---|
| Hang | `WWT_THREAD_DUMP=15000 node runtime/node/wine.mjs …` prints every thread's state and call. "Ready but never runs" means a nested or polling wait (§5.2). `node --prof` + `node --prof-process` shows whether the host is spinning or sleeping (`idle`). `--report-on-signal` won't fire while the main thread is blocked in `Atomics.wait`. |
| Windows and messages | `--trace-unix win,msg` (Node) or `?unixtrace=win,msg` (page); PE side: `WINEDEBUG=+ddraw,+d3d,+win,+msg,+timestamp` (Node env), `?debug=` (page) |
| GL calls | `?unixtrace=gl` |
| A rendering difference | the d3d9-visual harness per function (`--only N`), the `batch-*.txt` outputs, and `--native` for what native Wine gets |
| Draw-level detail | `node runtime/node/wine.mjs --d3d-record f.gz …` then the replay example with `FROM=N`, `RS=STATE:VALUE`, `SHADERS=DIR` and `DUMP=full` |
| Unsupported x86 | `target/release/wwt translate X.dll -o x.wasm` lists unsupported instructions. One `aas` in gl4es is data, not code |
| Status | `WWT_STATUS=1` (Node; only prints when the host waits), the page's status line, `window.webwindows.status()` |
| Slow frames | `tools/web/bench/farcry.mjs` for numbers; `drive.mjs` `profile MS` for where the guest and render workers' time goes, by DLL and function; the status line's `waits:` for time blocked on the render worker ([game-performance.md](game-performance.md)) |

## 8. Gotchas (each cost time once)

- **Never `pkill -f <pattern>`.** It matches your own shell's command line and
  kills it. Kill exact PIDs:
  `ps -eo pid,comm,args | awk '$2=="node" && /pattern/ {print $1}'`.
- **Wine tests silence repeated lines after 42 occurrences.** The harness's
  `wwt function N` traces disappear after that; it doesn't mean the run
  stopped.
- **Headless Chromium's WebGPU canvas records black,** and buffer mapping
  breaks after presenting to an OffscreenCanvas. Use GDI or offscreen present
  for screenshots.
- **SwiftShader is not a GPU.** It is lenient where Metal, Vulkan and D3D12
  are undefined (derivatives in non-uniform flow, LOD), and about 6× slower,
  which changes time-based game behaviour. Real-GPU bugs (§6.1) need the
  owner's debug reports.
- **No WebGL in Node,** so the Node runs use the D3D9-based `opengl32`.
- **gl4es aliases `gl_Vertex` with attribute 0,** and its fixed-function
  pointers fold the bound buffer into the pointer. That is the bug patched in
  `build.sh`.
- **The WebGL context needs `alpha: true`** because the pixel format promises
  destination alpha. Without it, copies to RGBA textures fail.
- **`near` and `far` are macros** in MinGW's Windows headers. Don't name C
  functions that.
- **Disk:** writable space is a fixed allowance, and `target/` grows with
  translation caches and builds. Delete `target/debug` and old drive
  logs/videos when you hit "no space left".
- **apt on CI** stalls on mirror downloads, not on `apt update`. That is what
  `.github/actions/apt` (the `.deb` cache) is for. A cold cache can still
  stall once.

## 9. Conventions

- **Commits:** an imperative subject, then a body that explains what was
  wrong and why the fix is right, with measurements (for example "0 hangs in
  40 runs"). Match the existing docs' style: plain prose, no hype.
- **Docs:** update `docs/*.md` when behaviour changes. The PR description is
  the changelog; keep it current.
- **Validate before pushing.** CI cycles are slow. Run the relevant suites
  from §4 locally, and for CI fixes reproduce the failure first.
- **Baselines:** `--write-baseline` (browser and native) after a deliberate
  improvement. Never loosen a baseline to hide a regression.
