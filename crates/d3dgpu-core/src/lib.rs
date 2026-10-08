//! The d3dgpu render core: executes the command stream on WebGPU.
//!
//! The core knows Direct3D 9 and 10/11 semantics and nothing about wined3d. A front
//! end encodes batches with [`d3dgpu_proto::Writer`]; the host (the browser
//! render worker, a native test, a replay tool) owns a [`Core`] and feeds it
//! each batch with [`Core::execute`], plus the shared memory region the
//! batch's `Data::Shared` ranges and readbacks refer to.
//!
//! The core is the only thing that touches WebGPU. Everything Direct3D 9
//! has and WebGPU lacks is emulated here (with [`d3dgpu_emu`] and
//! [`d3dgpu_shader`]), so a second front end inherits every fix.
//!
//! ```no_run
//! # fn device() -> (wgpu::Device, wgpu::Queue) { unimplemented!() }
//! use d3dgpu_core::Core;
//! use d3dgpu_proto::{Writer, d3d9::clear};
//! let (device, queue) = device();
//! let mut core = Core::new(device, queue);
//! let mut w = Writer::new();
//! w.clear(clear::TARGET, 0xff336699, 1.0, 0, &[]);
//! core.execute(&w.finish(), &[]).unwrap();
//! ```

mod convert;
mod d3d11;
mod draw;
mod pass_cache;
mod present;
mod resources;
mod state;

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use d3dgpu_emu::format::{self as fmt, Conversion, FormatOptions};
use d3dgpu_proto::d3d9::*;
use d3dgpu_proto::{
    buffer_usage, texture_usage, Command, Data, Handle, Rect, Stage, TextureDesc, TextureKind, TextureRegion,
};

pub use present::{HeadlessPresenter, PresentContext, Presenter, SurfacePresenter};
use resources::{Buffer, FastMap, Object, Ring, Texture};
use state::{State, Target};

/// What the device offers beyond core WebGPU, and the size of the rings.
#[derive(Clone, Debug)]
pub struct Options {
    /// Use `@builtin(clip_distances)` for user clip planes (needs the
    /// `clip-distances` feature); otherwise planes are tested in the pixel
    /// shader.
    pub clip_distances: bool,
    /// Keep DXT/ATI textures compressed (needs `texture-compression-bc`).
    pub bc: bool,
    /// `float32-filterable`.
    pub float32_filterable: bool,
    pub uniform_ring: u64,
    pub stream_ring: u64,
    pub upload_ring: u64,
}

impl Options {
    /// Options matching what `device` was created with.
    pub fn for_device(device: &wgpu::Device) -> Options {
        let f = device.features();
        Options {
            clip_distances: f.contains(wgpu::Features::CLIP_DISTANCES),
            bc: f.contains(wgpu::Features::TEXTURE_COMPRESSION_BC),
            float32_filterable: f.contains(wgpu::Features::FLOAT32_FILTERABLE),
            uniform_ring: 4 << 20,
            stream_ring: 8 << 20,
            upload_ring: 16 << 20,
        }
    }
}

/// Counters for the demo page and the steady-state budget tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub batches: u64,
    pub commands: u64,
    pub draws: u64,
    /// Draws skipped because the core can't do them yet (logged).
    pub skipped_draws: u64,
    pub submits: u64,
    pub passes: u64,
    pub pipelines_created: u64,
    pub bind_groups_created: u64,
    pub samplers_created: u64,
    pub buffers_created: u64,
    pub textures_created: u64,
    pub shader_translations: u64,
    pub bytes_uploaded: u64,
    /// Clears folded into a render pass load operation.
    pub load_op_clears: u64,
    /// Clears drawn as quads (partial or mid-pass).
    pub quad_clears: u64,
    pub presents: u64,
    /// Render pass state commands (pipeline, bind group, buffers, viewport,
    /// …) issued, and skipped because they would set what was set.
    pub pass_commands: u64,
    pub pass_commands_skipped: u64,
    pub errors: u64,
    /// Nanoseconds spent in draws (decode excluded), of which deriving
    /// state (variants, bind groups, pipelines) and recording pass
    /// commands; and in `encoder.finish` + `queue.submit`. Kept only while
    /// profiling ([`Core::set_profiling`]).
    pub draw_ns: u64,
    pub prepare_ns: u64,
    pub record_ns: u64,
    pub submit_ns: u64,
}

/// A monotonic clock in nanoseconds.
fn clock_ns() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        START.get_or_init(std::time::Instant::now).elapsed().as_nanos() as u64
    }
    #[cfg(target_arch = "wasm32")]
    {
        (performance_now() * 1e6) as u64
    }
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
extern "C" {
    /// `performance.now()`, in windows and workers.
    #[wasm_bindgen(js_namespace = performance, js_name = now)]
    fn performance_now() -> f64;
}

/// A shader's cache key: the front end's hash, or one of the bytecode when
/// the front end sent 0. Translations are cached by it, so shaders must
/// never share one.
pub(crate) fn content_hash(hash: u64, bytes: &[u8]) -> u64 {
    if hash != 0 {
        return hash;
    }
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100_0000_01b3)) | 1
}

