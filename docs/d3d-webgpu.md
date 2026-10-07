# Direct3D 9 on WebGPU — the reusable core

Status as of October 7, 2026. This implements build-plan steps 1–4 and
parts of 5 and 6 of the *Direct3D on WebGPU* implementation guide: the
protocol, shader translator, emulation library and render core that a
wined3d `adapter_wgpu` front end (step 7) will drive. Nothing here knows
about wined3d.

```
front end (wined3d adapter_wgpu, later maybe a native d3d9.dll)
   │  d3dgpu-proto: versioned command batches in Direct3D 9 terms
   ▼  (shared memory; one hand-over per batch)
render worker ── d3dgpu-core ── wgpu ── WebGPU (browser) / Vulkan, Metal, DX12 (native)
                   ├── d3dgpu-shader: SM1–3 bytecode → WGSL, per variant key
                   └── d3dgpu-emu: fans, fill modes, formats, viewport clamping
```

## Crates

| Crate | What it is |
| --- | --- |
| `d3dgpu-proto` | The command stream: `Writer` (builder API in Direct3D 9 terms), zero-copy `Reader`, Direct3D 9 vocabulary (`Format`, `RenderState`, … as open newtypes with D3D values, plus D3D9's default states), and `include/d3dgpu_proto.h` for C front ends. |
| `d3dgpu-shader` | Shader model 1.0–3.0 bytecode parser, an fxc-syntax assembler/disassembler, reflection, and the WGSL generator. |
| `d3dgpu-emu` | Pure CPU emulation: index rewrites, texture format plans and conversions (incl. BC1–5 decode), vertex formats, viewport fitting. |
| `d3dgpu-core` | The render core on wgpu: executes batches, owns resources, caches, rings, readback and presentation. |
| `d3dgpu-scenes` | Test and performance scenes written against the builder API, with expected pixels and readback bytes. |
| `d3dgpu-web` | `wasm-bindgen` wrapper for the browser render worker; `runtime/d3dgpu/` is the page, the workers and the test runner. |

## Protocol

A batch is a 12-byte header (`"D3GP"`, version, length) followed by
commands of `u32 opcode, u32 size, payload`. Variable data is either inline
or a range of the shared memory region (`Data::Shared`), so a front end can
point at its upload buffers and resource shadows instead of copying.
Handles come from one front-end-allocated namespace. Readbacks
(`ReadTexture`) write the Direct3D format into shared memory and complete a
fence; `Signal` completes a fence when prior GPU work is done.

The C header mirrors the Rust layout; tests compile it and compare opcodes,
constants and struct sizes with what the writer emits.

## Shaders

Statement-by-statement translation: registers become `var<private>`, each
instruction a block that computes a `vec4<f32>` and writes the masked
components. D3D9's structured flow (`if`/`rep`/`loop`/`call`/`label`/`ret`,
`break*`, predication) maps directly onto WGSL. Every opcode of SM1–3 is
handled, including the 1.x texture-addressing family (`texbem`,
`texm3x3spec`, `texdepth`, …) and `ps_1_4` phases.

What the bytecode doesn't decide is the **variant key**:

* `VertexKey`: per input register, how the attribute arrives (float,
  `UBYTE4` as uint, `SHORT2/4` as sint, `UDEC3`/`DEC3N` unpacked, BGRA
  fallback); the varying list in location order (the pixel shader's inputs,
  so the stages always agree); clip mode; position fixup.
* `PixelKey`: per sampler, dimension, depth/compare, the format's swizzle
  and `D3DTTFF_PROJECTED` (1.x); alpha test function; fog mode (applied
  after pre-3.0 shaders); clip mode; flat shading.

Binding layout (all shaders): group 0 binds the vertex constants, pixel
constants and driver uniforms (half-pixel/viewport fixup, clip planes, fog,
alpha reference, bump matrices) with dynamic offsets; group 1 binds texture
`n` at `2n` and its sampler at `2n+1` (vertex samplers are 16–19).

Edge cases follow D3D9 hardware: `rcp`/`rsq`/`log` of 0 give ±FLT_MAX,
`pow` uses |x|, relative constant reads out of range return 0, `def`
constants win over the constant buffer (also through relative addressing),
pixel shader 1.x constants are clamped to [-1, 1], `vPos` is the integer
pixel coordinate, `vFace` is ±1, alpha test compares 8-bit values, depth
textures sample as a comparison except INTZ/DF16/DF24.

Derivatives and implicit-LOD sampling in non-uniform flow are accepted with
`diagnostic(off, derivative_uniformity)`. Hoisting coordinates and using
`textureSampleGrad` (the 3DMark06 lesson) is the precise fix and is still
to do.

## Render core

* **Resources.** Buffers keep a CPU shadow (index rewrites read it).
  Textures go through a format plan: native where WebGPU has the format,
  converted at upload otherwise (R5G6B5, A1R5G5B5, A4R4G4B4, R8G8B8, L8,
  A8L8, A8, P8, V8U8, …; BC decoded on the CPU without
  `texture-compression-bc`), with a shader swizzle for the Direct3D
  meaning of missing channels. Render targets in converted formats render
  into RGBA8 and convert back on readback.
* **Rings.** Uniforms, stream data (UP draws, rewritten indices) and
  uploads go into per-submission rings written with one `writeBuffer`
  before the submit. Writing a buffer or texture that recorded commands
  still read becomes a copy in command order, so a vertex buffer rewritten
  between two draws of one batch works. Constants upload only what the
  shader's reflection reads, so a draw with two vertex constants costs 256
  bytes of ring, not 4.4 KiB.
