# Direct3D 9, 10 and 11 on WebGPU — the reusable core

Status as of October 7, 2026. This implements build-plan steps 1–4 and
parts of 5 and 6 of the *Direct3D on WebGPU* implementation guide: the
protocol, shader translators, emulation library and render core that a
wined3d `adapter_wgpu` front end (step 7) will drive, for Direct3D 9 and
for Direct3D 10/11. Nothing here knows about wined3d.

```
front end (wined3d adapter_wgpu, later maybe a native d3d9.dll / d3d11.dll)
   │  d3dgpu-proto: versioned command batches in Direct3D 9 or 11 terms
   ▼  (shared memory; one hand-over per batch)
render worker ── d3dgpu-core ── wgpu ── WebGPU (browser) / Vulkan, Metal, DX12 (native)
                   ├── d3dgpu-shader: SM1–3 bytecode → WGSL, per variant key
                   ├── d3dgpu-dxbc: DXBC (SM4/5) → WGSL, per variant key
                   └── d3dgpu-emu: fans, fill modes, formats, viewport clamping
```

## Crates

| Crate | What it is |
| --- | --- |
| `d3dgpu-proto` | The command stream: `Writer` (builder API in Direct3D 9 terms), zero-copy `Reader`, Direct3D 9 vocabulary (`Format`, `RenderState`, … as open newtypes with D3D values, plus D3D9's default states), and `include/d3dgpu_proto.h` for C front ends. |
| `d3dgpu-shader` | Shader model 1.0–3.0 bytecode parser, an fxc-syntax assembler/disassembler, reflection, and the WGSL generator. |
| `d3dgpu-dxbc` | DXBC container and shader model 4.0–5.0 token parser, disassembler, reflection (signatures, declarations, resource usage), and the WGSL generator. |
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

Direct3D 10/11 commands (opcodes `0x100`–`0x139`) follow `ID3D11DeviceContext`
closely: resource, view and state-object creation with the D3D11
descriptions (`DXGI_FORMAT`, bind and misc flags, `D3D11_*_DESC` values
passed through), `CreateShader11` with the DXBC container, the `IA*`, `*S*`
and `OM*` setters, `Draw*`/`DrawIndexed*` (instanced), `Dispatch`, clears,
copies, `UpdateSubresource` (Map/Unmap of default resources is the front
end's `UpdateSubresource`), and `ReadSubresource` for staging maps. Both
APIs share the handle namespace, `Present`, fences and markers.

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

### Shader model 4/5

`d3dgpu-dxbc` reads the DXBC container (`ISGN`/`OSGN`/`OSG5`/`ISG1`/`OSG1`,
`SHDR`/`SHEX`) and the token stream: declarations, extended opcode tokens
(texel offsets, resource dimension and return type), relative and 64-bit
operand indices, immediate constant buffers. Registers are typeless, so
every register is a `vec4<u32>` and each instruction bitcasts its sources
to the type it works on (DXVK's approach in SPIR-V); WGSL is
structured like SM4/5 itself (`if`, `loop`, `switch` with case labels
gathered, `break`/`continue`/`retc`). D3D semantics where WGSL differs:
float-to-int conversions saturate and map NaN to 0, shifts mask their
count, `firstbit_hi` counts from the top, `umul`/`imul` high words,
integer division by zero, `sincos`, `bfi`/`ubfe`/`ibfe`, `f16tof32`
packing, atomics on storage buffers and workgroup memory (min/max on
signed values through compare-exchange loops).

Bindings: group 0 holds the vertex (or compute) shader's resources,
group 1 the pixel shader's, group 2 the driver block; within a group
constant buffer `n` is binding `n` (a uniform-ring window with a dynamic
offset), shader resource `n` is `16 + n`, sampler `n` is `144 + n`, UAV `n`
is `160 + n`. Typed buffers (`Buffer<T>`, `RWBuffer<T>`) become storage
buffers decoded in the shader, since WebGPU has no texel buffers; raw and
structured buffers are `array<u32>`. 1D textures are 2D textures one texel
high.

The variant key (`d3dgpu_dxbc::Key`) carries what the bytecode doesn't
say: which shader resources are depth textures (so `SampleCmp` gets a
`texture_depth_*`) and the element format of typed buffers and storage
textures; for vertex shaders, the pixel shader's input layout (varyings by
location, packed by semantic, integer varyings flat), clip/cull distances
as builtins or varyings plus `discard`, and the viewport fixup. The pixel
shader's `SV_Position.w` is the clip-space w (WebGPU's is its reciprocal);
`SV_VertexID` excludes `StartVertexLocation`/`BaseVertexLocation` and
`SV_InstanceID` excludes `StartInstanceLocation`, as in Wine's
`test_vertex_id` (the driver block carries the offsets).

All 362 shaders embedded in Wine's d3d10core/d3d11 conformance tests
(fxc output, `tools/dxbc/fetch-wine-shaders.sh`) decode; the 319 vertex,
pixel and compute shaders among them translate to WGSL that Naga
validates. Not translated yet: geometry, hull and domain shaders,
`gather4_po`, append/consume counters, `lod`, `msad`, `eval_snapped`,
doubles, subroutines (`fcall`).

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

### Direct3D 10/11 path

The D3D11 half (`crates/d3dgpu-core/src/d3d11/`) maps much more directly
than D3D9; what it emulates:

* **Constant buffers** are CPU shadows. A draw copies the window each
  shader reads into the uniform ring once per buffer version per
  submission and binds it with a dynamic offset, so the
  `Map(WRITE_DISCARD)`-per-draw pattern never ends the render pass and
  never needs a GPU copy.
* **Updating a buffer that recorded commands still read** renames it: a
  pooled buffer (reusable once its last submission is past) receives the
  whole new contents through the queue and the pass stays open. Large
  partial updates, and buffers the GPU wrote, fall back to a copy in
  command order.
* **Clears** of render-target and depth-stencil views are deferred and
  become the load operation of the next pass on that view (flushed as
  empty passes when something else reads the resource first).
* **Views.** Typeless textures are created as their UNORM/FLOAT member
  (or the depth format when bound as depth-stencil) with the sRGB twin as
  a view format; WebGPU allows no other reinterpretation, so other casts
  are logged. Shader views of depth-stencil textures see one aspect;
  read-only depth-stencil views become read-only attachments. Buffer
  views must start at a multiple of 256 bytes (WebGPU's storage offset
  alignment).
* **Hazards.** A texture bound both as a shader resource and as a render
  target in a draw reads zeros instead (D3D11's runtime unbinds it).
* **State caching.** A draw keeps what it derived from the bound state
  (targets, shader variants, bind groups, vertex layout, pipeline) until a
  command marks that part dirty; the pipeline is reused when the
  identities it depends on are unchanged, without building its full key.
  What remains per draw is the viewport fit, constant-buffer windows,
  buffer lookups and the pass commands.
* **Compute** dispatches run in their own compute pass; storage textures
  are write-only except single-channel 32-bit formats (WebGPU's
  read-write rule); UAV clears fill through the upload ring.

The design borrows from projects that put D3D11 on Metal — DXMT
(constant buffer and argument-buffer handling, encoder-level pass
merging) and Apple's D3DMetal as used by `d3dmetal-native` and RECALL
(state-object hashing, deferring work to pass boundaries) — at the level
of ideas; no code is shared.

## Pass commands

Both paths record pass commands through one cache that skips any that
would set what the pass already has (pipeline, bind group and dynamic
offsets, vertex and index buffers, viewport, scissor, blend constant,
stencil reference). In the browser each pass command is a call into
JavaScript and a Dawn validation step; in the perf frame this issues 2.7
commands per draw instead of 8. `Stats` counts issued and skipped
commands.

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

The page (`runtime/d3dgpu/`, also the Vercel preview) is the place for
testing on real hardware:

* **Demos** (`crates/d3dgpu-scenes/src/demos.rs`, HLSL in
  `crates/d3dgpu-scenes/hlsl/`): lit textured cubes with one draw each
  through Direct3D 11 (`cubes11`) and Direct3D 9 (`cubes9`, SM3 from
  HLSL), one instanced draw of up to a million cubes, compute-shader
  particles in structured buffers, HDR bloom (five passes), shadow maps
  with PCF, `DrawPrimitiveUP` sprite batches, and the synthetic perf
  frames. Each has a size parameter; the URL keeps scene, size and
  resolution, so a configuration can be shared as a link.
* **Statistics**: FPS, frame-time average and p50/p95/p99/worst, CPU time
  in the core (with `timing breakdown` or `?profile`: split into deriving
  state, recording and submitting; each clock read is a call into
  JavaScript, about 1 µs per draw, so it is off by default and in the
  benchmark), GPU
  submit-to-done time (`onSubmittedWorkDone`; at most two frames in
  flight, so GPU-bound scenes measure GPU throughput), draws and pass
  commands per draw, uploads, objects created per second (zero in a
  steady state), errors; with a frame-time graph. Optional vsync pacing
  (`requestAnimationFrame` in the render worker).
* **Benchmark** (`Run benchmark` or `?bench`): every demo at several sizes,
  1.5 s warm-up and 4 s measured each; results copy as Markdown (with the
  browser, `GPUAdapter.info`, features and limits) or download as JSON.
* **Tests** (`?test`): every scene checked against its expected pixels and
  readbacks, and every demo run for a few frames (no errors, real
  content); results copy as Markdown.

## Testing

| Layer | Command | Result |
| --- | --- | --- |
| Protocol round trips, malformed input, C header agreement | `cargo test -p d3dgpu-proto` | pass |
| Translator: every opcode assembles, round-trips through text, and translates under several keys to WGSL Naga accepts with browser capabilities; known fxc encodings | `cargo test -p d3dgpu-shader` | pass |
| Emulation unit tests (all 65,536 packed 16-bit values round-trip, BC blocks, viewport fits, fan/strip winding) | `cargo test -p d3dgpu-emu` | 45 pass |
| DXBC: container and token decoding, translation of an HLSL corpus (compiled with vkd3d, `tools/dxbc/compile.sh`) under several keys to WGSL Naga accepts; Wine's fxc corpus when fetched | `cargo test -p d3dgpu-dxbc` | pass |
| 54 scenes (36 D3D9, 18 D3D11) on native wgpu (lavapipe in CI), WebGPU default limits, with and without optional features | `cargo test -p d3dgpu-core --test scenes` | 54/54 |
| The 9 animated demos for a few frames: no errors, real content | `cargo test -p d3dgpu-core --test demos` | 9/9 |
| Wine's Direct3D 8/9 test shaders (244, SM1–3) translate to valid WGSL | `tools/dxbc/fetch-wine-d3d9-shaders.sh && cargo test -p d3dgpu-shader --test corpus` | 244/244 |
| Wine's Direct3D 10/11 test shaders (362, fxc) decode; VS/PS/CS translate to valid WGSL | `tools/dxbc/fetch-wine-shaders.sh && cargo test -p d3dgpu-dxbc --test corpus` | 319 + 35 GS/HS/DS unsupported |
| Steady-state budget: 150 draws/frame after warm-up create nothing, one pass and one submit per frame | `cargo test -p d3dgpu-core --test budget` | pass |
| The same 54 scenes and 9 demos in headless Chromium on WebGPU (Dawn/Tint, SwiftShader) through the producer/render-worker pipeline, both feature sets | `runtime/d3dgpu/build.sh && node tests/web/d3dgpu.mjs` | 63/63, 63/63 |

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

The Direct3D 11 scenes (HLSL in `crates/d3dgpu-scenes/hlsl/`, compiled
DXBC committed next to it) cover input layouts with `APPEND_ALIGNED`
offsets and BGRA colours, constant buffers rewritten (whole and by box)
between draws and shared between stages, vertex buffer renaming, indexed
draws with base vertex, `SV_VertexID`/`SV_InstanceID` offsets,
`SV_Position` (.5 centres, z, w), per-instance data with
`StartInstanceLocation`, textures and point sampling, mips by subresource
index with `SampleLevel`, a typeless texture through UNORM and sRGB
views, render-to-texture, depth then stencil write/test with colour
writes masked, alpha and blend-factor blending, cull/winding/scissor
rasterizer states, viewports past the target, a compute shader writing a
storage texture and a structured buffer (sampled, read back), a typed
`Buffer<float4>`, a depth-only pass (no pixel shader) into an
`R32_TYPELESS` texture sampled with a comparison sampler, MRT plus
`CopySubresourceRegion`, and readback of an `R32_UINT` target region and
of buffers after `CopyResource` and a partial update.

## Measurements

Native, llvmpipe on the 4-core development container (`cargo run
--release -p d3dgpu-core --example bench`): recording a game-shaped draw
(per-draw constants, texture and blend changes) costs **~1.1–1.5 µs** in
the core for either API; `finish` + `submit` (wgpu-core encoding plus
llvmpipe) dominates the frame.

In the browser the core runs as wasm and every pass command crosses into
JavaScript. Headless Chromium with SwiftShader in the container, perf
frame, execute time per batch:

| CPU µs per draw | perf9 | perf11 | cubes9 | cubes11 |
| --- | --- | --- | --- | --- |
| first version | 15 | 23 | 15 | – |
| pass-command cache, D3D11 state caching | 15 | 14 | 15 | 2.1 |
| D3D9 state reuse, device features read once | 5.4 | 4.5 | 2.4 | 2.6 |
| per-draw clocks off by default | 3.7 | 2.9 | 1.5 | 1.6 |
| wasm built with `opt-level = 3` instead of `"s"` | 2.1 | 1.7 | 1.1 | 1.0 |

The perf frames change texture and blend state on every draw; the cube
demos change only constants. The biggest single cost was
`device.features()`: on wgpu's WebGPU backend it walks the browser's
feature set, and the core called it for every texture binding.

On the first real-GPU run in Chrome (before the pass-command cache)
the Direct3D 9 frame measured ~37.5 ms per 5,000-draw batch, about
7.5 µs per draw. An iPhone (WebKit's WebGPU on Metal, before the last two
rows) measured 3.2 / 2.1 / 1.5 / 1.2 µs per draw at 5,000 draws, with
the per-draw clocks still on; the GPU-bound demos (500k instances, 1M
particles, 10k blended sprites at 1280x720) ran at 23, 77 and 10 ms of
GPU time per frame. The demo page shows the current numbers live.

## Not done yet

* **Direct3D 10/11:** geometry and tessellation shaders and stream output
  (compute emulation, as DXMT and D3DMetal do), wireframe fill (the D3D9
  index rewrite applies), instance step rates above 1 (vertex pulling),
  typeless reinterpretation beyond sRGB (needs copies), MSAA resolve,
  queries and predication, UAV counters, min/max filtering, border
  colours, dual-source blending, logic ops, buffer views off a 256-byte
  boundary, render target views of 3D slices other than 0, indirect draws,
  `GenerateMips`.

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
