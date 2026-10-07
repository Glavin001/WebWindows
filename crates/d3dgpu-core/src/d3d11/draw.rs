//! Direct3D 11 draws and dispatches: shader variants, bind groups,
//! pipelines and the render pass.
//!
//! Translated shaders use three bind groups: 0 for the vertex (or compute)
//! shader's resources, 1 for the pixel shader's, 2 for the driver uniforms.
//! Constant buffers and the driver block are windows of the uniform ring
//! bound with dynamic offsets, so a draw that only changes constants reuses
//! its bind groups. Pass commands go through [`crate::pass_cache`], which
//! skips those that would set what is already set.

use std::sync::Arc;

use d3dgpu_dxbc::{BindingType, Clip, ComponentType, Key, SrvKind, TexDim, TexSample, UavKind};
use d3dgpu_proto::d3d11::*;
use d3dgpu_proto::Handle;

use super::{format, Stage11 as S, Targets11, ViewRes, MAX_RTS};
use crate::resources::{FastMap, Object};
use crate::{convert, Core};

pub enum Draw11 {
    Vertices { count: u32, start: u32, instances: u32, start_instance: u32 },
    Indexed { count: u32, start: u32, base_vertex: i32, instances: u32, start_instance: u32 },
}

pub struct Variant11 {
    pub module: wgpu::ShaderModule,
    pub t: d3dgpu_dxbc::Translation,
    pub id: u64,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct VariantKey {
    shader: u64,
    /// Vertex shaders: the pixel shader they feed (its linkage is part of
    /// the translation), `u64::MAX` for none.
    ps: u64,
    srvs: Vec<(u32, SrvKind)>,
    uavs: Vec<(u32, UavKind)>,
    clip: Clip,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum EntryKind {
    Uniform(u32),
    Texture { dim: wgpu::TextureViewDimension, sample: wgpu::TextureSampleType, ms: bool },
    Sampler(wgpu::SamplerBindingType),
    Storage { read_only: bool },
    StorageTexture { format: wgpu::TextureFormat, dim: wgpu::TextureViewDimension, access: wgpu::StorageTextureAccess },
}

pub struct Layout11 {
    id: u64,
    group: wgpu::BindGroupLayout,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct VertexBufferKey {
    stride: u32,
    instance: bool,
    attrs: Vec<(wgpu::VertexFormat, u32, u32)>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct ColorKey {
    format: wgpu::TextureFormat,
    blend: Option<(wgpu::BlendComponent, wgpu::BlendComponent)>,
    mask: u32,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct DepthKey {
    format: wgpu::TextureFormat,
    write: bool,
    compare: wgpu::CompareFunction,
    stencil: Option<(wgpu::StencilFaceState, wgpu::StencilFaceState, u32, u32)>,
    bias: i32,
    slope: u32,
    clamp: u32,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct PipelineKey {
    vs: u64,
    ps: u64,
    layout: (u64, u64),
    buffers: Vec<VertexBufferKey>,
    topology: wgpu::PrimitiveTopology,
    strip_index: Option<wgpu::IndexFormat>,
    cull: Option<wgpu::Face>,
    front_ccw: bool,
    unclipped_depth: bool,
    colors: Vec<Option<ColorKey>>,
    depth: Option<DepthKey>,
    samples: u32,
    sample_mask: u32,
    alpha_to_coverage: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct SamplerKey {
    desc: [u32; 12],
    filtering: bool,
}

type LayoutKey = (u32, Vec<(u32, EntryKind)>);
type BindGroupKey = (u64, Vec<[u64; 2]>);

pub struct Caches11 {
    variants: FastMap<VariantKey, Option<Arc<Variant11>>>,
    layouts: FastMap<LayoutKey, Arc<Layout11>>,
    pipeline_layouts: FastMap<(u64, u64, bool), wgpu::PipelineLayout>,
    pipelines: FastMap<PipelineKey, (u64, wgpu::RenderPipeline)>,
    compute: FastMap<(u64, u64), (u64, wgpu::ComputePipeline)>,
    samplers: FastMap<SamplerKey, (u64, wgpu::Sampler)>,
    bind_groups: FastMap<BindGroupKey, (u64, wgpu::BindGroup)>,
    empty: Option<(Arc<Layout11>, (u64, wgpu::BindGroup))>,
    driver_layout: wgpu::BindGroupLayout,
    driver_group: wgpu::BindGroup,
    driver: ([u8; 32], u64, u64),
    zero_cb: (u64, u64, u32),
    zero_buffer: wgpu::Buffer,
    vertex_layouts: FastMap<Vec<(u32, VertexBufferKey)>, u64>,
    /// Pipelines by the identities they depend on (state objects are
    /// immutable), so switching between a few states skips the full key.
    fast_pipelines: FastMap<FastKey, (PipelineKey, (u64, wgpu::RenderPipeline))>,
    dummies: FastMap<(wgpu::TextureViewDimension, u8, bool), (wgpu::TextureView, u64)>,
    dummy_storage: FastMap<(wgpu::TextureFormat, wgpu::TextureViewDimension), (wgpu::TextureView, u64)>,
}

const DRIVER_SIZE: u64 = d3dgpu_dxbc::wgsl::DRIVER_SIZE;
const ZERO_SIZE: u64 = 4096;
// Pass cache ids of the core-lifetime objects (see crate::draw).
const DRIVER_GROUP_ID: u64 = u64::MAX - 4;
const ZERO_BUFFER_ID: u64 = u64::MAX - 5;

impl Caches11 {
    /// Drops what is keyed by handles (after a handle is destroyed and may
    /// be reused for another object).
    pub fn forget_handles(&mut self) {
        self.fast_pipelines.clear();
    }

    pub fn new(device: &wgpu::Device, uniform_ring: &wgpu::Buffer) -> Caches11 {
        let driver_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("d3dgpu driver11"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(DRIVER_SIZE),
                },
                count: None,
            }],
        });
        let driver_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("d3dgpu driver11"),
            layout: &driver_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: uniform_ring,
                    offset: 0,
                    size: wgpu::BufferSize::new(DRIVER_SIZE),
                }),
            }],
        });
        let zero_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("d3dgpu zero11"),
            size: ZERO_SIZE,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        Caches11 {
            variants: FastMap::default(),
            layouts: FastMap::default(),
            pipeline_layouts: FastMap::default(),
            pipelines: FastMap::default(),
            compute: FastMap::default(),
            samplers: FastMap::default(),
            bind_groups: FastMap::default(),
            empty: None,
            driver_layout,
            driver_group,
            driver: ([0; 32], 0, u64::MAX),
            zero_cb: (u64::MAX, 0, 0),
            zero_buffer,
            vertex_layouts: FastMap::default(),
            fast_pipelines: FastMap::default(),
            dummies: FastMap::default(),
            dummy_storage: FastMap::default(),
        }
    }
}

/// Why a draw didn't happen.
enum Fail {
    /// The uniform ring is full: submit and try again.
    Retry,
    Skip(String),
    /// Nothing would be drawn (an empty viewport); not an error.
    Nothing,
}

fn skip<T>(msg: impl Into<String>) -> Result<T, Fail> {
    Err(Fail::Skip(msg.into()))
}

/// A resolved binding.
enum Res {
    Ring(u32),
    View(wgpu::TextureView),
    Sampler(wgpu::Sampler),
    Buffer(wgpu::Buffer, u64, u64, u64),
}

/// A resolved bind group, and the constant buffer windows (slot, size)
/// whose ring offsets are its dynamic offsets, in binding order.
#[derive(Clone)]
struct Bound {
    layout: Arc<Layout11>,
    group: (u64, wgpu::BindGroup),
    cbs: Vec<(u32, u32)>,
}

