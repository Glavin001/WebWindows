//! The Direct3D 10/11 half of the core.
//!
//! Direct3D 11 maps onto WebGPU much more directly than Direct3D 9: state
//! comes in immutable objects (blend, depth-stencil, rasterizer, input
//! layout, sampler), resources are bound through views, and shaders are
//! DXBC that [`d3dgpu_dxbc`] translates. What is left to emulate:
//!
//! - **Constant buffers** are CPU shadows. A draw copies the window each
//!   shader reads into the uniform ring (once per buffer version per
//!   submission) and binds it with a dynamic offset, so updating a
//!   constant buffer between draws never breaks the render pass.
//! - **Updates to buffers in use** by recorded commands rename the buffer
//!   (a pooled buffer receives the whole new contents through the queue),
//!   which keeps the pass open; large partial updates fall back to a copy
//!   in command order.
//! - **Clears** are deferred and become the load operation of the next
//!   pass that renders to the view.
//! - **Typed buffer views** are storage buffers the translated shader
//!   decodes; texture views follow WebGPU's rules (a texture can only be
//!   reinterpreted as its sRGB twin).

mod draw;
pub mod format;

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use d3dgpu_proto::d3d11::*;
use d3dgpu_proto::{Command, Handle, Rect};

pub use draw::{Caches11, Prepared};

/// Parts of the derived draw state ([`Prepared`]) a command invalidates.
pub mod dirty {
    pub const TARGETS: u32 = 1;
    pub const SHADERS: u32 = 2;
    pub const RES_VS: u32 = 4;
    pub const RES_PS: u32 = 8;
    pub const VERTEX: u32 = 16;
    pub const PIPELINE: u32 = 32;
    pub const ALL: u32 = 63;
}

use crate::resources::{FastMap, Object};
use crate::{Core, Pending, PendingKind, RawRead};

pub const MAX_RTS: usize = 8;
pub const MAX_VBS: usize = 32;
pub const MAX_CBS: usize = 14;
pub const MAX_SRVS: usize = 128;
pub const MAX_SAMPLERS: usize = 16;
pub const MAX_UAVS: usize = 8;
const STAGES: usize = 6;

/// A Direct3D 11 buffer.
pub struct Buffer11 {
    /// `None` for constant buffers, which live in `shadow` only.
    pub gpu: Option<wgpu::Buffer>,
    /// Identity of `gpu` (changes when the buffer is renamed).
    pub gen: u64,
    pub size: u32,
    pub bind: u32,
    pub misc: u32,
    pub stride: u32,
    /// CPU copy of the contents (valid unless the GPU wrote the buffer).
    pub shadow: Vec<u8>,
    /// Bumped on every CPU update.
    pub version: u64,
    pub gpu_written: bool,
    pub last_use: u64,
    /// Constant buffers: the last copy into the uniform ring
    /// (version, byte offset, window size, epoch, ring offset).
    pub ring: Option<(u64, u32, u32, u64, u64)>,
}

/// A Direct3D 11 texture (1D textures are 2D textures one texel high).
pub struct Texture11 {
    pub gpu: wgpu::Texture,
    pub desc: Texture11Desc,
    pub format: wgpu::TextureFormat,
    pub levels: u32,
    /// Array layers (6 per cube); 1 for 3D textures.
    pub layers: u32,
    pub last_use: u64,
}

impl Texture11 {
    pub fn mip_size(&self, mip: u32) -> (u32, u32, u32) {
        let d = &self.desc;
        let depth = if d.dim == TextureDim::D3 { (d.depth_or_array >> mip).max(1) } else { 1 };
        ((d.width >> mip).max(1), (d.height >> mip).max(1), depth)
    }

    /// (mip, array layer) of a subresource index.
    pub fn subresource(&self, sub: u32) -> (u32, u32) {
        (sub % self.levels, sub / self.levels)
    }

    pub fn is_depth(&self) -> bool {
        self.format.is_depth_stencil_format()
    }
}

/// What a view resolves to.
pub enum ViewRes {
    Texture {
        view: wgpu::TextureView,
        format: wgpu::TextureFormat,
        dim: wgpu::TextureViewDimension,
        aspect: wgpu::TextureAspect,
        mip: u32,
        /// Size of the view's first mip.
        size: (u32, u32),
        samples: u32,
    },
    Buffer {
        offset: u64,
        size: u64,
        /// Typed views: the element format.
        format: Option<d3dgpu_dxbc::BufferFormat>,
    },
}

pub struct View11 {
    pub id: u64,
    pub kind: ViewKind,
    pub resource: Handle,
    pub desc: ViewDesc,
    pub res: ViewRes,
}

pub struct Shader11 {
    pub id: u64,
    pub hash: u64,
    pub shader: d3dgpu_dxbc::Shader,
    /// Pixel shaders: the varyings they read.
    pub linkage: Vec<d3dgpu_dxbc::LinkSlot>,
    /// Vertex shaders: clip and cull distances written.
    pub clip_distances: u8,
}

/// An input layout with `APPEND_ALIGNED_ELEMENT` offsets resolved.
pub struct InputLayout11 {
    pub elements: Vec<InputElement>,
}

/// A deferred clear of one view.
#[derive(Clone, Copy, Debug, Default)]
pub struct Clear11 {
    pub color: Option<wgpu::Color>,
    pub depth: Option<f32>,
    pub stencil: Option<u32>,
}

/// The attachments of a Direct3D 11 render pass (view handles).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Targets11 {
    pub colors: [Handle; MAX_RTS],
    pub depth: Handle,
    pub depth_read_only: bool,
    pub stencil_read_only: bool,
    pub width: u32,
    pub height: u32,
    pub samples: u32,
}

/// The device context state.
pub struct State11 {
    pub input_layout: Handle,
    pub vbs: [VertexBufferBinding; MAX_VBS],
    pub ib: (Handle, DxgiFormat, u32),
    pub topology: u32,
    pub shaders: [Handle; STAGES],
    pub cbs: [[ConstantBufferBinding; MAX_CBS]; STAGES],
    pub srvs: [[Handle; MAX_SRVS]; STAGES],
    pub samplers: [[Handle; MAX_SAMPLERS]; STAGES],
    pub uavs: [[Handle; MAX_UAVS]; STAGES],
    pub rtvs: [Handle; MAX_RTS],
    pub dsv: Handle,
    pub blend: Handle,
    pub blend_factor: [f32; 4],
    pub sample_mask: u32,
    pub depth_stencil: Handle,
    pub stencil_ref: u32,
    pub rasterizer: Handle,
    pub viewport: Option<Viewport11>,
    pub scissor: Rect,
}