* **Pipelines.** Keyed on shaders, vertex layout, topology, cull,
  depth/stencil/bias, blend and write masks per target, formats. Layouts,
  samplers and bind groups are cached. `FrontFace::Cw` matches D3D9's
  default culling. Pipelines are created synchronously for now.
* **Emulation.** Triangle fans, wireframe and point fill become index
  lists; strips with a value WebGPU would treat as primitive restart become
  lists; viewports past the target are clamped with a clip-space fixup
  that also carries the half-pixel offset; user clip planes use
  `clip-distances` or a varying plus `discard`; X8 targets read as alpha
  one in blending; unbound samplers read (0, 0, 0, 1).
* **Clears** that cover the whole target become the next pass's load op;
  partial clears (rects, scissor, viewport) draw a quad whose colour is the
  blend constant.
* **Readback** copies to a `MAP_READ` buffer and completes in `poll()` from
  the event loop (browser) or `wait()` (native); fences retire in order.
* **Presentation** blits the owned back buffer into the output (a
  `wgpu::Surface` for a canvas or window, or a headless front buffer),
  forcing alpha to one for X8 back buffers and applying the gamma ramp.

## Browser threads (spike 1)

`runtime/d3dgpu/` runs the split the guide describes, with a producer
worker standing in for wined3d's CS thread:

* the **producer** encodes batches (the wasm scene library) into a
  `SharedArrayBuffer` slot and blocks with `Atomics.wait` for slot space and,
  for readbacks, for the fence — the synchronous `LockRect` path;
* the **render worker** owns the `GPUDevice` and the `OffscreenCanvas`,
  waits with `Atomics.waitAsync`, executes, and returns to the event loop so
  presentation and `mapAsync` proceed; a 1 ms pump copies finished
  readbacks into shared memory and notifies the fence.

The demo page shows the performance scene (500/2,000/5,000 draws) or any
test scene on the canvas with live counters; `?test` runs every scene
headlessly and checks it.

## Testing

| Layer | Command | Result |
| --- | --- | --- |
| Protocol round trips, malformed input, C header agreement | `cargo test -p d3dgpu-proto` | pass |
| Translator: every opcode assembles, round-trips through text, and translates under several keys to WGSL Naga accepts with browser capabilities; known fxc encodings | `cargo test -p d3dgpu-shader` | pass |
| Emulation unit tests (all 65,536 packed 16-bit values round-trip, BC blocks, viewport fits, fan/strip winding) | `cargo test -p d3dgpu-emu` | 45 pass |
| 36 scenes on native wgpu (lavapipe in CI), WebGPU default limits, with and without optional features | `cargo test -p d3dgpu-core --test scenes` | 36/36, 36/36 |
| Steady-state budget: 150 draws/frame after warm-up create nothing, one pass and one submit per frame | `cargo test -p d3dgpu-core --test budget` | pass |
| The same 36 scenes in headless Chromium on WebGPU (Dawn/Tint, SwiftShader) through the producer/render-worker pipeline, both feature sets | `runtime/d3dgpu/build.sh && node tests/web/d3dgpu.mjs` | 36/36, 36/36 |

Scenes cover: clears, culling, `vs_1_1`/`ps_1_1` texturing, eight texture
formats (packed 16-bit, L8, A8L8, X8, DXT1, A8), indexed and non-indexed
fans, wireframe, alpha test and blend, depth and stencil, scissored and
rect clears, table and vertex fog, user clip planes, the half-pixel rule,
viewport clamping, buffer and texture rewrites between draws, readback of
A8R8G8B8 and R5G6B5 targets, `ps_3_0` `vPos`/`vFace`, vertex formats
(`SHORT2`, `UBYTE4`, `DEC3N`, `D3DCOLOR`), relative constant addressing, UP
draws, instancing, `StretchRect`, gamma ramp, lines and points, cube and
volume textures, shadow-map comparison, `ps_1_4`, P8 palettes, flat
shading, 32-bit indexed strips with base vertex, and VS/PS flow control.

## Measurements

Native, llvmpipe on the 4-core development container (`cargo run
--release -p d3dgpu-core --example bench`): recording a game-shaped draw
(per-draw constants, texture and blend changes) costs **~1.2–1.5 µs** in
the core; `finish` + `submit` (wgpu-core encoding plus llvmpipe) dominates
the frame. The browser numbers on a real GPU are the ones that count; the
demo page shows them live.

## Not done yet

* **Front end:** wined3d `adapter_wgpu` (step 7) — the C header is ready.
* **Fixed function** in the core (wined3d sends its `ffp_hlsl` bytecode, so
  the wined3d path doesn't need it; a native d3d9 would), and the DXVK-style
  ubershader, async pipeline creation and per-game pipeline lists (step 5).
* **Point size and sprites** (points are 1 px), more than 8 vertex streams
  and unaligned vertex layouts (skipped and logged; `d3dgpu-emu` has the
  repacking), border colour and mirror-once (clamped), `MIPMAPLODBIAS`,
  sRGB writes, MSAA, occlusion queries, auto-generated mipmaps.
* **Depth:** readback of D24/D32 formats, `StretchRect` between depth
  surfaces, lockable depth.
* **Derivative hoisting** (see Shaders), pixel shader 1.x register range.
* **Threads:** the batch slot is single-buffered and copied out of shared
  memory; reading batches directly from shared wasm memory needs the
  atomics build. Clearing a native window (spike 1's last item) has the
  `SurfacePresenter` but no example yet.