/// Errors that stop a batch. Problems with single commands (an unknown
/// handle, an unsupported format) are logged and skipped instead, as a
/// driver would.
#[derive(Debug)]
pub enum Error {
    Decode(d3dgpu_proto::DecodeError),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Decode(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

/// The colour, depth and stencil attachments of the open (or next) render
/// pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
struct PassTargets {
    colors: [Option<Target>; MAX_RENDER_TARGETS],
    depth: Option<Target>,
    width: u32,
    height: u32,
}

#[derive(Clone, Copy, Debug, Default)]
struct PendingClear {
    colors: [Option<wgpu::Color>; MAX_RENDER_TARGETS],
    depth: Option<f32>,
    stencil: Option<u32>,
}

impl PendingClear {
    fn any(&self) -> bool {
        self.colors.iter().any(Option::is_some) || self.depth.is_some() || self.stencil.is_some()
    }
}

enum PendingKind {
    Signal,
    Read {
        buffer: wgpu::Buffer,
        read: ReadInfo,
    },
    /// A Direct3D 11 readback: rows copied as they are.
    Raw {
        buffer: wgpu::Buffer,
        read: RawRead,
    },
}

/// Where the rows of a raw readback go.
struct RawRead {
    rows: usize,
    row_bytes: usize,
    slices: usize,
    gpu_row_pitch: usize,
    gpu_slice_pitch: usize,
    dest_offset: u32,
    row_pitch: u32,
    depth_pitch: u32,
}

struct ReadInfo {
    format: Format,
    plan: fmt::FormatPlan,
    width: u32,
    height: u32,
    gpu_row_pitch: u32,
    dest_offset: u32,
    row_pitch: u32,
}

struct Pending {
    fence: u64,
    /// 0 = waiting, 1 = done, 2 = failed.
    state: Arc<AtomicU8>,
    kind: PendingKind,
}

/// The render core.
pub struct Core {
    device: wgpu::Device,
    /// `device.features()`, which in the browser walks a JavaScript set.
    features: wgpu::Features,
    queue: wgpu::Queue,
    opts: Options,
    objects: FastMap<u32, Object>,
    st: State,
    enc: Option<wgpu::CommandEncoder>,
    pass: Option<wgpu::RenderPass<'static>>,
    pass_targets: Option<PassTargets>,
    /// A full-target clear waiting to become the next pass's load op.
    clear: Option<(PassTargets, PendingClear)>,
    /// Submission counter; resources record the epoch they were last used in.
    epoch: u64,
    uniforms: Ring,
    streams: Ring,
    uploads: Ring,
    caches: draw::Caches,
    d11: d3d11::Device11,
    pc: pass_cache::PassCache,
    blitter: present::Blitter,
    presenters: HashMap<u32, Box<dyn Presenter>>,
    presented: Vec<u32>,
    last_present: Option<u32>,
    pending: VecDeque<Pending>,
    completed_fence: u64,
    next_id: u64,
    stats: Stats,
    /// Whether the `*_ns` stats are kept. Off by default: in browsers each
    /// clock read is a call into JavaScript, several per draw.
    profile: bool,
    log: Vec<String>,
}

const LOG_LIMIT: usize = 256;

impl Core {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Core {
        let opts = Options::for_device(&device);
        Core::with_options(device, queue, opts)
    }

    pub fn with_options(device: wgpu::Device, queue: wgpu::Queue, opts: Options) -> Core {
        use wgpu::BufferUsages as U;
        let uniforms = Ring::new(&device, "d3dgpu uniforms", opts.uniform_ring, U::UNIFORM);
        let streams = Ring::new(&device, "d3dgpu streams", opts.stream_ring, U::VERTEX | U::INDEX);
        let uploads = Ring::new(&device, "d3dgpu uploads", opts.upload_ring, U::COPY_SRC);
        let caches = draw::Caches::new(&device, &uniforms.buffer);
        let d11 = d3d11::Device11::new(&device, &uniforms.buffer);
        let blitter = present::Blitter::new(&device);
        Core {
            features: device.features(),
            device,
            queue,
            opts,
            objects: FastMap::default(),
            st: State::new(),
            enc: None,
            pass: None,
            pass_targets: None,
            clear: None,
            epoch: 1,
            uniforms,
            streams,
            uploads,
            caches,
            d11,
            pc: pass_cache::PassCache::new(),
            blitter,
            presenters: HashMap::new(),
            presented: Vec::new(),
            last_present: None,
            pending: VecDeque::new(),
            completed_fence: 0,
            next_id: 1,
            stats: Stats::default(),
            profile: false,
            log: Vec::new(),
        }
    }

    /// Keep the time spent in draws, state derivation, pass recording and
    /// submission in [`Stats`] (`draw_ns` and the rest).
    pub fn set_profiling(&mut self, on: bool) {
        self.profile = on;
    }

    /// [`clock_ns`] while profiling, else 0.
    fn now_ns(&self) -> u64 {
        if self.profile {
            clock_ns()
        } else {
            0
        }
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    pub fn stats(&self) -> Stats {
        Stats { pass_commands: self.pc.issued, pass_commands_skipped: self.pc.skipped, ..self.stats }
    }

    /// Messages about skipped commands and unsupported features (most
    /// recent last, bounded).
    pub fn log(&self) -> &[String] {
        &self.log
    }

    /// Where `Present` to `window` goes. Windows without a presenter get a
    /// [`HeadlessPresenter`] the first time they are presented to.
    pub fn set_presenter(&mut self, window: u32, presenter: Box<dyn Presenter>) {
        self.presenters.insert(window, presenter);
    }

    /// Removes the presenter of `window` (to hand it to a new core).
    pub fn take_presenter(&mut self, window: u32) -> Option<Box<dyn Presenter>> {
        self.presenters.remove(&window)
    }

    pub fn presenter(&mut self, window: u32) -> Option<&mut (dyn Presenter + 'static)> {
        self.presenters.get_mut(&window).map(|p| p.as_mut())
    }

    /// The flags of the last `Present` since the previous call (a host
    /// returns to its event loop after a frame, or waits for vsync).
    pub fn take_present(&mut self) -> Option<u32> {
        self.last_present.take()
    }

    /// The highest fence whose work (and readbacks) has completed.
    pub fn completed_fence(&self) -> u64 {
        self.completed_fence
    }

    /// The texture behind a handle (for hosts and tests).
    pub fn texture(&self, h: Handle) -> Option<&wgpu::Texture> {
        match self.objects.get(&h.0) {
            Some(Object::Texture(t)) => Some(&t.gpu),
            Some(Object::Texture11(t)) => Some(&t.gpu),
            _ => None,
        }
    }

    pub(crate) fn warn(&mut self, msg: impl Into<String>) {
        self.stats.errors += 1;
        let msg = msg.into();
        if self.log.len() >= LOG_LIMIT {
            self.log.remove(0);
        }
        self.log.push(msg);
    }

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    // ---- Execution ----

    /// Executes one batch. `shared` is the shared memory region that
    /// `Data::Shared` ranges point into. GPU work is recorded and submitted
    /// at the end of the batch (and earlier when a readback, present or a
    /// full ring needs it).
    pub fn execute(&mut self, batch: &[u8], shared: &[u8]) -> Result<(), Error> {
        self.stats.batches += 1;
        for cmd in d3dgpu_proto::Reader::new(batch).map_err(Error::Decode)? {
            let cmd = cmd.map_err(Error::Decode)?;
            self.stats.commands += 1;
            self.command(cmd, shared);
        }
        self.submit();
        Ok(())
    }

    fn resolve<'s>(&mut self, d: Data<'s>, shared: &'s [u8]) -> Option<&'s [u8]> {
        let r = d.resolve(shared);
        if r.is_none() {
            self.warn(format!("shared memory range {d:?} is outside the {}-byte region", shared.len()));
        }
        r
    }

    fn command(&mut self, cmd: Command, shared: &[u8]) {
        if !matches!(
            cmd,
            Command::SetShaderConstF { .. }
                | Command::SetShaderConstI { .. }
                | Command::SetShaderConstB { .. }
                | Command::Draw { .. }
                | Command::DrawIndexed { .. }
                | Command::DrawUp { .. }
                | Command::DrawIndexedUp { .. }
                | Command::Clear { .. }
                | Command::WriteBuffer { .. }
                | Command::WriteTexture { .. }
                | Command::Present { .. }
                | Command::Marker(_)
                | Command::Signal { .. }
                | Command::ReadTexture { .. }
        ) {
            self.st.version += 1;
        }
        match cmd {
            Command::CreateBuffer { id, size, usage } => self.create_buffer(id, size, usage),
            Command::Destroy { id } => {
                self.objects.remove(&id.0);
                self.d11.dirty = d3d11::dirty::ALL;
                self.d11.caches.forget_handles();
            }
            Command::WriteBuffer { id, offset, data } => {
                if let Some(bytes) = self.resolve(data, shared) {
                    self.write_buffer(id, offset, bytes);
                }
            }
            Command::CreateTexture { id, desc } => self.create_texture(id, desc),
            Command::WriteTexture { region, row_pitch, slice_pitch, data } => {
                if let Some(bytes) = self.resolve(data, shared) {
                    self.write_texture(&region, row_pitch, slice_pitch, bytes);
                }
            }
            Command::CreateShader { id, stage, hash, bytecode } => {
                let Some(bytes) = self.resolve(bytecode, shared) else { return };
                let hash = content_hash(hash, bytes);
                match d3dgpu_shader::ShaderModule::from_bytes(bytes) {
                    Ok(module) => {
                        let want = match stage {
                            Stage::Vertex => d3dgpu_shader::Stage::Vertex,
                            Stage::Pixel => d3dgpu_shader::Stage::Pixel,
                        };
                        if module.stage() != want {
                            self.warn(format!("shader {id:?}: bytecode stage does not match {stage:?}"));
                            return;
                        }
                        let module = Arc::new(module);
                        self.objects.insert(id.0, Object::Shader(Box::new(resources::Shader { module, hash })));
                    }
                    Err(e) => self.warn(format!("shader {id:?} ({hash:016x}): {e}")),
                }
            }
            Command::CreateVertexDecl { id, elements } => {
                self.objects.insert(id.0, Object::VertexDecl(elements));
            }
            Command::SetPalette { index, entries } => {
                let mut p = [[0u8; 4]; 256];
                for (i, e) in entries.as_chunks::<4>().0.iter().enumerate() {
                    p[i].copy_from_slice(e);
                }
                self.st.palettes.insert(index, p);
            }
            Command::SetRenderTarget { index, texture, face, level } => {
                if let Some(t) = self.st.rts.get_mut(index as usize) {
                    *t = Target { texture, face, level };
                }
            }
            Command::SetDepthStencil { texture, face, level } => self.st.ds = Target { texture, face, level },
            Command::SetViewport(vp) => self.st.viewport = vp,
            Command::SetScissor(r) => self.st.scissor = r,
            Command::SetRenderState { state, value } => {
                if let Some(s) = self.st.rs.get_mut(state.0 as usize) {
                    *s = value;
                }
            }
            Command::SetSamplerState { sampler, state, value } => {
                if let Some(s) = self.st.samp.get_mut(sampler as usize).and_then(|s| s.get_mut(state.0 as usize)) {
                    *s = value;
                }
            }
            Command::SetTexture { sampler, texture } => {
                if let Some(t) = self.st.textures.get_mut(sampler as usize) {
                    *t = texture;
                }
            }
            Command::SetTextureStageState { stage, state, value } => {
                if let Some(s) = self.st.tss.get_mut(stage as usize).and_then(|s| s.get_mut(state.0 as usize)) {
                    *s = value;
                }
            }
            Command::SetVertexShader(h) => self.st.vs = h,
            Command::SetPixelShader(h) => self.st.ps = h,
            Command::SetVertexDecl(h) => self.st.decl = h,
            Command::SetStreamSource { stream, buffer, offset, stride } => {
                if let Some(s) = self.st.streams.get_mut(stream as usize) {
                    s.buffer = buffer;
                    s.offset = offset;
                    s.stride = stride;
                }
            }
            Command::SetStreamFreq { stream, value } => {
                if let Some(s) = self.st.streams.get_mut(stream as usize) {
                    s.freq = value;
                }
            }
            Command::SetIndices { buffer, format } => {
                self.st.indices = buffer;
                self.st.index_format = format;
            }
            Command::SetShaderConstF { stage, start, data, .. } => self.consts(stage).set_f(start, data),
            Command::SetShaderConstI { stage, start, data, .. } => self.consts(stage).set_i(start, data),
            Command::SetShaderConstB { stage, start, data, .. } => self.consts(stage).set_b(start, data),
            Command::SetClipPlane { index, plane } => {
                if let Some(p) = self.st.clip_planes.get_mut(index as usize) {
                    *p = plane;
                }
            }
            Command::Clear { flags, color, z, stencil, rects } => self.clear(flags, color, z, stencil, &rects),
            Command::Draw { prim, start_vertex, prim_count } => {
                self.draw(prim, draw::Source::Vertices { start: start_vertex, count: prim_count })
            }
            Command::DrawIndexed { prim, draw } => self.draw(prim, draw::Source::Indexed(draw)),
            Command::DrawUp { prim, prim_count, stride, vertices } => {
                if let Some(v) = self.resolve(vertices, shared) {
                    self.draw(prim, draw::Source::Up { count: prim_count, stride, vertices: v, indices: None })
                }
            }
            Command::DrawIndexedUp {
                prim,
                min_index: _,
                num_vertices: _,
                prim_count,
                index_format,
                stride,
                indices,
                vertices,
            } => {
                if let (Some(i), Some(v)) = (self.resolve(indices, shared), self.resolve(vertices, shared)) {
                    let indices = Some((i, index_format));
                    self.draw(prim, draw::Source::Up { count: prim_count, stride, vertices: v, indices })
                }
            }
            Command::StretchRect { src, src_face, src_level, src_rect, dst, dst_face, dst_level, dst_rect, filter } => {
                self.stretch_rect(
                    Target { texture: src, face: src_face, level: src_level },
                    src_rect,
                    Target { texture: dst, face: dst_face, level: dst_level },
                    dst_rect,
                    filter,
                )
            }
            Command::Present { texture, window, flags } => {
                self.last_present = Some(flags);
                self.present(texture, window)
            }
            Command::SetGammaRamp { window, ramp } => {
                let mut r = [[0u16; 256]; 3];
                for (i, c) in ramp.as_chunks::<2>().0.iter().enumerate() {
                    r[i / 256][i % 256] = u16::from_le_bytes([c[0], c[1]]);
                }
                self.ensure_presenter(window);
                if let Some(p) = self.presenters.get_mut(&window) {
                    p.set_gamma_ramp(&r);
                }
            }
            Command::ReadTexture { region, dest_offset, row_pitch, slice_pitch: _, fence } => {
                self.read_texture(&region, dest_offset, row_pitch, fence)
            }
            Command::Signal { fence } => {
                self.submit();
                let state = Arc::new(AtomicU8::new(0));
                let s = state.clone();
                self.queue.on_submitted_work_done(move || s.store(1, Ordering::Release));
                self.pending.push_back(Pending { fence, state, kind: PendingKind::Signal });
            }
            Command::Marker(text) => {
                if let Some(p) = self.pass.as_mut() {
                    p.insert_debug_marker(text);
                } else if let Some(e) = self.enc.as_mut() {
                    e.insert_debug_marker(text);
                }
            }
            other => self.command11(other, shared),
        }
    }

    fn consts(&mut self, stage: Stage) -> &mut state::Consts {
        match stage {
            Stage::Vertex => &mut self.st.vs_consts,
            Stage::Pixel => &mut self.st.ps_consts,
        }
    }

    // ---- Resources ----

    fn create_buffer(&mut self, id: Handle, size: u32, usage: u32) {
        let mut u = wgpu::BufferUsages::COPY_DST;
        if usage & buffer_usage::VERTEX != 0 {
            u |= wgpu::BufferUsages::VERTEX;
        }
        if usage & buffer_usage::INDEX != 0 {
            u |= wgpu::BufferUsages::INDEX;
        }
        if usage & (buffer_usage::VERTEX | buffer_usage::INDEX) == 0 {
            u |= wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::INDEX;
        }
        let padded = (size as u64).next_multiple_of(4).max(4);
        let gpu = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("d3dgpu buffer"),
            size: padded,
            usage: u,
            mapped_at_creation: false,
        });
        self.stats.buffers_created += 1;
        let bid = self.id();
        let b = Buffer { gpu, id: bid, size, shadow: vec![0; padded as usize], last_use: 0 };
        self.objects.insert(id.0, Object::Buffer(b));
    }

    fn write_buffer(&mut self, id: Handle, offset: u32, bytes: &[u8]) {
        let epoch = self.epoch;
        let Some(Object::Buffer(b)) = self.objects.get_mut(&id.0) else {
            return self.warn(format!("WriteBuffer: {id:?} is not a buffer"));
        };
        let end = offset as usize + bytes.len();
        if end > b.size as usize {
            let size = b.size;
            return self.warn(format!("WriteBuffer: {} bytes at {offset} overflow {id:?} ({size} bytes)", bytes.len()));
        }
        b.shadow[offset as usize..end].copy_from_slice(bytes);
        // WebGPU copies need 4-byte alignment; widen to whole words from
        // the shadow copy.
        let start = offset as usize & !3;
        let stop = end.next_multiple_of(4);
        let in_use = b.last_use == epoch;
        self.stats.bytes_uploaded += (stop - start) as u64;
        if !in_use {
            let Some(Object::Buffer(b)) = self.objects.get(&id.0) else { unreachable!() };
            self.queue.write_buffer(&b.gpu, start as u64, &b.shadow[start..stop]);
            return;
        }
        // Recorded commands read the old contents: copy in command order.
        self.end_pass();
        let data = {
            let Some(Object::Buffer(b)) = self.objects.get(&id.0) else { unreachable!() };
            b.shadow[start..stop].to_vec()
        };
        let src = match self.uploads.alloc(&data, 4) {
            Some(o) => o,
            None => {
                self.submit();
                let Some(Object::Buffer(b)) = self.objects.get(&id.0) else { unreachable!() };
                self.queue.write_buffer(&b.gpu, start as u64, &data);
                return;
            }
        };
        let upload = self.uploads.buffer.clone();
        let Some(Object::Buffer(b)) = self.objects.get(&id.0) else { unreachable!() };
        let dst = b.gpu.clone();
        self.encoder().copy_buffer_to_buffer(&upload, src, &dst, start as u64, (stop - start) as u64);
    }

    fn create_texture(&mut self, id: Handle, desc: TextureDesc) {
        let opts = FormatOptions { bc_supported: self.opts.bc };
        let Some(plan) = fmt::plan(desc.format, &opts) else {
            return self.warn(format!("CreateTexture {id:?}: unsupported format {:?}", desc.format));
        };
        let format = convert::texture_format(plan.gpu);
        let max_levels = 32 - desc.width.max(desc.height).max(desc.depth).max(1).leading_zeros();
        let levels = if desc.levels == 0 { max_levels } else { desc.levels.min(max_levels) };
        let (dimension, layers) = match desc.kind {
            TextureKind::D2 => (wgpu::TextureDimension::D2, 1),
            TextureKind::Cube => (wgpu::TextureDimension::D2, 6),
            TextureKind::Volume => (wgpu::TextureDimension::D3, desc.depth.max(1)),
        };
        let mut usage = wgpu::TextureUsages::TEXTURE_BINDING;
        let depth = format.is_depth_stencil_format();
        if !matches!(format, wgpu::TextureFormat::Depth24Plus | wgpu::TextureFormat::Depth24PlusStencil8) {
            usage |= wgpu::TextureUsages::COPY_SRC;
            if !depth {
                usage |= wgpu::TextureUsages::COPY_DST;
            }
        }
        let renderable = desc.kind != TextureKind::Volume && !format.is_compressed();
        if renderable && (desc.usage & (texture_usage::RENDER_TARGET | texture_usage::DEPTH_STENCIL) != 0 || depth) {
            usage |= wgpu::TextureUsages::RENDER_ATTACHMENT;
        }
        let srgb = plan.gpu.srgb().map(convert::texture_format);
        let view_formats: Vec<wgpu::TextureFormat> = srgb.into_iter().collect();
        let (w, h) = plan.gpu_extent(desc.width, desc.height);
        let gpu = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("d3dgpu texture"),
            size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: layers },
            mip_level_count: levels,
            sample_count: 1,
            dimension,
            format,
            usage,
            view_formats: &view_formats,
        });
        self.stats.textures_created += 1;
        let view_dim = match desc.kind {
            TextureKind::D2 => wgpu::TextureViewDimension::D2,
            TextureKind::Cube => wgpu::TextureViewDimension::Cube,
            TextureKind::Volume => wgpu::TextureViewDimension::D3,
        };
        let aspect = if depth { wgpu::TextureAspect::DepthOnly } else { wgpu::TextureAspect::All };
        let mk_view = |f: wgpu::TextureFormat| {
            gpu.create_view(&wgpu::TextureViewDescriptor {
                label: Some("d3dgpu sample view"),
                format: (!depth).then_some(f),
                dimension: Some(view_dim),
                aspect,
                ..Default::default()
            })
        };
        let view = mk_view(format);
        let srgb_view = srgb.map(mk_view);
        let view_id = self.id();
        let srgb_view = srgb_view.map(|v| (v, self.id()));
        let t = Texture {
            gpu,
            desc,
            plan,
            format,
            levels,
            view,
            view_id,
            srgb_view,
            rt_views: HashMap::new(),
            last_use: 0,
        };
        self.objects.insert(id.0, Object::Texture(t));
    }

    fn write_texture(&mut self, region: &TextureRegion, row_pitch: u32, slice_pitch: u32, bytes: &[u8]) {
        let palette = self.st.palettes.get(&0).copied();
        let epoch = self.epoch;
        let Some(Object::Texture(t)) = self.objects.get(&region.texture.0) else {
            return self.warn(format!("WriteTexture: {:?} is not a texture", region.texture));
        };
        if region.level >= t.levels {
            return self.warn(format!("WriteTexture: level {} out of range", region.level));
        }
        if t.plan.conversion == Conversion::NoUpload {
            return self.warn(format!("WriteTexture: {:?} has no CPU upload path", t.desc.format));
        }
        let plan = t.plan;
        let block = t.desc.format.block_dim();
        let gpu_block = plan.gpu.block_dim();
        let (x, y) = (region.x / block * block, region.y / block * block);
        let depth = region.depth.max(1);
        let mut packed = Vec::new();
        for z in 0..depth {
            let src = &bytes[(z * slice_pitch) as usize..];
            packed.extend(fmt::convert(&plan, src, region.width, region.height, row_pitch, palette.as_ref()));
        }
        let gpu_row = plan.gpu.row_bytes(region.width);
        let gpu_rows = plan.gpu.block_rows(region.height);
        let in_use = t.last_use == epoch;
        let (layer, z0) = match t.desc.kind {
            TextureKind::Cube => (region.face, 0),
            TextureKind::Volume => (0, region.z),
            TextureKind::D2 => (0, 0),
        };
        let gpu = t.gpu.clone();
        // Copies must cover whole blocks and stay inside the level, which
        // for a BC level smaller than a block means its physical size.
        let (lw, lh) = t.level_size(region.level);
        let phys = |v: u32, lv: u32| {
            if gpu_block > 1 {
                v.next_multiple_of(gpu_block).min(lv.next_multiple_of(gpu_block))
            } else {
                v
            }
        };
        let extent = wgpu::Extent3d {
            width: phys(region.width, lw),
            height: phys(region.height, lh),
            depth_or_array_layers: depth,
        };
        let (level, origin) = (region.level, wgpu::Origin3d { x, y, z: layer + z0 });
        fn dest(gpu: &wgpu::Texture, mip_level: u32, origin: wgpu::Origin3d) -> wgpu::TexelCopyTextureInfo<'_> {
            wgpu::TexelCopyTextureInfo { texture: gpu, mip_level, origin, aspect: wgpu::TextureAspect::All }
        }
        self.stats.bytes_uploaded += packed.len() as u64;
        if !in_use {
            let layout =
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(gpu_row), rows_per_image: Some(gpu_rows) };
            self.queue.write_texture(dest(&gpu, level, origin), &packed, layout, extent);
            return;
        }
        // Rows of buffer-to-texture copies are 256-byte aligned.
        self.end_pass();
        let aligned = gpu_row.next_multiple_of(256);
        let total = aligned as usize * gpu_rows as usize * depth as usize;
        let Some((offset, dst)) = self.uploads.reserve(total, 256) else {
            self.submit();
            let layout =
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(gpu_row), rows_per_image: Some(gpu_rows) };
            self.queue.write_texture(dest(&gpu, level, origin), &packed, layout, extent);
            return;
        };
        for r in 0..(gpu_rows * depth) as usize {
            let s = r * gpu_row as usize;
            dst[r * aligned as usize..r * aligned as usize + gpu_row as usize]
                .copy_from_slice(&packed[s..s + gpu_row as usize]);
        }
        let upload = self.uploads.buffer.clone();
        self.encoder().copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: &upload,
                layout: wgpu::TexelCopyBufferLayout {
                    offset,
                    bytes_per_row: Some(aligned),
                    rows_per_image: Some(gpu_rows),
                },
            },
            dest(&gpu, level, origin),
            extent,
        );
    }

    // ---- Encoding ----

    fn encoder(&mut self) -> &mut wgpu::CommandEncoder {
        let device = &self.device;
        self.enc.get_or_insert_with(|| {
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("d3dgpu") })
        })
    }

    fn end_pass(&mut self) {
        self.pass = None;
        self.pass_targets = None;
        self.d11.pass = None;
    }

    /// The attachments the current state renders to, or `None` when
    /// nothing usable is bound.
    fn current_targets(&mut self) -> Option<PassTargets> {
        let mut t = PassTargets::default();
        let mut size = None;
        for i in 0..MAX_RENDER_TARGETS {
            let rt = self.st.rts[i];
            if rt.texture.is_none() {
                continue;
            }
            match self.objects.get(&rt.texture.0) {
                Some(Object::Texture(tex)) if !tex.is_depth() && rt.level < tex.levels => {
                    t.colors[i] = Some(rt);
                    size.get_or_insert(tex.level_size(rt.level));
                }
                _ => {}
            }
        }
        let ds = self.st.ds;
        if !ds.texture.is_none() {
            if let Some(Object::Texture(tex)) = self.objects.get(&ds.texture.0) {
                if tex.is_depth() && ds.level < tex.levels {
                    t.depth = Some(ds);
                    size.get_or_insert(tex.level_size(ds.level));
                }
            }
        }
        let (w, h) = size?;
        t.width = w;
        t.height = h;
        Some(t)
    }

    /// Makes sure a render pass is open on the current targets.
    fn ensure_pass(&mut self) -> Option<PassTargets> {
        let targets = self.current_targets()?;
        if self.pass.is_some() && self.pass_targets == Some(targets) {
            return Some(targets);
        }
        self.end_pass();
        if let Some((ct, _)) = &self.clear {
            if *ct != targets {
                self.flush_clear();
            }
        }
        let clear = match self.clear.take() {
            Some((_, c)) => c,
            None => PendingClear::default(),
        };
        self.begin_pass(targets, clear);
        Some(targets)
    }

    fn begin_pass(&mut self, targets: PassTargets, clear: PendingClear) {
        let epoch = self.epoch;
        let mut colors: Vec<Option<(wgpu::TextureView, Option<wgpu::Color>)>> = Vec::new();
        for (i, c) in targets.colors.iter().enumerate() {
            colors.push(c.and_then(|c| match self.objects.get_mut(&c.texture.0) {
                Some(Object::Texture(t)) => {
                    t.last_use = epoch;
                    Some((t.rt_view(c.face, c.level), clear.colors[i]))
                }
                _ => None,
            }));
        }
        while colors.last().is_some_and(Option::is_none) {
            colors.pop();
        }
        let depth = targets.depth.and_then(|d| match self.objects.get_mut(&d.texture.0) {
            Some(Object::Texture(t)) => {
                t.last_use = epoch;
                Some((t.rt_view(d.face, d.level), t.format.has_stencil_aspect()))
            }
            _ => None,
        });
        let color_attachments: Vec<Option<wgpu::RenderPassColorAttachment>> = colors
            .iter()
            .map(|c| {
                c.as_ref().map(|(view, clear)| wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: match clear {
                            Some(c) => wgpu::LoadOp::Clear(*c),
                            None => wgpu::LoadOp::Load,
                        },
                        store: wgpu::StoreOp::Store,
                    },
                })
            })
            .collect();
        let depth_attachment = depth.as_ref().map(|(view, has_stencil)| wgpu::RenderPassDepthStencilAttachment {
            view,
            depth_ops: Some(wgpu::Operations {
                load: match clear.depth {
                    Some(z) => wgpu::LoadOp::Clear(z),
                    None => wgpu::LoadOp::Load,
                },
                store: wgpu::StoreOp::Store,
            }),
            stencil_ops: has_stencil.then_some(wgpu::Operations {
                load: match clear.stencil {
                    Some(s) => wgpu::LoadOp::Clear(s),
                    None => wgpu::LoadOp::Load,
                },
                store: wgpu::StoreOp::Store,
            }),
        });
        let pass = self
            .encoder()
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("d3dgpu pass"),
                color_attachments: &color_attachments,
                depth_stencil_attachment: depth_attachment,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            })
            .forget_lifetime();
        self.stats.passes += 1;
        self.pass = Some(pass);
        self.pass_targets = Some(targets);
        self.pc.reset();
    }

    /// Applies a pending load-op clear with an empty pass.
    fn flush_clear(&mut self) {
        if let Some((targets, clear)) = self.clear.take() {
            self.end_pass();
            self.begin_pass(targets, clear);
            self.end_pass();
        }
    }

    /// Ends recording and submits everything recorded so far.
    pub fn submit(&mut self) {
        self.end_pass();
        self.flush_clear();
        self.flush_clears11();
        if self.enc.is_none() && self.uniforms.is_empty() && self.streams.is_empty() && self.uploads.is_empty() {
            return;
        }
        self.uniforms.flush(&self.queue);
        self.streams.flush(&self.queue);
        self.uploads.flush(&self.queue);
        let enc = self.enc.take().unwrap_or_else(|| self.device.create_command_encoder(&Default::default()));
        let t = self.now_ns();
        self.queue.submit([enc.finish()]);
        self.stats.submit_ns += self.now_ns() - t;
        self.stats.submits += 1;
        self.epoch += 1;
        for w in std::mem::take(&mut self.presented) {
            if let Some(p) = self.presenters.get_mut(&w) {
                p.after_submit(&self.queue);
            }
        }
    }

    // ---- Clears ----

    fn clear(&mut self, flags: u32, color: u32, z: f32, stencil: u32, rects: &[Rect]) {
        let Some(targets) = self.current_targets() else {
            return self.warn("Clear with no render target or depth buffer bound");
        };
        let vp = self.st.viewport;
        let mut region = Rect::new(vp.x as i32, vp.y as i32, (vp.x + vp.width) as i32, (vp.y + vp.height) as i32);
        if self.st.r(RenderState::ScissorTestEnable) != 0 {
            region = intersect(region, self.st.scissor);
        }
        let full = Rect::new(0, 0, targets.width as i32, targets.height as i32);
        region = intersect(region, full);
        let regions: Vec<Rect> = if rects.is_empty() {
            vec![region]
        } else {
            rects.iter().map(|r| intersect(*r, region)).filter(|r| r.width() > 0 && r.height() > 0).collect()
        };
        if regions.is_empty() || region.width() == 0 || region.height() == 0 {
            return;
        }
        let has_stencil = targets.depth.is_some_and(|d| match self.objects.get(&d.texture.0) {
            Some(Object::Texture(t)) => t.format.has_stencil_aspect(),
            _ => false,
        });
        let mut want = PendingClear::default();
        if flags & clear::TARGET != 0 {
            for (i, c) in targets.colors.iter().enumerate() {
                if c.is_some() {
                    want.colors[i] = Some(convert::color(color));
                }
            }
        }
        if flags & clear::ZBUFFER != 0 && targets.depth.is_some() {
            want.depth = Some(z.clamp(0.0, 1.0));
        }
        if flags & clear::STENCIL != 0 && has_stencil {
            want.stencil = Some(stencil & 0xff);
        }
        if !want.any() {
            return;
        }
        let whole = regions.len() == 1 && regions[0] == full;
        // A whole-target clear of every attachment the pass would load
        // becomes the next pass's load op.
        let covers_all = targets.colors.iter().zip(want.colors).all(|(t, c)| t.is_none() || c.is_some())
            && (targets.depth.is_none() || (want.depth.is_some() && (!has_stencil || want.stencil.is_some())));
        if whole && (covers_all || self.pass.is_none()) {
            self.end_pass();
            let merged = match self.clear.take() {
                Some((t, mut c)) if t == targets => {
                    for i in 0..MAX_RENDER_TARGETS {
                        c.colors[i] = want.colors[i].or(c.colors[i]);
                    }
                    c.depth = want.depth.or(c.depth);
                    c.stencil = want.stencil.or(c.stencil);
                    c
                }
                Some(other) => {
                    self.clear = Some(other);
                    self.flush_clear();
                    want
                }
                None => want,
            };
            self.clear = Some((targets, merged));
            self.stats.load_op_clears += 1;
            return;
        }
        self.quad_clear(targets, &want, color, &regions);
    }

    fn quad_clear(&mut self, targets: PassTargets, want: &PendingClear, color: u32, regions: &[Rect]) {
        if self.ensure_pass().is_none() {
            return;
        }
        let mut formats = [None; MAX_RENDER_TARGETS];
        for (i, c) in targets.colors.iter().enumerate() {
            if let Some(c) = c {
                if let Some(Object::Texture(t)) = self.objects.get(&c.texture.0) {
                    formats[i] = Some((t.format, want.colors[i].is_some()));
                }
            }
        }
        let depth = targets.depth.and_then(|d| match self.objects.get(&d.texture.0) {
            Some(Object::Texture(t)) => Some(t.format),
            _ => None,
        });
        let key = draw::ClearKey {
            colors: formats,
            depth,
            write_depth: want.depth.is_some(),
            write_stencil: want.stencil.is_some(),
        };
        let pipeline = self.caches.clear_pipeline(&self.device, &key, &mut self.stats);
        let pass = self.pass.as_mut().unwrap();
        pass.set_pipeline(&pipeline);
        let c = convert::color(color);
        pass.set_blend_constant(c);
        let z = want.depth.unwrap_or(0.0);
        pass.set_viewport(0.0, 0.0, targets.width as f32, targets.height as f32, z, z);
        pass.set_stencil_reference(want.stencil.unwrap_or(0));
        for r in regions {
            pass.set_scissor_rect(r.x1 as u32, r.y1 as u32, r.width(), r.height());
            pass.draw(0..3, 0..1);
        }
        self.pc.reset();
        self.stats.quad_clears += 1;
    }

    // ---- Readback and fences ----

    fn read_texture(&mut self, region: &TextureRegion, dest_offset: u32, row_pitch: u32, fence: u64) {
        let state = Arc::new(AtomicU8::new(0));
        let Some(Object::Texture(t)) = self.objects.get(&region.texture.0) else {
            self.warn(format!("ReadTexture: {:?} is not a texture", region.texture));
            state.store(2, Ordering::Release);
            self.pending.push_back(Pending { fence, state, kind: PendingKind::Signal });
            return;
        };
        if !t.gpu.usage().contains(wgpu::TextureUsages::COPY_SRC) || t.format.is_compressed() {
            let f = t.desc.format;
            self.warn(format!("ReadTexture: {f:?} can't be read back yet"));
            state.store(2, Ordering::Release);
            self.pending.push_back(Pending { fence, state, kind: PendingKind::Signal });
            return;
        }
        let (format, plan, gpu) = (t.desc.format, t.plan, t.gpu.clone());
        let aspect = if t.is_depth() { wgpu::TextureAspect::DepthOnly } else { wgpu::TextureAspect::All };
        let layer = if t.desc.kind == TextureKind::Cube { region.face } else { region.z };
        let bpp = plan.gpu.bytes_per_block();
        let gpu_row_pitch = (region.width * bpp).next_multiple_of(256);
        let size = gpu_row_pitch as u64 * region.height.max(1) as u64;
        self.end_pass();
        self.flush_clear();
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("d3dgpu readback"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.encoder().copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &gpu,
                mip_level: region.level,
                origin: wgpu::Origin3d { x: region.x, y: region.y, z: layer },
                aspect,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(gpu_row_pitch),
                    rows_per_image: Some(region.height),
                },
            },
            wgpu::Extent3d { width: region.width, height: region.height, depth_or_array_layers: 1 },
        );
        self.submit();
        let s = state.clone();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |r| s.store(if r.is_ok() { 1 } else { 2 }, Ordering::Release));
        let read = ReadInfo {
            format,
            plan,
            width: region.width,
            height: region.height,
            gpu_row_pitch,
            dest_offset,
            row_pitch,
        };
        self.pending.push_back(Pending { fence, state, kind: PendingKind::Read { buffer, read } });
    }

    /// Finishes completed readbacks (writing them into `shared`) and
    /// advances the completed fence. Never blocks.
    pub fn poll(&mut self, shared: &mut [u8]) -> u64 {
        let _ = self.device.poll(wgpu::PollType::Poll);
        self.retire(shared);
        self.completed_fence
    }

    /// Blocks until all submitted work and readbacks are done (native only;
    /// in the browser, use [`Core::poll`] from the event loop).
    pub fn wait(&mut self, shared: &mut [u8]) -> u64 {
        self.submit();
        while !self.pending.is_empty() {
            let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
            if !self.retire(shared) {
                break;
            }
        }
        self.completed_fence
    }

    /// Retires finished pending items in order; returns whether any
    /// progress was made.
    fn retire(&mut self, shared: &mut [u8]) -> bool {
        let mut progress = false;
        while let Some(p) = self.pending.front() {
            let s = p.state.load(Ordering::Acquire);
            if s == 0 {
                break;
            }
            let p = self.pending.pop_front().unwrap();
            progress = true;
            match (&p.kind, s) {
                (PendingKind::Read { buffer, read }, 1) => self.finish_read(buffer, read, shared),
                (PendingKind::Raw { buffer, read }, 1) => self.finish_raw(buffer, read, shared),
                _ => {}
            }
            self.completed_fence = self.completed_fence.max(p.fence);
        }
        progress
    }

    fn finish_read(&mut self, buffer: &wgpu::Buffer, r: &ReadInfo, shared: &mut [u8]) {
        let data = match buffer.slice(..).get_mapped_range() {
            Ok(v) => v.to_vec(),
            Err(e) => return self.warn(format!("readback map failed: {e:?}")),
        };
        buffer.unmap();
        let Some(out) = fmt::convert_back(r.format, &r.plan, &data, r.width, r.height, r.gpu_row_pitch, r.row_pitch)
        else {
            return self.warn(format!("readback of {:?} has no conversion", r.format));
        };
        let start = r.dest_offset as usize;
        match shared.get_mut(start..start + out.len()) {
            Some(dst) => dst.copy_from_slice(&out),
            None => self.warn("readback destination is outside shared memory"),
        }
    }

    fn finish_raw(&mut self, buffer: &wgpu::Buffer, r: &RawRead, shared: &mut [u8]) {
        let data = match buffer.slice(..).get_mapped_range() {
            Ok(v) => v.to_vec(),
            Err(e) => return self.warn(format!("readback map failed: {e:?}")),
        };
        buffer.unmap();
        for z in 0..r.slices {
            for y in 0..r.rows {
                let src = z * r.gpu_slice_pitch + y * r.gpu_row_pitch;
                let dst = r.dest_offset as usize + z * r.depth_pitch as usize + y * r.row_pitch as usize;
                match (shared.get_mut(dst..dst + r.row_bytes), data.get(src..src + r.row_bytes)) {
                    (Some(d), Some(s)) => d.copy_from_slice(s),
                    _ => return self.warn("readback destination is outside shared memory"),
                }
            }
        }
    }

    // ---- Presentation ----

    fn ensure_presenter(&mut self, window: u32) {
        self.presenters.entry(window).or_insert_with(|| Box::new(HeadlessPresenter::new()));
    }

    fn present(&mut self, texture: Handle, window: u32) {
        self.end_pass();
        self.flush_clear();
        let (src, (w, h), alpha_is_one) = match self.objects.get_mut(&texture.0) {
            Some(Object::Texture(t)) => (t.rt_view(0, 0), t.level_size(0), t.plan.alpha_is_one()),
            Some(Object::Texture11(_)) => self.present_source11(texture).unwrap(),
            _ => return self.warn(format!("Present: {texture:?} is not a texture")),
        };
        self.ensure_presenter(window);
        let mut presenter = self.presenters.remove(&window).unwrap();
        {
            let enc = self.enc.get_or_insert_with(|| self.device.create_command_encoder(&Default::default()));
            let mut ctx = PresentContext {
                device: &self.device,
                queue: &self.queue,
                encoder: enc,
                blitter: &mut self.blitter,
                uniforms: &mut self.uniforms,
            };
            presenter.present(&mut ctx, &src, (w, h), alpha_is_one);
        }
        self.presenters.insert(window, presenter);
        self.presented.push(window);
        self.stats.presents += 1;
        self.submit();
    }

    fn stretch_rect(&mut self, src: Target, src_rect: Rect, dst: Target, dst_rect: Rect, filter: TextureFilter) {
        self.end_pass();
        self.flush_clear();
        let epoch = self.epoch;
        let (src_view, src_size, src_depth, filterable) = match self.objects.get_mut(&src.texture.0) {
            Some(Object::Texture(t)) => {
                t.last_use = epoch;
                let filterable = t.format.sample_type(None, Some(self.features))
                    == Some(wgpu::TextureSampleType::Float { filterable: true });
                (t.rt_view(src.face, src.level), t.level_size(src.level), t.is_depth(), filterable)
            }
            _ => return self.warn("StretchRect: bad source"),
        };
        let (dst_view, dst_size, dst_format) = match self.objects.get_mut(&dst.texture.0) {
            Some(Object::Texture(t)) if t.gpu.usage().contains(wgpu::TextureUsages::RENDER_ATTACHMENT) => {
                t.last_use = epoch;
                (t.rt_view(dst.face, dst.level), t.level_size(dst.level), t.format)
            }
            _ => return self.warn("StretchRect: destination is not a render target"),
        };
        if src_depth || dst_format.is_depth_stencil_format() {
            return self.warn("StretchRect between depth surfaces is not supported yet");
        }
        let full = |r: Rect, (w, h): (u32, u32)| if r.width() == 0 { Rect::new(0, 0, w as i32, h as i32) } else { r };
        let (sr, dr) = (full(src_rect, src_size), full(dst_rect, dst_size));
        let linear = filter == TextureFilter::Linear && filterable;
        let enc = self.enc.get_or_insert_with(|| self.device.create_command_encoder(&Default::default()));
        let mut ctx = PresentContext {
            device: &self.device,
            queue: &self.queue,
            encoder: enc,
            blitter: &mut self.blitter,
            uniforms: &mut self.uniforms,
        };
        let params = present::BlitParams {
            src_rect: sr,
            src_size,
            dst_rect: dr,
            linear,
            filterable,
            alpha_one: false,
            gamma: None,
        };
        ctx.blit(&src_view, &dst_view, dst_format, &params);
    }
}

pub(crate) fn intersect(a: Rect, b: Rect) -> Rect {
    let r = Rect::new(a.x1.max(b.x1), a.y1.max(b.y1), a.x2.min(b.x2), a.y2.min(b.y2));
    if r.x2 <= r.x1 || r.y2 <= r.y1 {
        Rect::new(r.x1, r.y1, r.x1, r.y1)
    } else {
        r
    }
}