impl State11 {
    fn new() -> State11 {
        State11 {
            input_layout: Handle::NONE,
            vbs: [VertexBufferBinding::default(); MAX_VBS],
            ib: (Handle::NONE, DxgiFormat::R16Uint, 0),
            topology: 0,
            shaders: [Handle::NONE; STAGES],
            cbs: [[ConstantBufferBinding::default(); MAX_CBS]; STAGES],
            srvs: [[Handle::NONE; MAX_SRVS]; STAGES],
            samplers: [[Handle::NONE; MAX_SAMPLERS]; STAGES],
            uavs: [[Handle::NONE; MAX_UAVS]; STAGES],
            rtvs: [Handle::NONE; MAX_RTS],
            dsv: Handle::NONE,
            blend: Handle::NONE,
            blend_factor: [1.0; 4],
            sample_mask: !0,
            depth_stencil: Handle::NONE,
            stencil_ref: 0,
            rasterizer: Handle::NONE,
            viewport: None,
            scissor: Rect::default(),
        }
    }
}

/// A buffer waiting to be reused by a rename.
struct Pooled {
    gpu: wgpu::Buffer,
    gen: u64,
    last_use: u64,
}

/// Everything Direct3D 11 adds to the core.
pub struct Device11 {
    pub st: State11,
    /// Attachments of the open pass when it is a Direct3D 11 pass.
    pub pass: Option<Targets11>,
    pub clears: FastMap<u32, Clear11>,
    pub caches: Caches11,
    pub prepared: Option<Arc<Prepared>>,
    pub prepared_epoch: u64,
    pub dirty: u32,
    pool: FastMap<(u64, u32), Vec<Pooled>>,
    /// Warnings already given (each is logged once).
    warned: std::collections::HashSet<String>,
}

impl Device11 {
    pub fn new(device: &wgpu::Device, uniform_ring: &wgpu::Buffer) -> Device11 {
        Device11 {
            st: State11::new(),
            pass: None,
            clears: FastMap::default(),
            caches: Caches11::new(device, uniform_ring),
            prepared: None,
            prepared_epoch: 0,
            dirty: dirty::ALL,
            pool: FastMap::default(),
            warned: Default::default(),
        }
    }
}

fn stage_index(s: Stage11) -> usize {
    s as usize
}

fn res_dirty(s: Stage11) -> u32 {
    match s {
        Stage11::Vertex => dirty::RES_VS,
        Stage11::Pixel => dirty::RES_PS,
        _ => 0,
    }
}

impl Core {
    pub(crate) fn warn_once(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        if self.d11.warned.insert(msg.clone()) {
            self.warn(msg);
        }
    }

    pub(crate) fn command11(&mut self, cmd: Command, shared: &[u8]) {
        match cmd {
            Command::CreateBuffer11 { id, size, bind: b, misc: m, stride } => {
                self.create_buffer11(id, size, b, m, stride)
            }
            Command::CreateTexture11 { id, desc } => self.create_texture11(id, desc),
            Command::UpdateSubresource { resource, subresource, bx, row_pitch, depth_pitch, data } => {
                if let Some(bytes) = self.resolve(data, shared) {
                    self.update_subresource(resource, subresource, bx, row_pitch, depth_pitch, bytes);
                }
            }
            Command::CreateView { id, kind, resource, desc } => self.create_view(id, kind, resource, desc),
            Command::CreateSampler { id, desc } => {
                self.objects.insert(id.0, Object::Sampler11(desc));
            }
            Command::CreateBlendState { id, desc } => {
                let oid = self.id();
                self.objects.insert(id.0, Object::Blend11(oid, desc));
            }
            Command::CreateDepthStencilState { id, desc } => {
                let oid = self.id();
                self.objects.insert(id.0, Object::DepthStencil11(oid, desc));
            }
            Command::CreateRasterizerState { id, desc } => {
                let oid = self.id();
                self.objects.insert(id.0, Object::Rasterizer11(oid, desc));
            }
            Command::CreateInputLayout { id, mut elements } => {
                let mut next = [0u32; MAX_VBS];
                for e in &mut elements {
                    let slot = (e.slot as usize).min(MAX_VBS - 1);
                    if e.offset == APPEND_ALIGNED_ELEMENT {
                        e.offset = next[slot];
                    }
                    next[slot] = e.offset + format::element_bytes(e.format).unwrap_or(0);
                }
                let layout = InputLayout11 { elements };
                self.objects.insert(id.0, Object::InputLayout11(Arc::new(layout)));
            }
            Command::CreateShader11 { id, stage, hash, dxbc } => {
                let Some(bytes) = self.resolve(dxbc, shared) else { return };
                self.create_shader11(id, stage, hash, bytes);
            }
            Command::SetInputLayout(h) => {
                self.d11.st.input_layout = h;
                self.d11.dirty |= dirty::VERTEX;
            }
            Command::SetVertexBuffers { start, buffers } => {
                self.d11.dirty |= dirty::VERTEX;
                for (i, b) in buffers.into_iter().enumerate() {
                    if let Some(s) = self.d11.st.vbs.get_mut(start as usize + i) {
                        *s = b;
                    }
                }
            }
            Command::SetIndexBuffer { buffer, format, offset } => self.d11.st.ib = (buffer, format, offset),
            Command::SetPrimitiveTopology(t) => {
                self.d11.st.topology = t;
                self.d11.dirty |= dirty::PIPELINE;
            }
            Command::SetShader11 { stage, id } => {
                self.d11.st.shaders[stage_index(stage)] = id;
                self.d11.dirty |= dirty::SHADERS;
            }
            Command::SetConstantBuffers { stage, start, buffers } => {
                let s = &mut self.d11.st.cbs[stage_index(stage)];
                for (i, b) in buffers.into_iter().enumerate() {
                    if let Some(slot) = s.get_mut(start as usize + i) {
                        *slot = b;
                    }
                }
            }
            Command::SetShaderResources { stage, start, views } => {
                self.d11.dirty |= res_dirty(stage);
                set_range(&mut self.d11.st.srvs[stage_index(stage)], start, &views)
            }
            Command::SetSamplers { stage, start, samplers } => {
                self.d11.dirty |= res_dirty(stage);
                set_range(&mut self.d11.st.samplers[stage_index(stage)], start, &samplers)
            }
            Command::SetUnorderedAccessViews { stage, start, views } => {
                self.d11.dirty |= res_dirty(stage);
                set_range(&mut self.d11.st.uavs[stage_index(stage)], start, &views)
            }
            Command::SetRenderTargets11 { rtvs, dsv } => {
                let st = &mut self.d11.st;
                st.rtvs = [Handle::NONE; MAX_RTS];
                for (i, v) in rtvs.into_iter().take(MAX_RTS).enumerate() {
                    st.rtvs[i] = v;
                }
                st.dsv = dsv;
                self.d11.dirty |= dirty::TARGETS;
            }
            Command::SetBlendState { id, factor, sample_mask } => {
                let st = &mut self.d11.st;
                st.blend = id;
                st.blend_factor = factor;
                st.sample_mask = sample_mask;
                self.d11.dirty |= dirty::PIPELINE;
            }
            Command::SetDepthStencilState { id, stencil_ref } => {
                self.d11.st.depth_stencil = id;
                self.d11.st.stencil_ref = stencil_ref;
                self.d11.dirty |= dirty::PIPELINE;
            }
            Command::SetRasterizerState(h) => {
                self.d11.st.rasterizer = h;
                self.d11.dirty |= dirty::PIPELINE;
            }
            Command::SetViewports(vps) => self.d11.st.viewport = vps.first().copied(),
            Command::SetScissorRects(rects) => self.d11.st.scissor = rects.first().copied().unwrap_or_default(),
            Command::Draw11 { vertex_count, start_vertex, instance_count, start_instance } => {
                self.draw11(draw::Draw11::Vertices {
                    count: vertex_count,
                    start: start_vertex,
                    instances: instance_count,
                    start_instance,
                });
            }
            Command::DrawIndexed11 { index_count, start_index, base_vertex, instance_count, start_instance } => {
                self.draw11(draw::Draw11::Indexed {
                    count: index_count,
                    start: start_index,
                    base_vertex,
                    instances: instance_count,
                    start_instance,
                });
            }
            Command::Dispatch { x, y, z } => self.dispatch11(x, y, z),
            Command::ClearRenderTargetView { view, color } => {
                let c = wgpu::Color { r: color[0] as f64, g: color[1] as f64, b: color[2] as f64, a: color[3] as f64 };
                self.clear_view(view, Clear11 { color: Some(c), ..Default::default() });
            }
            Command::ClearDepthStencilView { view, flags, depth, stencil } => {
                let c = Clear11 {
                    color: None,
                    depth: (flags & clear11::DEPTH != 0).then_some(depth.clamp(0.0, 1.0)),
                    stencil: (flags & clear11::STENCIL != 0).then_some(stencil & 0xff),
                };
                self.clear_view(view, c);
            }
            Command::ClearUnorderedAccessViewUint { view, values } => self.clear_uav(view, values, false),
            Command::ClearUnorderedAccessViewFloat { view, values } => {
                self.clear_uav(view, values.map(f32::to_bits), true)
            }
            Command::CopyResource { dst, src } => self.copy_resource(dst, src),
            Command::CopySubresourceRegion { dst, dst_sub, x, y, z, src, src_sub, bx } => {
                self.copy_region(dst, dst_sub, [x, y, z], src, src_sub, bx)
            }
            Command::ReadSubresource { resource, subresource, bx, dest_offset, row_pitch, depth_pitch, fence } => {
                self.read_subresource(resource, subresource, bx, dest_offset, row_pitch, depth_pitch, fence)
            }
            other => self.warn(format!("unexpected command {other:?}")),
        }
    }