/// What a draw derives from the bound state, kept until a command marks
/// a part of it dirty (see [`super::dirty`]).
pub struct Prepared {
    targets: Targets11,
    vs: Arc<super::Shader11>,
    vsv: Arc<Variant11>,
    ps: Option<Arc<super::Shader11>>,
    psv: Option<Arc<Variant11>>,
    g0: Bound,
    g1: Bound,
    slots: Vec<(u32, VertexBufferKey)>,
    vertex_id: u64,
    fast: FastKey,
    key: PipelineKey,
    pipeline: (u64, wgpu::RenderPipeline),
    scissor: bool,
}

/// Everything the pipeline depends on, by identity: when it is unchanged
/// the pipeline is too, without building and hashing the full key.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct FastKey {
    vs: u64,
    ps: u64,
    layouts: (u64, u64),
    vertex: u64,
    blend: u64,
    depth_stencil: u64,
    rasterizer: u64,
    topology: u32,
    targets: Targets11,
    sample_mask: u32,
}

fn view_dim(d: TexDim) -> wgpu::TextureViewDimension {
    match d {
        TexDim::D2 => wgpu::TextureViewDimension::D2,
        TexDim::D2Array => wgpu::TextureViewDimension::D2Array,
        TexDim::D3 => wgpu::TextureViewDimension::D3,
        TexDim::Cube => wgpu::TextureViewDimension::Cube,
        TexDim::CubeArray => wgpu::TextureViewDimension::CubeArray,
    }
}

fn topology(t: u32) -> Option<wgpu::PrimitiveTopology> {
    use wgpu::PrimitiveTopology as P;
    Some(match t {
        1 => P::PointList,
        2 => P::LineList,
        3 => P::LineStrip,
        4 => P::TriangleList,
        5 => P::TriangleStrip,
        _ => return None,
    })
}

impl Core {
    fn skip11(&mut self, why: String) {
        self.stats.skipped_draws += 1;
        self.warn_once(why);
    }

    pub(crate) fn draw11(&mut self, d: Draw11) {
        self.stats.draws += 1;
        for attempt in 0..2 {
            match self.try_draw11(&d) {
                Ok(()) | Err(Fail::Nothing) => return,
                Err(Fail::Retry) if attempt == 0 => self.submit(),
                Err(Fail::Retry) => return self.skip11("a draw's constants don't fit in the uniform ring".into()),
                Err(Fail::Skip(why)) => return self.skip11(why),
            }
        }
    }

    pub(crate) fn dispatch11(&mut self, x: u32, y: u32, z: u32) {
        if x == 0 || y == 0 || z == 0 {
            return;
        }
        self.flush_clears11();
        for attempt in 0..2 {
            match self.try_dispatch11(x, y, z) {
                Ok(()) | Err(Fail::Nothing) => return,
                Err(Fail::Retry) if attempt == 0 => self.submit(),
                Err(Fail::Retry) => return self.warn("a dispatch's constants don't fit in the uniform ring"),
                Err(Fail::Skip(why)) => return self.warn_once(why),
            }
        }
    }

    fn shader11(&self, stage: S) -> Option<Arc<super::Shader11>> {
        match self.objects.get(&self.d11.st.shaders[stage as usize].0) {
            Some(Object::Shader11(s)) => Some(s.clone()),
            _ => None,
        }
    }

    /// The attachments the bound views describe.
    fn targets11(&mut self) -> Result<Targets11, Fail> {
        let st = &self.d11.st;
        let mut t = Targets11::default();
        let mut size = None;
        let mut samples = None;
        let mut mismatch = false;
        for (i, rtv) in st.rtvs.iter().enumerate() {
            if let Some(Object::View11(v)) = self.objects.get(&rtv.0) {
                if let (ViewKind::RenderTarget, ViewRes::Texture { size: s, samples: n, .. }) = (v.kind, &v.res) {
                    mismatch |= size.is_some_and(|z| z != *s) || samples.is_some_and(|z| z != *n);
                    size.get_or_insert(*s);
                    samples.get_or_insert(*n);
                    t.colors[i] = *rtv;
                }
            }
        }
        if let Some(Object::View11(v)) = self.objects.get(&st.dsv.0) {
            if let (ViewKind::DepthStencil, ViewRes::Texture { size: s, samples: n, .. }) = (v.kind, &v.res) {
                mismatch |= size.is_some_and(|z| z != *s) || samples.is_some_and(|z| z != *n);
                size.get_or_insert(*s);
                samples.get_or_insert(*n);
                t.depth = st.dsv;
                t.depth_read_only = v.desc.flags & view_flags::READ_ONLY_DEPTH != 0;
                t.stencil_read_only = v.desc.flags & view_flags::READ_ONLY_STENCIL != 0;
            }
        }
        if mismatch {
            return skip("render targets of different sizes or sample counts");
        }
        let Some((w, h)) = size else { return skip("draw with no render target or depth-stencil view bound") };
        t.width = w;
        t.height = h;
        t.samples = samples.unwrap_or(1);
        Ok(t)
    }