    // ---- Resources ----

    fn create_buffer11(&mut self, id: Handle, size: u32, b: u32, m: u32, stride: u32) {
        let padded = (size as u64).next_multiple_of(4).max(4);
        let gpu = if b == bind::CONSTANT_BUFFER { None } else { Some(self.new_buffer11(padded, b, m)) };
        let gen = self.id();
        self.objects.insert(
            id.0,
            Object::Buffer11(Box::new(Buffer11 {
                gpu,
                gen,
                size,
                bind: b,
                misc: m,
                stride,
                shadow: vec![0; padded as usize],
                version: 0,
                gpu_written: false,
                last_use: 0,
                ring: None,
            })),
        );
    }

    fn buffer_usage11(b: u32, m: u32) -> wgpu::BufferUsages {
        use wgpu::BufferUsages as U;
        let mut u = U::COPY_DST | U::COPY_SRC;
        if b & bind::VERTEX_BUFFER != 0 {
            u |= U::VERTEX;
        }
        if b & bind::INDEX_BUFFER != 0 {
            u |= U::INDEX;
        }
        if b & (bind::SHADER_RESOURCE | bind::UNORDERED_ACCESS) != 0 {
            u |= U::STORAGE;
        }
        if m & misc::DRAWINDIRECT_ARGS != 0 {
            u |= U::INDIRECT;
        }
        u
    }

    fn new_buffer11(&mut self, size: u64, b: u32, m: u32) -> wgpu::Buffer {
        self.stats.buffers_created += 1;
        self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("d3dgpu buffer11"),
            size,
            usage: Self::buffer_usage11(b, m),
            mapped_at_creation: false,
        })
    }

    fn create_texture11(&mut self, id: Handle, desc: Texture11Desc) {
        let depth = desc.bind & bind::DEPTH_STENCIL != 0;
        let features = self.features;
        let Some(format) = format::texture_format(desc.format, depth, features) else {
            return self.warn(format!("CreateTexture11 {id:?}: format {:?} is not supported", desc.format));
        };
        let (w, h) = (desc.width.max(1), if desc.dim == TextureDim::D1 { 1 } else { desc.height.max(1) });
        let (dimension, layers) = match desc.dim {
            TextureDim::D3 => (wgpu::TextureDimension::D3, 1),
            _ => (wgpu::TextureDimension::D2, desc.depth_or_array.max(1)),
        };
        let extent_z = if desc.dim == TextureDim::D3 { desc.depth_or_array.max(1) } else { layers };
        let max_levels = 32 - w.max(h).max(if desc.dim == TextureDim::D3 { extent_z } else { 1 }).leading_zeros();
        let levels = if desc.mips == 0 { max_levels } else { desc.mips.min(max_levels) };
        let samples = if desc.samples > 1 { 4 } else { 1 };
        use wgpu::TextureUsages as U;
        let mut usage = U::empty();
        if desc.bind & bind::SHADER_RESOURCE != 0 || desc.bind == 0 {
            usage |= U::TEXTURE_BINDING;
        }
        if desc.bind & (bind::RENDER_TARGET | bind::DEPTH_STENCIL) != 0 || samples > 1 {
            usage |= U::RENDER_ATTACHMENT;
        }
        if desc.bind & bind::UNORDERED_ACCESS != 0 && format::storage_format(format).is_some() {
            usage |= U::STORAGE_BINDING;
        }
        if samples == 1
            && !matches!(format, wgpu::TextureFormat::Depth24Plus | wgpu::TextureFormat::Depth24PlusStencil8)
        {
            usage |= U::COPY_SRC;
            if !format.is_depth_stencil_format() || format == wgpu::TextureFormat::Depth16Unorm {
                usage |= U::COPY_DST;
            }
        }
        if usage.is_empty() {
            usage = U::TEXTURE_BINDING;
        }
        let view_formats: Vec<wgpu::TextureFormat> = [format.add_srgb_suffix(), format.remove_srgb_suffix()]
            .into_iter()
            .filter(|f| *f != format)
            .take(1)
            .collect();
        let gpu = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("d3dgpu texture11"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: extent_z },
            mip_level_count: if samples > 1 { 1 } else { levels },
            sample_count: samples,
            dimension,
            format,
            usage,
            view_formats: &view_formats,
        });
        self.stats.textures_created += 1;
        let t = Texture11 {
            gpu,
            desc,
            format,
            levels: if samples > 1 { 1 } else { levels },
            layers: if desc.dim == TextureDim::D3 { 1 } else { layers },
            last_use: 0,
        };
        self.objects.insert(id.0, Object::Texture11(Box::new(t)));
    }

    fn create_shader11(&mut self, id: Handle, stage: Stage11, hash: u64, bytes: &[u8]) {
        let shader = match d3dgpu_dxbc::Shader::from_dxbc(bytes) {
            Ok(s) => s,
            Err(e) => return self.warn(format!("shader {id:?} ({hash:016x}): {e}")),
        };
        use d3dgpu_dxbc::ProgramType as P;
        let want = match stage {
            Stage11::Pixel => P::Pixel,
            Stage11::Vertex => P::Vertex,
            Stage11::Geometry => P::Geometry,
            Stage11::Hull => P::Hull,
            Stage11::Domain => P::Domain,
            Stage11::Compute => P::Compute,
        };
        if shader.ty() != want {
            return self.warn(format!("shader {id:?}: bytecode is a {:?} shader, not {stage:?}", shader.ty()));
        }
        let linkage = if stage == Stage11::Pixel { shader.linkage() } else { Vec::new() };
        let clip_distances = shader
            .reflection
            .outputs
            .iter()
            .filter(|e| e.system_value == d3dgpu_dxbc::container::sv::CLIP_DISTANCE)
            .map(|e| e.mask.count_ones() as u8)
            .sum();
        let s = Shader11 { id: self.id(), hash, shader, linkage, clip_distances };
        self.objects.insert(id.0, Object::Shader11(Arc::new(s)));
    }

    fn create_view(&mut self, id: Handle, kind: ViewKind, resource: Handle, desc: ViewDesc) {
        let vid = self.id();
        let mut warnings = Vec::new();
        let res = match self.objects.get(&resource.0) {
            Some(Object::Buffer11(b)) => {
                let typed = desc.flags & view_flags::RAW == 0 && b.misc & misc::BUFFER_STRUCTURED == 0;
                let el = if desc.flags & view_flags::RAW != 0 {
                    4
                } else if b.misc & misc::BUFFER_STRUCTURED != 0 {
                    b.stride.max(1)
                } else {
                    format::element_bytes(desc.format).unwrap_or(4)
                };
                let format = if typed {
                    match format::buffer_format(desc.format) {
                        Some(f) => Some(f),
                        None => {
                            return self
                                .warn(format!("view {id:?}: typed buffer format {:?} not supported", desc.format))
                        }
                    }
                } else {
                    None
                };
                let offset = desc.first_element as u64 * el as u64;
                let size = (desc.num_elements as u64 * el as u64).min((b.size as u64).saturating_sub(offset));
                if !offset.is_multiple_of(256) {
                    self.warn_once(format!(
                        "buffer views must start at a multiple of 256 bytes on WebGPU (view {id:?} starts at {offset})"
                    ));
                }
                ViewRes::Buffer { offset: offset & !255, size: size.next_multiple_of(4).max(4), format }
            }
            Some(Object::Texture11(t)) => {
                let features = self.features;
                let depth = t.is_depth();
                // Shader views of a depth-stencil texture see one aspect.
                let aspect = match (depth, kind) {
                    (false, _) | (true, ViewKind::DepthStencil) => wgpu::TextureAspect::All,
                    _ => match format::view_aspect(desc.format) {
                        wgpu::TextureAspect::All => wgpu::TextureAspect::DepthOnly,
                        a => a,
                    },
                };
                let format = if depth {
                    t.format.aspect_specific_format(aspect).unwrap_or(t.format)
                } else {
                    match format::texture_format(desc.format, false, features) {
                        Some(f)
                            if f == t.format
                                || f == t.format.add_srgb_suffix()
                                || f == t.format.remove_srgb_suffix() =>
                        {
                            f
                        }
                        Some(f) => {
                            warnings.push(format!(
                                "view of a {:?} texture as {f:?} is not possible on WebGPU; using {:?}",
                                t.format, t.format
                            ));
                            t.format
                        }
                        None => t.format,
                    }
                };
                let single = matches!(kind, ViewKind::RenderTarget | ViewKind::DepthStencil);
                let dim = match desc.dim {
                    ViewDim::Texture1D | ViewDim::Texture2D | ViewDim::Texture2DMs => wgpu::TextureViewDimension::D2,
                    ViewDim::Texture1DArray | ViewDim::Texture2DArray | ViewDim::Texture2DMsArray => {
                        if single {
                            wgpu::TextureViewDimension::D2
                        } else {
                            wgpu::TextureViewDimension::D2Array
                        }
                    }
                    ViewDim::Texture3D => {
                        if single {
                            wgpu::TextureViewDimension::D2
                        } else {
                            wgpu::TextureViewDimension::D3
                        }
                    }
                    ViewDim::TextureCube => wgpu::TextureViewDimension::Cube,
                    ViewDim::TextureCubeArray => wgpu::TextureViewDimension::CubeArray,
                    other => return self.warn(format!("view {id:?}: dimension {other:?} on a texture")),
                };
                let mip = desc.first_mip.min(t.levels - 1);
                let mips = if single || kind == ViewKind::UnorderedAccess {
                    1
                } else if desc.mip_count == u32::MAX || desc.mip_count == 0 {
                    t.levels - mip
                } else {
                    desc.mip_count.min(t.levels - mip)
                };
                let (layer, layers) = match dim {
                    wgpu::TextureViewDimension::D3 => (0, 1),
                    wgpu::TextureViewDimension::D2 if t.desc.dim == TextureDim::D3 => (0, 1),
                    wgpu::TextureViewDimension::D2 => (desc.first_slice.min(t.layers - 1), 1),
                    wgpu::TextureViewDimension::Cube => (desc.first_slice.min(t.layers - 1), 6),
                    _ => {
                        let first = desc.first_slice.min(t.layers - 1);
                        let n = if desc.slice_count == u32::MAX || desc.slice_count == 0 {
                            t.layers - first
                        } else {
                            desc.slice_count.min(t.layers - first)
                        };
                        let n = if dim == wgpu::TextureViewDimension::CubeArray { n / 6 * 6 } else { n };
                        (first, n.max(1))
                    }
                };
                if t.desc.dim == TextureDim::D3 && single && desc.first_slice != 0 {
                    warnings.push("render target views of 3D texture slices other than 0 are not supported yet".into());
                }
                let view = t.gpu.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("d3dgpu view11"),
                    format: Some(format),
                    dimension: Some(dim),
                    usage: None,
                    aspect,
                    base_mip_level: mip,
                    mip_level_count: Some(mips),
                    base_array_layer: layer,
                    array_layer_count: Some(layers),
                });
                let (w, h, _) = t.mip_size(mip);
                ViewRes::Texture { view, format, dim, aspect, mip, size: (w, h), samples: t.gpu.sample_count() }
            }
            _ => return self.warn(format!("CreateView {id:?}: {resource:?} is not a buffer or texture")),
        };
        for w in warnings {
            self.warn_once(w);
        }
        if let (ViewKind::RenderTarget | ViewKind::DepthStencil, ViewRes::Buffer { .. }) = (kind, &res) {
            return self.warn(format!("CreateView {id:?}: render target views of buffers are not supported"));
        }
        self.objects.insert(id.0, Object::View11(Box::new(View11 { id: vid, kind, resource, desc, res })));
    }

    // ---- Updates ----

    fn update_subresource(
        &mut self,
        resource: Handle,
        subresource: u32,
        bx: Option<Box3>,
        row_pitch: u32,
        depth_pitch: u32,
        bytes: &[u8],
    ) {
        match self.objects.get(&resource.0) {
            Some(Object::Buffer11(b)) => {
                let (start, end) = match bx {
                    Some(bx) => (bx.left, bx.right.min(b.size)),
                    None => (0, b.size),
                };
                if end <= start {
                    return;
                }
                let n = ((end - start) as usize).min(bytes.len());
                self.update_buffer11(resource, start, &bytes[..n]);
            }
            Some(Object::Texture11(_)) => {
                self.flush_clears11();
                self.update_texture11(resource, subresource, bx, row_pitch, depth_pitch, bytes)
            }
            _ => self.warn(format!("UpdateSubresource: {resource:?} is not a resource")),
        }
    }

    fn update_buffer11(&mut self, id: Handle, offset: u32, bytes: &[u8]) {
        let epoch = self.epoch;
        let Some(Object::Buffer11(b)) = self.objects.get_mut(&id.0) else { unreachable!() };
        let end = offset as usize + bytes.len();
        b.shadow[offset as usize..end].copy_from_slice(bytes);
        b.version += 1;
        self.stats.bytes_uploaded += bytes.len() as u64;
        let Some(gpu) = b.gpu.clone() else { return }; // constant buffer: shadow only
        let start = offset as usize & !3;
        let stop = end.next_multiple_of(4);
        let in_use = b.last_use == epoch;
        if !in_use {
            self.queue.write_buffer(&gpu, start as u64, &b.shadow[start..stop]);
            return;
        }
        let size = b.shadow.len();
        let whole_cheap = size <= 1 << 20 || (stop - start) * 2 >= size;
        if !b.gpu_written && whole_cheap {
            // Rename: recorded commands keep the old buffer; the new one
            // gets the whole contents through the queue.
            let key = (gpu.size(), b.bind | b.misc << 16);
            let (bind_flags, misc_flags) = (b.bind, b.misc);
            let reuse = self.d11.pool.get_mut(&key).and_then(|v| {
                let i = v.iter().position(|p| p.last_use < epoch)?;
                Some(v.swap_remove(i))
            });
            let (new_gpu, new_gen) = match reuse {
                Some(p) => (p.gpu, p.gen),
                None => {
                    let g = self.new_buffer11(key.0, bind_flags, misc_flags);
                    (g, self.id())
                }
            };
            let Some(Object::Buffer11(b)) = self.objects.get_mut(&id.0) else { unreachable!() };
            self.queue.write_buffer(&new_gpu, 0, &b.shadow);
            let old = Pooled { gpu: b.gpu.replace(new_gpu).unwrap(), gen: b.gen, last_use: epoch };
            b.gen = new_gen;
            b.last_use = 0;
            // Storage buffer bindings name the old buffer.
            self.d11.dirty |= dirty::RES_VS | dirty::RES_PS;
            let list = self.d11.pool.entry(key).or_default();
            if list.len() < 8 {
                list.push(old);
            }
            return;
        }
        // Copy in command order.
        let data = b.shadow[start..stop].to_vec();
        self.end_pass();
        let src = match self.uploads.alloc(&data, 4) {
            Some(o) => o,
            None => {
                self.submit();
                self.queue.write_buffer(&gpu, start as u64, &data);
                return;
            }
        };
        let upload = self.uploads.buffer.clone();
        self.encoder().copy_buffer_to_buffer(&upload, src, &gpu, start as u64, data.len() as u64);
    }

    fn update_texture11(
        &mut self,
        id: Handle,
        sub: u32,
        bx: Option<Box3>,
        row_pitch: u32,
        depth_pitch: u32,
        bytes: &[u8],
    ) {
        let epoch = self.epoch;
        let Some(Object::Texture11(t)) = self.objects.get(&id.0) else { unreachable!() };
        let (mip, layer) = t.subresource(sub);
        if mip >= t.levels || layer >= t.layers {
            return self.warn(format!("UpdateSubresource: subresource {sub} of {id:?} out of range"));
        }
        if !t.gpu.usage().contains(wgpu::TextureUsages::COPY_DST) {
            return self.warn_once(format!("UpdateSubresource: {:?} textures can't be written on WebGPU", t.format));
        }
        let (mw, mh, md) = t.mip_size(mip);
        let bx = bx.unwrap_or(Box3 { left: 0, top: 0, front: 0, right: mw, bottom: mh, back: md });
        let (w, h, d) = (
            bx.right.min(mw).saturating_sub(bx.left),
            bx.bottom.min(mh).saturating_sub(bx.top),
            bx.back.min(md).saturating_sub(bx.front),
        );
        if w == 0 || h == 0 || d == 0 {
            return;
        }
        let f = t.desc.format;
        let block = if f.is_block_compressed() { 4 } else { 1 };
        let Some(row) = f.row_bytes(w) else { return self.warn(format!("UpdateSubresource: format {f:?}")) };
        let rows = f.block_rows(h);
        let need = depth_pitch as usize * (d as usize - 1) + row_pitch as usize * (rows as usize - 1) + row as usize;
        if bytes.len() < need {
            return self.warn(format!("UpdateSubresource: {} bytes for a {need}-byte region", bytes.len()));
        }
        let gpu = t.gpu.clone();
        let is_3d = t.desc.dim == TextureDim::D3;
        let origin = wgpu::Origin3d { x: bx.left, y: bx.top, z: if is_3d { bx.front } else { layer } };
        // Copies cover whole blocks, clamped to the physical mip size.
        let phys = |v: u32, lv: u32| if block > 1 { v.next_multiple_of(4).min(lv.next_multiple_of(4)) } else { v };
        let extent = wgpu::Extent3d {
            width: phys(w, mw.saturating_sub(bx.left)),
            height: phys(h, mh.saturating_sub(bx.top)),
            depth_or_array_layers: d,
        };
        let in_use = t.last_use == epoch;
        let dest =
            wgpu::TexelCopyTextureInfo { texture: &gpu, mip_level: mip, origin, aspect: wgpu::TextureAspect::All };
        self.stats.bytes_uploaded += (row * rows * d) as u64;
        if !in_use {
            let layout = wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row_pitch.max(row)),
                rows_per_image: Some(if d > 1 { depth_pitch / row_pitch.max(1) } else { rows }),
            };
            if d > 1 && !depth_pitch.is_multiple_of(row_pitch.max(1)) {
                return self.warn_once("UpdateSubresource: depth pitch not a multiple of the row pitch");
            }
            self.queue.write_texture(dest, bytes, layout, extent);
            return;
        }
        self.end_pass();
        let aligned = row.next_multiple_of(256);
        let total = aligned as usize * rows as usize * d as usize;
        let Some((offset, dst)) = self.uploads.reserve(total, 256) else {
            self.submit();
            let layout =
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row_pitch), rows_per_image: Some(rows) };
            let dest =
                wgpu::TexelCopyTextureInfo { texture: &gpu, mip_level: mip, origin, aspect: wgpu::TextureAspect::All };
            self.queue.write_texture(dest, bytes, layout, extent);
            return;
        };
        for z in 0..d as usize {
            for r in 0..rows as usize {
                let s = z * depth_pitch as usize + r * row_pitch as usize;
                let o = (z * rows as usize + r) * aligned as usize;
                dst[o..o + row as usize].copy_from_slice(&bytes[s..s + row as usize]);
            }
        }
        let upload = self.uploads.buffer.clone();
        self.encoder().copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: &upload,
                layout: wgpu::TexelCopyBufferLayout {
                    offset,
                    bytes_per_row: Some(aligned),
                    rows_per_image: Some(rows),
                },
            },
            wgpu::TexelCopyTextureInfo { texture: &gpu, mip_level: mip, origin, aspect: wgpu::TextureAspect::All },
            extent,
        );
    }

    // ---- Clears ----

    fn clear_view(&mut self, view: Handle, c: Clear11) {
        let Some(Object::View11(v)) = self.objects.get(&view.0) else {
            return self.warn(format!("clear: {view:?} is not a view"));
        };
        let ViewRes::Texture { format, .. } = v.res else { return };
        let mut c = c;
        if !format.has_stencil_aspect() {
            c.stencil = None;
        }
        if !format.has_depth_aspect() {
            c.depth = None;
        }
        if c.color.is_none() && c.depth.is_none() && c.stencil.is_none() {
            return;
        }
        // A clear of an attachment of the open pass ends it; the next pass
        // on this view loads with the clear.
        if let Some(t) = self.d11.pass {
            if t.colors.contains(&view) || t.depth == view {
                self.end_pass();
            }
        }
        let e = self.d11.clears.entry(view.0).or_default();
        e.color = c.color.or(e.color);
        e.depth = c.depth.or(e.depth);
        e.stencil = c.stencil.or(e.stencil);
        self.stats.load_op_clears += 1;
    }

    /// Runs every deferred clear (each as an empty pass).
    pub(crate) fn flush_clears11(&mut self) {
        if self.d11.clears.is_empty() {
            return;
        }
        self.end_pass();
        let clears: Vec<(u32, Clear11)> = self.d11.clears.drain().collect();
        for (view, c) in clears {
            let Some(Object::View11(v)) = self.objects.get(&view) else { continue };
            let ViewRes::Texture { view: tv, format, .. } = &v.res else { continue };
            let tv = tv.clone();
            let format = *format;
            let resource = v.resource;
            self.touch_texture(resource);
            let enc = self.encoder();
            if format.is_depth_stencil_format() {
                let depth_ops = format.has_depth_aspect().then_some(wgpu::Operations {
                    load: c.depth.map(wgpu::LoadOp::Clear).unwrap_or(wgpu::LoadOp::Load),
                    store: wgpu::StoreOp::Store,
                });
                let stencil_ops = format.has_stencil_aspect().then_some(wgpu::Operations {
                    load: c.stencil.map(wgpu::LoadOp::Clear).unwrap_or(wgpu::LoadOp::Load),
                    store: wgpu::StoreOp::Store,
                });
                enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("d3dgpu clear11"),
                    color_attachments: &[],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &tv,
                        depth_ops,
                        stencil_ops,
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
            } else if let Some(color) = c.color {
                enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("d3dgpu clear11"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &tv,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(color), store: wgpu::StoreOp::Store },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
            }
            self.stats.passes += 1;
        }
    }

    pub(crate) fn touch_texture(&mut self, h: Handle) {
        let epoch = self.epoch;
        if let Some(Object::Texture11(t)) = self.objects.get_mut(&h.0) {
            t.last_use = epoch;
        }
    }

    fn clear_uav(&mut self, view: Handle, values: [u32; 4], float: bool) {
        self.flush_clears11();
        let Some(Object::View11(v)) = self.objects.get(&view.0) else {
            return self.warn(format!("ClearUnorderedAccessView: {view:?} is not a view"));
        };
        let resource = v.resource;
        match &v.res {
            ViewRes::Buffer { offset, size, format } => {
                let (offset, size) = (*offset, *size);
                let word = match format {
                    Some(f) if float && !format_is_float(*f) => (f32::from_bits(values[0]) as u32).to_le_bytes(),
                    _ => values[0].to_le_bytes(),
                };
                let words = format.map(|f| f.words()).unwrap_or(1) as usize;
                let mut pattern = Vec::new();
                for i in 0..words {
                    let v = if words == 1 { u32::from_le_bytes(word) } else { values[i.min(3)] };
                    pattern.extend_from_slice(&v.to_le_bytes());
                }
                let Some(Object::Buffer11(b)) = self.objects.get_mut(&resource.0) else { return };
                b.gpu_written = true;
                b.last_use = self.epoch;
                let Some(gpu) = b.gpu.clone() else { return };
                self.end_pass();
                if pattern.iter().all(|b| *b == 0) {
                    self.encoder().clear_buffer(&gpu, offset, Some(size));
                    return;
                }
                let data: Vec<u8> = pattern.iter().copied().cycle().take(size as usize).collect();
                let Some(src) = self.uploads.alloc(&data, 4) else {
                    return self.warn("ClearUnorderedAccessView: upload ring full");
                };
                let upload = self.uploads.buffer.clone();
                self.encoder().copy_buffer_to_buffer(&upload, src, &gpu, offset, size);
            }
            ViewRes::Texture { format, mip, .. } => {
                let (format, mip) = (*format, *mip);
                let Some(texel) = encode_texel(format, values, float) else {
                    return self.warn_once(format!("ClearUnorderedAccessView on {format:?} is not supported"));
                };
                let Some(Object::Texture11(t)) = self.objects.get(&resource.0) else { return };
                let (w, h, d) = t.mip_size(mip);
                let layers = if t.desc.dim == TextureDim::D3 { d } else { t.layers };
                let gpu = t.gpu.clone();
                self.touch_texture(resource);
                self.end_pass();
                let row = w * texel.len() as u32;
                let aligned = row.next_multiple_of(256);
                let total = aligned as usize * h as usize * layers as usize;
                let Some((offset, dst)) = self.uploads.reserve(total, 256) else {
                    return self.warn("ClearUnorderedAccessView: upload ring full");
                };
                for r in 0..(h * layers) as usize {
                    let line = &mut dst[r * aligned as usize..r * aligned as usize + row as usize];
                    for px in line.chunks_exact_mut(texel.len()) {
                        px.copy_from_slice(&texel);
                    }
                }
                let upload = self.uploads.buffer.clone();
                self.encoder().copy_buffer_to_texture(
                    wgpu::TexelCopyBufferInfo {
                        buffer: &upload,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset,
                            bytes_per_row: Some(aligned),
                            rows_per_image: Some(h),
                        },
                    },
                    wgpu::TexelCopyTextureInfo {
                        texture: &gpu,
                        mip_level: mip,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::Extent3d { width: w, height: h, depth_or_array_layers: layers },
                );
            }
        }
    }

    // ---- Copies ----

    fn copy_resource(&mut self, dst: Handle, src: Handle) {
        match (self.objects.get(&dst.0), self.objects.get(&src.0)) {
            (Some(Object::Buffer11(d)), Some(Object::Buffer11(s))) => {
                let n = d.size.min(s.size);
                self.copy_buffer11(dst, 0, src, 0, n);
            }
            (Some(Object::Texture11(d)), Some(Object::Texture11(s))) => {
                let (levels, layers) = (d.levels.min(s.levels), d.layers.min(s.layers));
                let (dl, sl) = (d.levels, s.levels);
                for layer in 0..layers {
                    for mip in 0..levels {
                        self.copy_texture11(dst, mip + layer * dl, [0; 3], src, mip + layer * sl, None);
                    }
                }
            }
            _ => self.warn("CopyResource: resources are not both buffers or both textures"),
        }
    }

    fn copy_region(&mut self, dst: Handle, dsub: u32, at: [u32; 3], src: Handle, ssub: u32, bx: Option<Box3>) {
        match (self.objects.get(&dst.0), self.objects.get(&src.0)) {
            (Some(Object::Buffer11(_)), Some(Object::Buffer11(s))) => {
                let (l, r) = bx.map(|b| (b.left, b.right)).unwrap_or((0, s.size));
                self.copy_buffer11(dst, at[0], src, l, r.saturating_sub(l));
            }
            (Some(Object::Texture11(_)), Some(Object::Texture11(_))) => {
                self.copy_texture11(dst, dsub, at, src, ssub, bx);
            }
            _ => self.warn("CopySubresourceRegion: resources are not both buffers or both textures"),
        }
    }

    fn copy_buffer11(&mut self, dst: Handle, dst_off: u32, src: Handle, src_off: u32, n: u32) {
        self.flush_clears11();
        let epoch = self.epoch;
        let (Some(Object::Buffer11(s)), true) = (self.objects.get(&src.0), n > 0) else { return };
        if !dst_off.is_multiple_of(4) || !src_off.is_multiple_of(4) {
            return self.warn_once("buffer copies at offsets that aren't multiples of 4 are not supported");
        }
        let src_shadow = (!s.gpu_written).then(|| s.shadow[src_off as usize..(src_off + n) as usize].to_vec());
        let sgpu = s.gpu.clone();
        let Some(Object::Buffer11(d)) = self.objects.get_mut(&dst.0) else { return };
        let end = (dst_off + n).min(d.size);
        let n = end.saturating_sub(dst_off);
        // Keep the shadow valid when the source's is.
        match &src_shadow {
            Some(bytes) => {
                d.shadow[dst_off as usize..end as usize].copy_from_slice(&bytes[..n as usize]);
                d.version += 1;
            }
            None => d.gpu_written = true,
        }
        d.last_use = epoch;
        let Some(dgpu) = d.gpu.clone() else { return }; // constant buffer: shadow updated above
        let Some(sgpu) = sgpu else {
            // From a constant buffer: through the upload ring.
            let bytes = src_shadow.unwrap_or_default();
            self.end_pass();
            if let Some(o) = self.uploads.alloc(&bytes[..(n as usize).next_multiple_of(4).min(bytes.len())], 4) {
                let upload = self.uploads.buffer.clone();
                self.encoder().copy_buffer_to_buffer(&upload, o, &dgpu, dst_off as u64, (n as u64) & !3);
            }
            return;
        };
        if let Some(Object::Buffer11(s)) = self.objects.get_mut(&src.0) {
            s.last_use = epoch;
        }
        self.end_pass();
        self.encoder().copy_buffer_to_buffer(
            &sgpu,
            src_off as u64,
            &dgpu,
            dst_off as u64,
            (n as u64).next_multiple_of(4),
        );
    }

    fn copy_texture11(&mut self, dst: Handle, dsub: u32, at: [u32; 3], src: Handle, ssub: u32, bx: Option<Box3>) {
        self.flush_clears11();
        let (Some(Object::Texture11(d)), Some(Object::Texture11(s))) =
            (self.objects.get(&dst.0), self.objects.get(&src.0))
        else {
            return;
        };
        let ((dmip, dlayer), (smip, slayer)) = (d.subresource(dsub), s.subresource(ssub));
        if dmip >= d.levels || dlayer >= d.layers || smip >= s.levels || slayer >= s.layers {
            return self.warn("copy: subresource out of range");
        }
        if !s.gpu.usage().contains(wgpu::TextureUsages::COPY_SRC)
            || !d.gpu.usage().contains(wgpu::TextureUsages::COPY_DST)
        {
            return self.warn_once(format!("copies between {:?} textures are not possible on WebGPU", s.format));
        }
        let (sw, sh, sd) = s.mip_size(smip);
        let bx = bx.unwrap_or(Box3 { left: 0, top: 0, front: 0, right: sw, bottom: sh, back: sd });
        let extent = wgpu::Extent3d {
            width: bx.right.min(sw).saturating_sub(bx.left),
            height: bx.bottom.min(sh).saturating_sub(bx.top),
            depth_or_array_layers: bx.back.min(sd).saturating_sub(bx.front).max(1),
        };
        let s3d = s.desc.dim == TextureDim::D3;
        let d3d = d.desc.dim == TextureDim::D3;
        let (sg, dg) = (s.gpu.clone(), d.gpu.clone());
        self.touch_texture(src);
        self.touch_texture(dst);
        self.end_pass();
        self.encoder().copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &sg,
                mip_level: smip,
                origin: wgpu::Origin3d { x: bx.left, y: bx.top, z: if s3d { bx.front } else { slayer } },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &dg,
                mip_level: dmip,
                origin: wgpu::Origin3d { x: at[0], y: at[1], z: if d3d { at[2] } else { dlayer } },
                aspect: wgpu::TextureAspect::All,
            },
            extent,
        );
    }

    // ---- Readback ----

    #[allow(clippy::too_many_arguments)]
    fn read_subresource(
        &mut self,
        resource: Handle,
        sub: u32,
        bx: Option<Box3>,
        dest_offset: u32,
        row_pitch: u32,
        depth_pitch: u32,
        fence: u64,
    ) {
        self.flush_clears11();
        let state = Arc::new(AtomicU8::new(0));
        let fail = |core: &mut Core, msg: String| {
            core.warn(msg);
            state.store(2, Ordering::Release);
            core.pending.push_back(Pending { fence, state: state.clone(), kind: PendingKind::Signal });
        };
        let epoch = self.epoch;
        let (buffer, read) = match self.objects.get_mut(&resource.0) {
            Some(Object::Buffer11(b)) => {
                let (l, r) = bx.map(|b| (b.left, b.right)).unwrap_or((0, b.size));
                let (l, r) = (l & !3, r.min(b.size).next_multiple_of(4));
                if r <= l {
                    return fail(self, "ReadSubresource: empty range".into());
                }
                let n = (r - l) as u64;
                b.last_use = epoch;
                let read = RawRead {
                    rows: 1,
                    row_bytes: (r - l) as usize,
                    slices: 1,
                    gpu_row_pitch: n as usize,
                    gpu_slice_pitch: n as usize,
                    dest_offset,
                    row_pitch,
                    depth_pitch,
                };
                let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("d3dgpu readback11"),
                    size: n,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                match b.gpu.clone() {
                    Some(gpu) => {
                        self.end_pass();
                        self.encoder().copy_buffer_to_buffer(&gpu, l as u64, &staging, 0, n);
                    }
                    None => {
                        let data = b.shadow[l as usize..r as usize].to_vec();
                        self.queue.write_buffer(&staging, 0, &data);
                    }
                }
                (staging, read)
            }
            Some(Object::Texture11(t)) => {
                let (mip, layer) = t.subresource(sub);
                if mip >= t.levels || layer >= t.layers {
                    return fail(self, "ReadSubresource: subresource out of range".into());
                }
                if !t.gpu.usage().contains(wgpu::TextureUsages::COPY_SRC) {
                    let f = t.format;
                    return fail(self, format!("ReadSubresource: {f:?} textures can't be read back on WebGPU"));
                }
                t.last_use = epoch;
                let (mw, mh, md) = t.mip_size(mip);
                let bx = bx.unwrap_or(Box3 { left: 0, top: 0, front: 0, right: mw, bottom: mh, back: md });
                let (w, h, d) = (
                    bx.right.min(mw).saturating_sub(bx.left),
                    bx.bottom.min(mh).saturating_sub(bx.top),
                    bx.back.min(md).saturating_sub(bx.front).max(1),
                );
                let f = t.desc.format;
                let aspect = if t.format.is_depth_stencil_format() {
                    wgpu::TextureAspect::DepthOnly
                } else {
                    wgpu::TextureAspect::All
                };
                let copy_format = t.format.aspect_specific_format(aspect).unwrap_or(t.format);
                let Some(bpb) = copy_format.block_copy_size(Some(aspect)) else {
                    return fail(self, format!("ReadSubresource: {f:?} can't be copied"));
                };
                let (bw, bh) = copy_format.block_dimensions();
                let row_bytes = w.div_ceil(bw) * bpb;
                let rows = h.div_ceil(bh);
                let gpu_row = row_bytes.next_multiple_of(256);
                let size = gpu_row as u64 * rows as u64 * d as u64;
                let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("d3dgpu readback11"),
                    size,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let gpu = t.gpu.clone();
                let z = if t.desc.dim == TextureDim::D3 { bx.front } else { layer };
                self.end_pass();
                self.encoder().copy_texture_to_buffer(
                    wgpu::TexelCopyTextureInfo {
                        texture: &gpu,
                        mip_level: mip,
                        origin: wgpu::Origin3d { x: bx.left, y: bx.top, z },
                        aspect,
                    },
                    wgpu::TexelCopyBufferInfo {
                        buffer: &staging,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(gpu_row),
                            rows_per_image: Some(rows),
                        },
                    },
                    wgpu::Extent3d { width: w, height: h, depth_or_array_layers: d },
                );
                let read = RawRead {
                    rows: rows as usize,
                    row_bytes: row_bytes as usize,
                    slices: d as usize,
                    gpu_row_pitch: gpu_row as usize,
                    gpu_slice_pitch: gpu_row as usize * rows as usize,
                    dest_offset,
                    row_pitch,
                    depth_pitch,
                };
                (staging, read)
            }
            _ => return fail(self, format!("ReadSubresource: {resource:?} is not a resource")),
        };
        self.submit();
        let s = state.clone();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |r| s.store(if r.is_ok() { 1 } else { 2 }, Ordering::Release));
        self.pending.push_back(Pending { fence, state, kind: PendingKind::Raw { buffer, read } });
    }

    /// The view, size and alpha handling `Present` uses for a Direct3D 11
    /// texture.
    pub(crate) fn present_source11(&mut self, h: Handle) -> Option<(wgpu::TextureView, (u32, u32), bool)> {
        self.flush_clears11();
        let epoch = self.epoch;
        let Some(Object::Texture11(t)) = self.objects.get_mut(&h.0) else { return None };
        t.last_use = epoch;
        let view = t.gpu.create_view(&wgpu::TextureViewDescriptor {
            label: Some("d3dgpu present11"),
            dimension: Some(wgpu::TextureViewDimension::D2),
            base_mip_level: 0,
            mip_level_count: Some(1),
            base_array_layer: 0,
            array_layer_count: Some(1),
            ..Default::default()
        });
        let (w, h, _) = t.mip_size(0);
        Some((view, (w, h), format::alpha_is_one(t.desc.format)))
    }
}