    /// Opens a render pass on `targets` unless it is the open one.
    fn ensure_pass11(&mut self, targets: Targets11) {
        if self.pass.is_some() && self.d11.pass == Some(targets) {
            return;
        }
        let blocks = |v: &u32| {
            let h = Handle(*v);
            !(targets.colors.contains(&h)
                || (targets.depth == h && !targets.depth_read_only && !targets.stencil_read_only))
        };
        if self.d11.clears.keys().any(blocks) {
            self.flush_clears11();
        }
        self.end_pass();
        self.flush_clear();
        let epoch = self.epoch;
        let mut colors: Vec<Option<(wgpu::TextureView, Option<wgpu::Color>)>> = Vec::new();
        let mut touched = Vec::new();
        for c in targets.colors {
            let clear = self.d11.clears.remove(&c.0).and_then(|c| c.color);
            colors.push(match self.objects.get(&c.0) {
                Some(Object::View11(v)) => match &v.res {
                    ViewRes::Texture { view, .. } => {
                        touched.push(v.resource);
                        Some((view.clone(), clear))
                    }
                    _ => None,
                },
                _ => None,
            });
        }
        while colors.last().is_some_and(Option::is_none) {
            colors.pop();
        }
        let depth = match self.objects.get(&targets.depth.0) {
            Some(Object::View11(v)) => match &v.res {
                ViewRes::Texture { view, format, .. } => {
                    touched.push(v.resource);
                    Some((view.clone(), *format))
                }
                _ => None,
            },
            _ => None,
        };
        let dclear = self.d11.clears.remove(&targets.depth.0).unwrap_or_default();
        for r in touched {
            if let Some(Object::Texture11(t)) = self.objects.get_mut(&r.0) {
                t.last_use = epoch;
            }
        }
        let color_attachments: Vec<Option<wgpu::RenderPassColorAttachment>> = colors
            .iter()
            .map(|c| {
                c.as_ref().map(|(view, clear)| wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: clear.map(wgpu::LoadOp::Clear).unwrap_or(wgpu::LoadOp::Load),
                        store: wgpu::StoreOp::Store,
                    },
                })
            })
            .collect();
        let depth_attachment = depth.as_ref().map(|(view, format)| wgpu::RenderPassDepthStencilAttachment {
            view,
            depth_ops: (!targets.depth_read_only).then_some(wgpu::Operations {
                load: dclear.depth.map(wgpu::LoadOp::Clear).unwrap_or(wgpu::LoadOp::Load),
                store: wgpu::StoreOp::Store,
            }),
            stencil_ops: (format.has_stencil_aspect() && !targets.stencil_read_only).then_some(wgpu::Operations {
                load: dclear.stencil.map(wgpu::LoadOp::Clear).unwrap_or(wgpu::LoadOp::Load),
                store: wgpu::StoreOp::Store,
            }),
        });
        let pass = self
            .encoder()
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("d3dgpu pass11"),
                color_attachments: &color_attachments,
                depth_stencil_attachment: depth_attachment,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            })
            .forget_lifetime();
        self.stats.passes += 1;
        self.pass = Some(pass);
        self.d11.pass = Some(targets);
        self.pc.reset();
    }

    // ---- Shader variants ----

    fn variant11(
        &mut self,
        sh: &Arc<super::Shader11>,
        stage: S,
        ps: Option<&Arc<super::Shader11>>,
        clip: Clip,
    ) -> Result<Arc<Variant11>, Fail> {
        let si = stage as usize;
        let mut srvs = Vec::new();
        for d in &sh.shader.reflection.resources {
            let Some(Object::View11(v)) = self.objects.get(&self.d11.st.srvs[si][d.slot as usize % super::MAX_SRVS].0)
            else {
                continue;
            };
            match &v.res {
                ViewRes::Buffer { format: Some(f), .. } if *f != d3dgpu_dxbc::BufferFormat::R32Uint => {
                    srvs.push((d.slot, SrvKind::Buffer(*f)))
                }
                ViewRes::Texture { format, .. } if format.is_depth_stencil_format() => {
                    if matches!(format, wgpu::TextureFormat::Stencil8)
                        || format::view_aspect(v.desc.format) == wgpu::TextureAspect::StencilOnly
                    {
                        continue;
                    }
                    srvs.push((d.slot, SrvKind::Texture { depth: true }))
                }
                _ => {}
            }
        }
        let mut uavs = Vec::new();
        for d in &sh.shader.reflection.uavs {
            let Some(Object::View11(v)) = self.objects.get(&self.d11.st.uavs[si][d.slot as usize % super::MAX_UAVS].0)
            else {
                continue;
            };
            match &v.res {
                ViewRes::Buffer { format: Some(f), .. } => uavs.push((d.slot, UavKind::Buffer(*f))),
                ViewRes::Texture { format, .. } => match format::storage_format(*format) {
                    Some(f) => uavs.push((d.slot, UavKind::Texture(f))),
                    None => return skip(format!("{format:?} can't be a storage texture on WebGPU")),
                },
                _ => {}
            }
        }
        let key = VariantKey {
            shader: sh.id,
            ps: if stage == S::Vertex { ps.map(|p| p.id).unwrap_or(u64::MAX) } else { 0 },
            srvs,
            uavs,
            clip,
        };
        if let Some(v) = self.d11.caches.variants.get(&key) {
            return match v {
                Some(v) => Ok(v.clone()),
                None => skip(format!("shader {:016x} failed to translate", sh.hash)),
            };
        }
        let dkey = Key {
            srvs: key.srvs.clone(),
            uavs: key.uavs.clone(),
            outputs: (stage == S::Vertex).then(|| ps.map(|p| p.linkage.clone()).unwrap_or_default()),
            clip,
            pos_fixup: stage == S::Vertex,
        };
        self.stats.shader_translations += 1;
        let v = match sh.shader.translate(&dkey) {
            Ok(t) => {
                let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("d3dgpu shader11"),
                    source: wgpu::ShaderSource::Wgsl(t.wgsl.as_str().into()),
                });
                Some(Arc::new(Variant11 { module, t, id: self.id() }))
            }
            Err(e) => {
                self.warn(format!("shader {:016x} ({stage:?}): {e}", sh.hash));
                None
            }
        };
        self.d11.caches.variants.insert(key, v.clone());
        v.ok_or_else(|| Fail::Skip(format!("shader {:016x} failed to translate", sh.hash)))
    }

    // ---- Uniforms ----

    fn driver11(&mut self, bytes: [u8; 32]) -> Result<u64, Fail> {
        let c = &self.d11.caches.driver;
        if c.2 == self.epoch && c.0 == bytes {
            return Ok(c.1);
        }
        let off = self.uniforms.alloc(&bytes, 256).ok_or(Fail::Retry)?;
        self.d11.caches.driver = (bytes, off, self.epoch);
        Ok(off)
    }

    /// The ring offset of a constant buffer window of `size` bytes.
    fn cb_window(&mut self, stage: usize, slot: u32, size: u32) -> Result<u64, Fail> {
        let epoch = self.epoch;
        let b = self.d11.st.cbs[stage].get(slot as usize).copied().unwrap_or_default();
        let Some(Object::Buffer11(buf)) = self.objects.get_mut(&b.buffer.0) else {
            let z = self.d11.caches.zero_cb;
            if z.0 == epoch && z.2 >= size {
                return Ok(z.1);
            }
            let (off, dst) = self.uniforms.reserve(size as usize, 256).ok_or(Fail::Retry)?;
            dst.fill(0);
            self.d11.caches.zero_cb = (epoch, off, size);
            return Ok(off);
        };
        let start = b.first_constant * 16;
        if let Some((ver, s, sz, e, off)) = buf.ring {
            if ver == buf.version && s == start && sz >= size && e == epoch {
                return Ok(off);
            }
        }
        let avail = (buf.size.saturating_sub(start) as usize).min(if b.num_constants == 0 {
            usize::MAX
        } else {
            b.num_constants as usize * 16
        });
        let n = avail.min(size as usize);
        let (off, dst) = self.uniforms.reserve(size as usize, 256).ok_or(Fail::Retry)?;
        dst[..n].copy_from_slice(&buf.shadow[start as usize..start as usize + n]);
        dst[n..].fill(0);
        buf.ring = Some((buf.version, start, size, epoch, off));
        Ok(off)
    }

    // ---- Bindings ----

    fn layout11(&mut self, vis: wgpu::ShaderStages, entries: Vec<(u32, EntryKind)>) -> Arc<Layout11> {
        let key = (vis.bits(), entries);
        if let Some(l) = self.d11.caches.layouts.get(&key) {
            return l.clone();
        }
        let wentries: Vec<wgpu::BindGroupLayoutEntry> = key
            .1
            .iter()
            .map(|(binding, k)| wgpu::BindGroupLayoutEntry {
                binding: *binding,
                visibility: vis,
                ty: match k {
                    EntryKind::Uniform(size) => wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(*size as u64),
                    },
                    EntryKind::Texture { dim, sample, ms } => {
                        wgpu::BindingType::Texture { sample_type: *sample, view_dimension: *dim, multisampled: *ms }
                    }
                    EntryKind::Sampler(t) => wgpu::BindingType::Sampler(*t),
                    EntryKind::Storage { read_only } => wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: *read_only },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    EntryKind::StorageTexture { format, dim, access } => {
                        wgpu::BindingType::StorageTexture { access: *access, format: *format, view_dimension: *dim }
                    }
                },
                count: None,
            })
            .collect();
        let group = self.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("d3dgpu layout11"),
            entries: &wentries,
        });
        let l = Arc::new(Layout11 { id: self.id(), group });
        self.d11.caches.layouts.insert(key, l.clone());
        l
    }

    fn empty11(&mut self) -> (Arc<Layout11>, (u64, wgpu::BindGroup)) {
        if let Some(e) = &self.d11.caches.empty {
            return e.clone();
        }
        let l = self.layout11(wgpu::ShaderStages::empty(), Vec::new());
        let g = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("d3dgpu empty11"),
            layout: &l.group,
            entries: &[],
        });
        let e = (l, (self.id(), g));
        self.d11.caches.empty = Some(e.clone());
        e
    }

    fn dummy_texture(&mut self, dim: wgpu::TextureViewDimension, sample: u8, ms: bool) -> (wgpu::TextureView, u64) {
        if let Some(d) = self.d11.caches.dummies.get(&(dim, sample, ms)) {
            return d.clone();
        }
        use wgpu::TextureViewDimension as D;
        let format = match sample {
            1 => wgpu::TextureFormat::Depth32Float,
            2 => wgpu::TextureFormat::Rgba8Uint,
            3 => wgpu::TextureFormat::Rgba8Sint,
            _ => wgpu::TextureFormat::Rgba8Unorm,
        };
        let layers = if matches!(dim, D::Cube | D::CubeArray) { 6 } else { 1 };
        let mut usage = wgpu::TextureUsages::TEXTURE_BINDING;
        if ms || sample == 1 {
            usage |= wgpu::TextureUsages::RENDER_ATTACHMENT;
        }
        let t = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("d3dgpu unbound11"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: layers },
            mip_level_count: 1,
            sample_count: if ms { 4 } else { 1 },
            dimension: if dim == D::D3 { wgpu::TextureDimension::D3 } else { wgpu::TextureDimension::D2 },
            format,
            usage,
            view_formats: &[],
        });
        let v = t.create_view(&wgpu::TextureViewDescriptor { dimension: Some(dim), ..Default::default() });
        let d = (v, self.id());
        self.d11.caches.dummies.insert((dim, sample, ms), d.clone());
        d
    }

    fn dummy_storage(
        &mut self,
        format: wgpu::TextureFormat,
        dim: wgpu::TextureViewDimension,
    ) -> (wgpu::TextureView, u64) {
        if let Some(d) = self.d11.caches.dummy_storage.get(&(format, dim)) {
            return d.clone();
        }
        let t = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("d3dgpu unbound uav11"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: if dim == wgpu::TextureViewDimension::D3 {
                wgpu::TextureDimension::D3
            } else {
                wgpu::TextureDimension::D2
            },
            format,
            usage: wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[],
        });
        let v = t.create_view(&wgpu::TextureViewDescriptor { dimension: Some(dim), ..Default::default() });
        let d = (v, self.id());
        self.d11.caches.dummy_storage.insert((format, dim), d.clone());
        d
    }

    fn sampler11(&mut self, h: Handle, filtering: bool, comparison: bool) -> (wgpu::Sampler, u64) {
        let desc = match self.objects.get(&h.0) {
            Some(Object::Sampler11(d)) => *d,
            _ => SamplerDesc11::default(),
        };
        let raw = [
            desc.filter,
            desc.address[0],
            desc.address[1],
            desc.address[2],
            desc.max_anisotropy,
            if comparison { desc.comparison } else { 0 },
            desc.min_lod.to_bits(),
            desc.max_lod.to_bits(),
            0,
            0,
            0,
            0,
        ];
        let key = SamplerKey { desc: raw, filtering };
        if let Some((id, s)) = self.d11.caches.samplers.get(&key) {
            return (s.clone(), *id);
        }
        let lin = |bit: u32| {
            if filtering && desc.filter & bit != 0 {
                wgpu::FilterMode::Linear
            } else {
                wgpu::FilterMode::Nearest
            }
        };
        let aniso = filtering && desc.filter & 0x40 != 0 && desc.max_anisotropy > 1;
        let (mag, min, mip) = if aniso {
            (wgpu::FilterMode::Linear, wgpu::FilterMode::Linear, wgpu::MipmapFilterMode::Linear)
        } else {
            let mip = if filtering && desc.filter & filter::MIP_LINEAR != 0 {
                wgpu::MipmapFilterMode::Linear
            } else {
                wgpu::MipmapFilterMode::Nearest
            };
            (lin(filter::MAG_LINEAR), lin(filter::MIN_LINEAR), mip)
        };
        let address = |a: u32| match a {
            1 => wgpu::AddressMode::Repeat,
            2 => wgpu::AddressMode::MirrorRepeat,
            _ => wgpu::AddressMode::ClampToEdge,
        };
        let lod_min = desc.min_lod.clamp(0.0, 32.0);
        let s = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("d3dgpu sampler11"),
            address_mode_u: address(desc.address[0]),
            address_mode_v: address(desc.address[1]),
            address_mode_w: address(desc.address[2]),
            mag_filter: mag,
            min_filter: min,
            mipmap_filter: mip,
            lod_min_clamp: lod_min,
            lod_max_clamp: desc.max_lod.clamp(lod_min, 32.0),
            compare: comparison.then(|| convert::compare(desc.comparison)),
            anisotropy_clamp: if aniso { desc.max_anisotropy.min(16) as u16 } else { 1 },
            border_color: None,
        });
        self.stats.samplers_created += 1;
        let id = self.id();
        self.d11.caches.samplers.insert(key, (id, s.clone()));
        (s, id)
    }

    /// Resolves one stage's bindings into a bind group.
    fn bind_stage11(
        &mut self,
        stage: S,
        sh: &super::Shader11,
        v: &Variant11,
        group: u32,
        targets: Option<&Targets11>,
    ) -> Result<Bound, Fail> {
        let si = stage as usize;
        let vis = match stage {
            S::Pixel => wgpu::ShaderStages::FRAGMENT,
            S::Compute => wgpu::ShaderStages::COMPUTE,
            _ => wgpu::ShaderStages::VERTEX,
        };
        let mut bindings: Vec<d3dgpu_dxbc::Binding> =
            v.t.bindings.iter().filter(|b| b.group == group).copied().collect();
        bindings.sort_by_key(|b| b.binding);
        let epoch = self.epoch;
        let features = self.features;
        let mut entries = Vec::with_capacity(bindings.len());
        let mut res = Vec::with_capacity(bindings.len());
        let mut ids = Vec::with_capacity(bindings.len());
        let mut cbs = Vec::new();
        let mut unfilterable: Vec<u32> = Vec::new();
        for b in &bindings {
            match b.ty {
                BindingType::ConstantBuffer { size } => {
                    cbs.push((b.slot, size));
                    entries.push((b.binding, EntryKind::Uniform(size)));
                    res.push(Res::Ring(size));
                    ids.push([0, size as u64]);
                }
                BindingType::Texture { dim, sample, multisampled } => {
                    let want = view_dim(dim);
                    let h = self.d11.st.srvs[si][b.slot as usize % super::MAX_SRVS];
                    let mut picked = None;
                    if let Some(Object::View11(view)) = self.objects.get(&h.0) {
                        if let ViewRes::Texture { view: tv, format, dim: vd, aspect, samples, .. } = &view.res {
                            let hazard = targets.is_some_and(|t| {
                                let bound_as = |vh: Handle| match self.objects.get(&vh.0) {
                                    Some(Object::View11(o)) => o.resource == view.resource,
                                    _ => false,
                                };
                                t.colors.iter().any(|c| bound_as(*c))
                                    || (bound_as(t.depth) && !(t.depth_read_only && t.stencil_read_only))
                            });
                            let st = format.sample_type(Some(*aspect), Some(features));
                            let ok_sample = matches!(
                                (sample, st),
                                (TexSample::Depth, Some(wgpu::TextureSampleType::Depth))
                                    | (TexSample::Float, Some(wgpu::TextureSampleType::Float { .. }))
                                    | (TexSample::Uint, Some(wgpu::TextureSampleType::Uint))
                                    | (TexSample::Sint, Some(wgpu::TextureSampleType::Sint))
                            );
                            if hazard {
                                self.warn_once("a texture is bound as a shader resource and a render target in the same draw; reading zeros instead");
                            } else if *vd == want && ok_sample && (*samples > 1) == multisampled {
                                picked = Some((tv.clone(), view.id, st.unwrap(), view.resource));
                            } else if !h.is_none() {
                                self.warn_once(format!(
                                    "shader resource {} ({vd:?}, {format:?}) doesn't match the shader's {want:?} {sample:?}",
                                    b.slot
                                ));
                            }
                        }
                    }
                    let (tv, id, st) = match picked {
                        Some((tv, id, st, r)) => {
                            if let Some(Object::Texture11(t)) = self.objects.get_mut(&r.0) {
                                t.last_use = epoch;
                            }
                            (tv, id, st)
                        }
                        None => {
                            let code = match sample {
                                TexSample::Float => 0,
                                TexSample::Depth => 1,
                                TexSample::Uint => 2,
                                TexSample::Sint => 3,
                            };
                            let (tv, id) = self.dummy_texture(want, code, multisampled);
                            let st = match sample {
                                TexSample::Depth => wgpu::TextureSampleType::Depth,
                                TexSample::Uint => wgpu::TextureSampleType::Uint,
                                TexSample::Sint => wgpu::TextureSampleType::Sint,
                                TexSample::Float => wgpu::TextureSampleType::Float { filterable: !multisampled },
                            };
                            (tv, id, st)
                        }
                    };
                    let st = match st {
                        wgpu::TextureSampleType::Float { .. } if multisampled => {
                            wgpu::TextureSampleType::Float { filterable: false }
                        }
                        s => s,
                    };
                    if st != (wgpu::TextureSampleType::Float { filterable: true }) {
                        unfilterable.push(b.slot);
                    }
                    entries.push((b.binding, EntryKind::Texture { dim: want, sample: st, ms: multisampled }));
                    res.push(Res::View(tv));
                    ids.push([id, 0]);
                }
                BindingType::Sampler { comparison } => {
                    let filtering = !comparison
                        && !sh.shader.reflection.pairs.iter().any(|(t, s)| *s == b.slot && unfilterable.contains(t));
                    let h = self.d11.st.samplers[si][b.slot as usize % super::MAX_SAMPLERS];
                    let (s, id) = self.sampler11(h, filtering, comparison);
                    let ty = if comparison {
                        wgpu::SamplerBindingType::Comparison
                    } else if filtering {
                        wgpu::SamplerBindingType::Filtering
                    } else {
                        wgpu::SamplerBindingType::NonFiltering
                    };
                    entries.push((b.binding, EntryKind::Sampler(ty)));
                    res.push(Res::Sampler(s));
                    ids.push([id, 1]);
                }
                BindingType::StorageBuffer { read_only } => {
                    let h = if b.binding >= d3dgpu_dxbc::wgsl::UAV_BASE {
                        self.d11.st.uavs[si][b.slot as usize % super::MAX_UAVS]
                    } else {
                        self.d11.st.srvs[si][b.slot as usize % super::MAX_SRVS]
                    };
                    let mut picked = None;
                    let view = match self.objects.get(&h.0) {
                        Some(Object::View11(view)) => match view.res {
                            ViewRes::Buffer { offset, size, .. } => Some((view.resource, offset, size)),
                            _ => None,
                        },
                        _ => None,
                    };
                    if let Some((resource, offset, size)) = view {
                        {
                            if let Some(Object::Buffer11(buf)) = self.objects.get_mut(&resource.0) {
                                if let Some(gpu) = &buf.gpu {
                                    buf.last_use = epoch;
                                    if !read_only {
                                        buf.gpu_written = true;
                                    }
                                    let size = size.min(gpu.size().saturating_sub(offset));
                                    picked = Some(Res::Buffer(gpu.clone(), buf.gen, offset, size));
                                }
                            }
                        }
                    }
                    let r = picked.unwrap_or_else(|| Res::Buffer(self.d11.caches.zero_buffer.clone(), 0, 0, ZERO_SIZE));
                    if let Res::Buffer(_, gen, off, size) = &r {
                        ids.push([*gen, off | size << 32]);
                    }
                    entries.push((b.binding, EntryKind::Storage { read_only }));
                    res.push(r);
                }
                BindingType::StorageTexture { format: sf, dim, read, write } => {
                    let want_dim = view_dim(dim);
                    let want_format = format::storage_texture_format(sf);
                    let h = self.d11.st.uavs[si][b.slot as usize % super::MAX_UAVS];
                    let mut picked = None;
                    if let Some(Object::View11(view)) = self.objects.get(&h.0) {
                        if let ViewRes::Texture { view: tv, format, dim: vd, .. } = &view.res {
                            if *format == want_format && *vd == want_dim {
                                picked = Some((tv.clone(), view.id, view.resource));
                            }
                        }
                    }
                    let (tv, id) = match picked {
                        Some((tv, id, r)) => {
                            if let Some(Object::Texture11(t)) = self.objects.get_mut(&r.0) {
                                t.last_use = epoch;
                            }
                            (tv, id)
                        }
                        None => self.dummy_storage(want_format, want_dim),
                    };
                    let access = match (read, write) {
                        (true, true) => wgpu::StorageTextureAccess::ReadWrite,
                        (true, false) => wgpu::StorageTextureAccess::ReadOnly,
                        _ => wgpu::StorageTextureAccess::WriteOnly,
                    };
                    entries.push((b.binding, EntryKind::StorageTexture { format: want_format, dim: want_dim, access }));
                    res.push(Res::View(tv));
                    ids.push([id, 2]);
                }
            }
        }
        let layout = self.layout11(vis, entries);
        let key = (layout.id, ids);
        if let Some(g) = self.d11.caches.bind_groups.get(&key) {
            return Ok(Bound { layout, group: g.clone(), cbs });
        }
        let ring = self.uniforms.buffer.clone();
        let wentries: Vec<wgpu::BindGroupEntry> = bindings
            .iter()
            .zip(&res)
            .map(|(b, r)| wgpu::BindGroupEntry {
                binding: b.binding,
                resource: match r {
                    Res::Ring(size) => wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &ring,
                        offset: 0,
                        size: wgpu::BufferSize::new(*size as u64),
                    }),
                    Res::View(v) => wgpu::BindingResource::TextureView(v),
                    Res::Sampler(s) => wgpu::BindingResource::Sampler(s),
                    Res::Buffer(buf, _, off, size) => wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: buf,
                        offset: *off,
                        size: wgpu::BufferSize::new(*size),
                    }),
                },
            })
            .collect();
        let g = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("d3dgpu group11"),
            layout: &layout.group,
            entries: &wentries,
        });
        self.stats.bind_groups_created += 1;
        if self.d11.caches.bind_groups.len() > 4096 {
            self.d11.caches.bind_groups.clear();
        }
        let g = (self.id(), g);
        self.d11.caches.bind_groups.insert(key, g.clone());
        Ok(Bound { layout, group: g, cbs })
    }

    /// The dynamic offsets of a bound group: its constant buffer windows.
    fn cb_offsets(&mut self, stage: S, b: &Bound) -> Result<Vec<u32>, Fail> {
        let mut out = Vec::with_capacity(b.cbs.len());
        for (slot, size) in &b.cbs {
            out.push(self.cb_window(stage as usize, *slot, *size)? as u32);
        }
        Ok(out)
    }

    fn pipeline_layout11(&mut self, g0: &Layout11, g1: &Layout11, compute: bool) -> wgpu::PipelineLayout {
        let key = (g0.id, g1.id, compute);
        if let Some(l) = self.d11.caches.pipeline_layouts.get(&key) {
            return l.clone();
        }
        let l = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("d3dgpu pipeline layout11"),
            bind_group_layouts: &[Some(&g0.group), Some(&g1.group), Some(&self.d11.caches.driver_layout)],
            immediate_size: 0,
        });
        self.d11.caches.pipeline_layouts.insert(key, l.clone());
        l
    }

    // ---- Draw ----

    /// Derives what the dirty parts of the state decide: targets, shader
    /// variants, bind groups, vertex layout, pipeline.
    fn prepare11(&mut self) -> Result<Arc<Prepared>, Fail> {
        use super::dirty::*;
        let prev = self.d11.prepared.take();
        let dirty = if prev.is_some() { self.d11.dirty } else { ALL };
        for s in [S::Geometry, S::Hull, S::Domain] {
            if !self.d11.st.shaders[s as usize].is_none() {
                return skip("geometry and tessellation shaders are not supported yet");
            }
        }
        let targets = match &prev {
            Some(p) if dirty & TARGETS == 0 => p.targets,
            _ => self.targets11()?,
        };
        let (vs, ps) = match &prev {
            Some(p) if dirty & SHADERS == 0 => (p.vs.clone(), p.ps.clone()),
            _ => {
                let Some(vs) = self.shader11(S::Vertex) else { return skip("draw without a vertex shader") };
                (vs, self.shader11(S::Pixel))
            }
        };
        let clip = match vs.clip_distances.min(8) {
            0 => Clip::None,
            n if self.opts.clip_distances => Clip::Builtin(n),
            n => Clip::Varying(n),
        };
        let (vsv, psv) = match &prev {
            Some(p) if dirty & (SHADERS | RES_VS | RES_PS) == 0 => (p.vsv.clone(), p.psv.clone()),
            _ => {
                let psv = match &ps {
                    Some(p) => Some(self.variant11(p, S::Pixel, None, clip)?),
                    None => None,
                };
                (self.variant11(&vs, S::Vertex, ps.as_ref(), clip)?, psv)
            }
        };
        let g0 = match &prev {
            Some(p) if dirty & (SHADERS | RES_VS | TARGETS) == 0 && p.vsv.id == vsv.id => p.g0.clone(),
            _ => self.bind_stage11(S::Vertex, &vs, &vsv, 0, Some(&targets))?,
        };
        let same_ps = |p: &Prepared| p.psv.as_ref().map(|v| v.id) == psv.as_ref().map(|v| v.id);
        let g1 = match (&prev, &ps, &psv) {
            (Some(p), _, _) if dirty & (SHADERS | RES_PS | TARGETS) == 0 && same_ps(p) => p.g1.clone(),
            (_, Some(p), Some(v)) => self.bind_stage11(S::Pixel, p, v, 1, Some(&targets))?,
            _ => {
                let (layout, group) = self.empty11();
                Bound { layout, group, cbs: Vec::new() }
            }
        };
        let (slots, vertex_id) = match &prev {
            Some(p) if dirty & (SHADERS | VERTEX) == 0 && p.vsv.id == vsv.id => (p.slots.clone(), p.vertex_id),
            _ => self.vertex_layout11(&vsv)?,
        };

        // Pipeline, unless nothing it depends on changed.
        let st = &self.d11.st;
        let (blend_id, blend) = match self.objects.get(&st.blend.0) {
            Some(Object::Blend11(id, b)) => (*id, **b),
            _ => (0, BlendDesc11::default()),
        };
        let (ds_id, dss) = match self.objects.get(&st.depth_stencil.0) {
            Some(Object::DepthStencil11(id, d)) => (*id, *d),
            _ => (0, DepthStencilDesc11::default()),
        };
        let (rs_id, rs) = match self.objects.get(&st.rasterizer.0) {
            Some(Object::Rasterizer11(id, r)) => (*id, *r),
            _ => (0, RasterizerDesc11::default()),
        };
        let fast = FastKey {
            vs: vsv.id,
            ps: psv.as_ref().map(|v| v.id).unwrap_or(0),
            layouts: (g0.layout.id, g1.layout.id),
            vertex: vertex_id,
            blend: blend_id,
            depth_stencil: ds_id,
            rasterizer: rs_id,
            topology: st.topology,
            targets,
            sample_mask: st.sample_mask,
        };
        let cached = match &prev {
            Some(p) if p.fast == fast => Some((p.key.clone(), p.pipeline.clone())),
            _ => self.d11.caches.fast_pipelines.get(&fast).cloned(),
        };
        let (key, pipeline) = match cached {
            Some(c) => c,
            None => {
                let Some(topo) = topology(st.topology) else {
                    return skip(format!("primitive topology {} is not supported", st.topology));
                };
                if rs.fill == 2 {
                    self.warn_once("wireframe fill is not supported yet; drawing solid");
                }
                let key =
                    self.pipeline_key11(&targets, topo, &blend, &dss, &rs, &vsv, psv.as_deref(), &g0, &g1, &slots)?;
                let pipeline = self.render_pipeline11(&key, &vsv, psv.as_deref(), &g0.layout, &g1.layout);
                // Handles are recycled after Destroy, which clears this.
                if self.d11.caches.fast_pipelines.len() > 4096 {
                    self.d11.caches.fast_pipelines.clear();
                }
                self.d11.caches.fast_pipelines.insert(fast, (key.clone(), pipeline.clone()));
                (key, pipeline)
            }
        };
        let p = Arc::new(Prepared {
            targets,
            vs,
            vsv,
            ps,
            psv,
            g0,
            g1,
            slots,
            vertex_id,
            fast,
            key,
            pipeline,
            scissor: rs.scissor,
        });
        self.d11.prepared = Some(p.clone());
        self.d11.prepared_epoch = self.epoch;
        self.d11.dirty = 0;
        Ok(p)
    }

    /// Vertex buffer layouts feeding the vertex shader's inputs from the
    /// input layout (zeros for inputs it lacks), interned to an id.
    fn vertex_layout11(&mut self, vsv: &Variant11) -> Result<(Vec<(u32, VertexBufferKey)>, u64), Fail> {
        let layout = match self.objects.get(&self.d11.st.input_layout.0) {
            Some(Object::InputLayout11(l)) => Some(l.clone()),
            _ => None,
        };
        let mut slots: Vec<(u32, VertexBufferKey)> = Vec::new();
        let mut zero_attrs = Vec::new();
        let mut step_rate_warning = false;
        for input in &vsv.t.vertex_inputs {
            let el = layout.as_ref().and_then(|l| {
                l.elements
                    .iter()
                    .find(|e| e.semantic.eq_ignore_ascii_case(&input.name) && e.semantic_index == input.index)
            });
            let Some(el) = el else {
                let f = match input.ty {
                    ComponentType::Uint => wgpu::VertexFormat::Uint32x4,
                    ComponentType::Sint => wgpu::VertexFormat::Sint32x4,
                    _ => wgpu::VertexFormat::Float32x4,
                };
                zero_attrs.push((f, 0, input.location));
                continue;
            };
            let Some(vf) = format::vertex_format(el.format) else {
                return skip(format!("vertex format {:?} is not supported", el.format));
            };
            let slot = el.slot.min(super::MAX_VBS as u32 - 1);
            let stride = self.d11.st.vbs[slot as usize].stride;
            if !stride.is_multiple_of(4) || !el.offset.is_multiple_of(4) {
                return skip(format!("unaligned vertex layout (stride {stride}, offset {})", el.offset));
            }
            if el.per_instance && el.step_rate > 1 {
                step_rate_warning = true;
            }
            let attr = (vf, el.offset, input.location);
            match slots.iter_mut().find(|(s, _)| *s == slot) {
                Some((_, k)) => k.attrs.push(attr),
                None => slots.push((slot, VertexBufferKey { stride, instance: el.per_instance, attrs: vec![attr] })),
            }
        }
        if step_rate_warning {
            self.warn_once("instance data step rates above 1 are not supported yet");
        }
        if !zero_attrs.is_empty() {
            slots.push((u32::MAX, VertexBufferKey { stride: 0, instance: false, attrs: zero_attrs }));
        }
        if slots.len() > 8 {
            return skip("more than 8 vertex buffers");
        }
        let id = match self.d11.caches.vertex_layouts.get(&slots) {
            Some(id) => *id,
            None => {
                let id = self.id();
                self.d11.caches.vertex_layouts.insert(slots.clone(), id);
                id
            }
        };
        Ok((slots, id))
    }

    #[allow(clippy::too_many_arguments)]
    fn pipeline_key11(
        &mut self,
        targets: &Targets11,
        topo: wgpu::PrimitiveTopology,
        blend: &BlendDesc11,
        dss: &DepthStencilDesc11,
        rs: &RasterizerDesc11,
        vsv: &Variant11,
        psv: Option<&Variant11>,
        g0: &Bound,
        g1: &Bound,
        slots: &[(u32, VertexBufferKey)],
    ) -> Result<PipelineKey, Fail> {
        let ps_targets: &[(u32, ComponentType)] = psv.map(|v| v.t.targets.as_slice()).unwrap_or(&[]);
        let mut colors: Vec<Option<ColorKey>> = Vec::new();
        for i in 0..MAX_RTS {
            let Some(Object::View11(v)) = self.objects.get(&targets.colors[i].0) else {
                colors.push(None);
                continue;
            };
            let ViewRes::Texture { format: f, .. } = v.res else { continue };
            let rt = if blend.independent { blend.targets[i] } else { blend.targets[0] };
            let written = ps_targets.iter().find(|(r, _)| *r == i as u32);
            if let Some((_, ty)) = written {
                let ok = match f.sample_type(None, None) {
                    Some(wgpu::TextureSampleType::Uint) => *ty == ComponentType::Uint,
                    Some(wgpu::TextureSampleType::Sint) => *ty == ComponentType::Sint,
                    _ => *ty == ComponentType::Float || *ty == ComponentType::Unknown,
                };
                if !ok {
                    return skip(format!("pixel shader output {i} type doesn't match render target format {f:?}"));
                }
            }
            let mask = if written.is_some() { rt.write_mask & 0xf } else { 0 };
            let a1 = format::alpha_is_one(v.desc.format);
            let blend = (rt.enable && convert::is_blendable(f)).then(|| {
                let color = convert::blend_component(rt.src, rt.dst, rt.op, a1);
                let alpha = convert::blend_component(rt.src_alpha, rt.dst_alpha, rt.op_alpha, a1);
                (color, alpha)
            });
            colors.push(Some(ColorKey { format: f, blend, mask }));
        }
        while colors.last().is_some_and(Option::is_none) {
            colors.pop();
        }
        let tri = matches!(topo, wgpu::PrimitiveTopology::TriangleList | wgpu::PrimitiveTopology::TriangleStrip);
        let depth = match self.objects.get(&targets.depth.0) {
            Some(Object::View11(v)) => match v.res {
                ViewRes::Texture { format: f, .. } => {
                    let (write, compare) = if dss.depth_enable {
                        (dss.depth_write && !targets.depth_read_only, convert::compare(dss.depth_func))
                    } else {
                        (false, wgpu::CompareFunction::Always)
                    };
                    let stencil = (dss.stencil_enable && f.has_stencil_aspect()).then(|| {
                        let face = |s: StencilFace| wgpu::StencilFaceState {
                            compare: convert::compare(s.func),
                            fail_op: convert::stencil_op(s.fail),
                            depth_fail_op: convert::stencil_op(s.depth_fail),
                            pass_op: convert::stencil_op(s.pass),
                        };
                        let wm = if targets.stencil_read_only { 0 } else { dss.write_mask & 0xff };
                        (face(dss.front), face(dss.back), dss.read_mask & 0xff, wm)
                    });
                    let (bias, slope, clamp) = if tri {
                        (rs.depth_bias, rs.slope_scaled_depth_bias, rs.depth_bias_clamp)
                    } else {
                        (0, 0.0, 0.0)
                    };
                    Some(DepthKey {
                        format: f,
                        write,
                        compare,
                        stencil,
                        bias,
                        slope: slope.to_bits(),
                        clamp: clamp.to_bits(),
                    })
                }
                _ => None,
            },
            _ => None,
        };
        let cull = match rs.cull {
            cull::FRONT => Some(wgpu::Face::Front),
            cull::BACK => Some(wgpu::Face::Back),
            _ => None,
        };
        let unclipped_depth = !rs.depth_clip && self.features.contains(wgpu::Features::DEPTH_CLIP_CONTROL);
        let msaa = targets.samples > 1;
        Ok(PipelineKey {
            vs: vsv.id,
            ps: psv.map(|v| v.id).unwrap_or(0),
            layout: (g0.layout.id, g1.layout.id),
            buffers: slots.iter().map(|(_, k)| k.clone()).collect(),
            topology: topo,
            strip_index: None,
            cull,
            front_ccw: rs.front_ccw,
            unclipped_depth,
            colors,
            depth,
            samples: targets.samples,
            sample_mask: if msaa { self.d11.st.sample_mask } else { !0 },
            alpha_to_coverage: msaa && blend.alpha_to_coverage,
        })
    }

    fn try_draw11(&mut self, d: &Draw11) -> Result<(), Fail> {
        let (count, instances) = match d {
            Draw11::Vertices { count, instances, .. } | Draw11::Indexed { count, instances, .. } => {
                (*count, *instances)
            }
        };
        if count == 0 || instances == 0 {
            return Err(Fail::Nothing);
        }
        if self.d11.prepared_epoch != self.epoch {
            // Bind groups mark the resources they use for this submission.
            self.d11.dirty |= super::dirty::RES_VS | super::dirty::RES_PS;
        }
        let p = match &self.d11.prepared {
            Some(p) if self.d11.dirty == 0 => p.clone(),
            _ => {
                let t = crate::now_ns();
                let p = self.prepare11();
                self.stats.prepare_ns += crate::now_ns() - t;
                p?
            }
        };
        let targets = p.targets;

        // Viewport and scissor.
        let st = &self.d11.st;
        let Some(vp) = st.viewport else { return Err(Fail::Nothing) };
        let Some(fit) = d3dgpu_emu::viewport::fit_viewport_f(
            vp.x as f64,
            vp.y as f64,
            vp.width as f64,
            vp.height as f64,
            targets.width,
            targets.height,
        ) else {
            return Err(Fail::Nothing);
        };
        let full = d3dgpu_proto::Rect::new(0, 0, targets.width as i32, targets.height as i32);
        let scissor = if p.scissor { crate::intersect(full, st.scissor) } else { full };
        if scissor.width() == 0 || scissor.height() == 0 {
            return Err(Fail::Nothing);
        }

        // Uniforms. SV_VertexID excludes StartVertexLocation and
        // BaseVertexLocation and SV_InstanceID excludes StartInstanceLocation
        // (Wine's test_vertex_id); WebGPU's builtins include them.
        let (start_instance, base_vertex) = match d {
            Draw11::Vertices { start_instance, start, .. } => (*start_instance, *start as i32),
            Draw11::Indexed { start_instance, base_vertex, .. } => (*start_instance, *base_vertex),
        };
        let mut drv = [0u8; 32];
        for (i, f) in [fit.scale[0], fit.scale[1], fit.offset[0], fit.offset[1]].iter().enumerate() {
            drv[i * 4..i * 4 + 4].copy_from_slice(&f.to_le_bytes());
        }
        drv[16..20].copy_from_slice(&start_instance.to_le_bytes());
        drv[20..24].copy_from_slice(&(base_vertex as u32).to_le_bytes());
        let drv_off = self.driver11(drv)?;
        let off0 = self.cb_offsets(S::Vertex, &p.g0)?;
        let off1 = self.cb_offsets(S::Pixel, &p.g1)?;

        // Vertex and index buffers.
        let epoch = self.epoch;
        let mut vbufs: Vec<(wgpu::Buffer, u64, u64)> = Vec::with_capacity(p.slots.len());
        for (slot, _) in &p.slots {
            if *slot == u32::MAX {
                vbufs.push((self.d11.caches.zero_buffer.clone(), ZERO_BUFFER_ID, 0));
                continue;
            }
            let b = self.d11.st.vbs[*slot as usize];
            match self.objects.get_mut(&b.buffer.0) {
                Some(Object::Buffer11(buf)) if buf.gpu.is_some() && (b.offset as u64) < buf.shadow.len() as u64 => {
                    buf.last_use = epoch;
                    vbufs.push((buf.gpu.clone().unwrap(), buf.gen, b.offset as u64));
                }
                _ => return skip(format!("vertex buffer slot {slot} is empty")),
            }
        }
        let index = match d {
            Draw11::Indexed { .. } => {
                let (h, f, off) = self.d11.st.ib;
                let f = match f {
                    DxgiFormat::R16Uint => wgpu::IndexFormat::Uint16,
                    DxgiFormat::R32Uint => wgpu::IndexFormat::Uint32,
                    _ => return skip(format!("index format {f:?}")),
                };
                match self.objects.get_mut(&h.0) {
                    Some(Object::Buffer11(buf)) if buf.gpu.is_some() => {
                        buf.last_use = epoch;
                        Some((buf.gpu.clone().unwrap(), buf.gen, off as u64, f))
                    }
                    _ => return skip("indexed draw without an index buffer"),
                }
            }
            Draw11::Vertices { .. } => None,
        };

        // Indexed strips restart at the index format's maximum.
        let strip =
            matches!(p.key.topology, wgpu::PrimitiveTopology::TriangleStrip | wgpu::PrimitiveTopology::LineStrip);
        let (pid, pipeline) = match index.as_ref().map(|i| i.3) {
            Some(f) if strip => {
                let key = PipelineKey { strip_index: Some(f), ..p.key.clone() };
                self.render_pipeline11(&key, &p.vsv, p.psv.as_deref(), &p.g0.layout, &p.g1.layout)
            }
            _ => p.pipeline.clone(),
        };

        // Record.
        let t_record = crate::now_ns();
        self.ensure_pass11(targets);
        let blend_factor = self.d11.st.blend_factor.map(|c| c as f64);
        let stencil_ref = self.d11.st.stencil_ref & 0xff;
        let driver = self.d11.caches.driver_group.clone();
        let (pass, pc) = (self.pass.as_mut().unwrap(), &mut self.pc);
        pc.set_pipeline(pass, pid, &pipeline);
        pc.set_bind_group(pass, 0, p.g0.group.0, &p.g0.group.1, &off0);
        pc.set_bind_group(pass, 1, p.g1.group.0, &p.g1.group.1, &off1);
        pc.set_bind_group(pass, 2, DRIVER_GROUP_ID, &driver, &[drv_off as u32]);
        for (i, (b, gen, off)) in vbufs.iter().enumerate() {
            pc.set_vertex_buffer(pass, i as u32, *gen, b, *off);
        }
        let min = vp.min_depth.clamp(0.0, 1.0);
        pc.set_viewport(pass, [fit.x, fit.y, fit.width, fit.height, min, vp.max_depth.clamp(min, 1.0)]);
        pc.set_scissor(pass, [scissor.x1 as u32, scissor.y1 as u32, scissor.width(), scissor.height()]);
        let [r, g, b, a] = blend_factor;
        pc.set_blend_constant(pass, wgpu::Color { r, g, b, a });
        pc.set_stencil_reference(pass, stencil_ref);
        match (d, index) {
            (Draw11::Indexed { count, start, base_vertex, instances, start_instance }, Some((buf, gen, off, f))) => {
                pc.set_index_buffer(pass, gen, &buf, off, f);
                pass.draw_indexed(*start..start + count, *base_vertex, *start_instance..start_instance + instances);
            }
            (Draw11::Vertices { count, start, instances, start_instance }, _) => {
                pass.draw(*start..start + count, *start_instance..start_instance + instances);
            }
            _ => {}
        }
        self.stats.record_ns += crate::now_ns() - t_record;
        Ok(())
    }

    fn render_pipeline11(
        &mut self,
        key: &PipelineKey,
        vs: &Variant11,
        ps: Option<&Variant11>,
        g0: &Layout11,
        g1: &Layout11,
    ) -> (u64, wgpu::RenderPipeline) {
        if let Some(p) = self.d11.caches.pipelines.get(key) {
            return p.clone();
        }
        let layout = self.pipeline_layout11(g0, g1, false);
        let attrs: Vec<Vec<wgpu::VertexAttribute>> = key
            .buffers
            .iter()
            .map(|b| {
                b.attrs
                    .iter()
                    .map(|(format, offset, loc)| wgpu::VertexAttribute {
                        format: *format,
                        offset: *offset as u64,
                        shader_location: *loc,
                    })
                    .collect()
            })
            .collect();
        let buffers: Vec<Option<wgpu::VertexBufferLayout>> = key
            .buffers
            .iter()
            .zip(&attrs)
            .map(|(b, a)| {
                Some(wgpu::VertexBufferLayout {
                    array_stride: b.stride as u64,
                    step_mode: if b.instance { wgpu::VertexStepMode::Instance } else { wgpu::VertexStepMode::Vertex },
                    attributes: a,
                })
            })
            .collect();
        let targets: Vec<Option<wgpu::ColorTargetState>> = key
            .colors
            .iter()
            .map(|c| {
                c.as_ref().map(|c| wgpu::ColorTargetState {
                    format: c.format,
                    blend: c.blend.map(|(color, alpha)| wgpu::BlendState { color, alpha }),
                    write_mask: wgpu::ColorWrites::from_bits_truncate(c.mask),
                })
            })
            .collect();
        let depth_stencil = key.depth.as_ref().map(|d| wgpu::DepthStencilState {
            format: d.format,
            depth_write_enabled: Some(d.write),
            depth_compare: Some(d.compare),
            stencil: match d.stencil {
                Some((front, back, read_mask, write_mask)) => wgpu::StencilState { front, back, read_mask, write_mask },
                None => wgpu::StencilState::default(),
            },
            bias: wgpu::DepthBiasState {
                constant: d.bias,
                slope_scale: f32::from_bits(d.slope),
                clamp: f32::from_bits(d.clamp),
            },
        });
        let p = self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("d3dgpu pipeline11"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &vs.module,
                entry_point: Some(vs.t.entry),
                compilation_options: Default::default(),
                buffers: &buffers,
            },
            primitive: wgpu::PrimitiveState {
                topology: key.topology,
                strip_index_format: key.strip_index,
                front_face: if key.front_ccw { wgpu::FrontFace::Ccw } else { wgpu::FrontFace::Cw },
                cull_mode: key.cull,
                unclipped_depth: key.unclipped_depth,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil,
            multisample: wgpu::MultisampleState {
                count: key.samples,
                mask: key.sample_mask as u64,
                alpha_to_coverage_enabled: key.alpha_to_coverage,
            },
            fragment: ps.map(|ps| wgpu::FragmentState {
                module: &ps.module,
                entry_point: Some(ps.t.entry),
                compilation_options: Default::default(),
                targets: &targets,
            }),
            multiview_mask: None,
            cache: None,
        });
        self.stats.pipelines_created += 1;
        let p = (self.id(), p);
        self.d11.caches.pipelines.insert(key.clone(), p.clone());
        p
    }

    // ---- Dispatch ----

    fn try_dispatch11(&mut self, x: u32, y: u32, z: u32) -> Result<(), Fail> {
        let Some(cs) = self.shader11(S::Compute) else { return skip("dispatch without a compute shader") };
        let csv = self.variant11(&cs, S::Compute, None, Clip::None)?;
        let mut drv = [0u8; 32];
        drv[0..4].copy_from_slice(&1f32.to_le_bytes());
        drv[4..8].copy_from_slice(&1f32.to_le_bytes());
        let drv_off = self.driver11(drv)?;
        let g0 = self.bind_stage11(S::Compute, &cs, &csv, 0, None)?;
        let off0 = self.cb_offsets(S::Compute, &g0)?;
        let (empty, empty_group) = self.empty11();
        let key = (csv.id, g0.layout.id);
        let pipeline = match self.d11.caches.compute.get(&key) {
            Some(p) => p.1.clone(),
            None => {
                let layout = self.pipeline_layout11(&g0.layout, &empty, true);
                let p = self.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("d3dgpu compute11"),
                    layout: Some(&layout),
                    module: &csv.module,
                    entry_point: Some(csv.t.entry),
                    compilation_options: Default::default(),
                    cache: None,
                });
                self.stats.pipelines_created += 1;
                let id = self.id();
                self.d11.caches.compute.insert(key, (id, p.clone()));
                p
            }
        };
        let max = 65535;
        if x > max || y > max || z > max {
            self.warn_once("dispatch larger than 65535 groups in a dimension; clamped");
        }
        self.end_pass();
        let driver = self.d11.caches.driver_group.clone();
        let mut cp = self.encoder().begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("d3dgpu dispatch11"),
            timestamp_writes: None,
        });
        cp.set_pipeline(&pipeline);
        cp.set_bind_group(0, &g0.group.1, &off0);
        cp.set_bind_group(1, &empty_group.1, &[]);
        cp.set_bind_group(2, &driver, &[drv_off as u32]);
        cp.dispatch_workgroups(x.min(max), y.min(max), z.min(max));
        Ok(())
    }
}