fn set_range<const N: usize>(slots: &mut [Handle; N], start: u32, views: &[Handle]) {
    for (i, v) in views.iter().enumerate() {
        if let Some(s) = slots.get_mut(start as usize + i) {
            *s = *v;
        }
    }
}

fn format_is_float(f: d3dgpu_dxbc::BufferFormat) -> bool {
    use d3dgpu_dxbc::BufferFormat as B;
    matches!(
        f,
        B::R32Float
            | B::Rg32Float
            | B::Rgb32Float
            | B::Rgba32Float
            | B::Rgba16Float
            | B::R16Float
            | B::Rg16Float
            | B::Rgba8Unorm
    )
}

/// One texel of `f` holding the clear values (raw integers, or float bits
/// when `float`).
fn encode_texel(f: wgpu::TextureFormat, v: [u32; 4], float: bool) -> Option<Vec<u8>> {
    use wgpu::TextureFormat as T;
    let fl = |i: usize| if float { f32::from_bits(v[i]) } else { v[i] as f32 };
    let u = |i: usize| if float { f32::from_bits(v[i]) as u32 } else { v[i] };
    let unorm8 = |i: usize| (fl(i).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    Some(match f {
        T::R32Uint | T::R32Sint => u(0).to_le_bytes().to_vec(),
        T::R32Float => fl(0).to_le_bytes().to_vec(),
        T::Rg32Uint | T::Rg32Sint => [u(0), u(1)].iter().flat_map(|x| x.to_le_bytes()).collect(),
        T::Rg32Float => [fl(0), fl(1)].iter().flat_map(|x| x.to_le_bytes()).collect(),
        T::Rgba32Uint | T::Rgba32Sint => (0..4).flat_map(|i| u(i).to_le_bytes()).collect(),
        T::Rgba32Float => (0..4).flat_map(|i| fl(i).to_le_bytes()).collect(),
        T::Rgba8Unorm => (0..4).map(unorm8).collect(),
        T::Bgra8Unorm => [2, 1, 0, 3].into_iter().map(unorm8).collect(),
        T::Rgba8Uint | T::Rgba8Sint => (0..4).map(|i| u(i) as u8).collect(),
        T::Rgba16Uint | T::Rgba16Sint => (0..4).flat_map(|i| (u(i) as u16).to_le_bytes()).collect(),
        T::Rgba16Float => (0..4).flat_map(|i| d3dgpu_emu::half::f32_to_f16(fl(i)).to_le_bytes()).collect(),
        _ => return None,
    })
}
