//! Draws: shader variants, pipelines, bind groups, index rewrites, uniforms.
//!
//! WebGPU's only dynamic state is viewport, scissor, blend constant and
//! stencil reference, so every other piece of Direct3D state that matters
//! is part of the pipeline key. Pipelines are created synchronously for now
//! (asynchronous creation with an ubershader fallback is the planned fix
//! for first-use stutter).

use std::sync::Arc;

use d3dgpu_emu::index;
use d3dgpu_emu::vertex::{self as vfmt, ShaderInput, VertexOptions};
use d3dgpu_proto::d3d9::*;
use d3dgpu_proto::{Handle, IndexedDraw, Stage, TextureKind};
use d3dgpu_shader::{
    self as sh, ClipMode, ConstLayout, Driver, Fog, InputKind, PixelKey, SamplerDim, SamplerKey, Translation, VertexKey,
};

use crate::convert;
use crate::resources::{FastMap, Object};
use crate::{Core, Stats};

/// Where a draw's vertices and indices come from.
pub enum Source<'a> {
    Vertices { start: u32, count: u32 },
    Indexed(IndexedDraw),
    Up { count: u32, stride: u32, vertices: &'a [u8], indices: Option<(&'a [u8], Format)> },
}

/// The state-derived half of a draw that doesn't depend on the primitive.
pub struct Front {
    vs_module: Arc<sh::ShaderModule>,
    ps_module: Arc<sh::ShaderModule>,
    psv: Arc<Variant>,
    vsv: Arc<Variant>,
    slots: Vec<(u32, VertexBufferKey)>,
    instances: u32,
    targets: crate::PassTargets,
}

/// Pipeline and texture bind group for a state, topology and submission.
pub struct Back {
    pipeline: (u64, wgpu::RenderPipeline),
    group1: (u64, wgpu::BindGroup),
}

type BackKey = (u64, u64, wgpu::PrimitiveTopology, Option<wgpu::IndexFormat>, u64, u64);

pub struct Variant {
    pub module: wgpu::ShaderModule,
    pub translation: Translation,
    pub id: u64,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct LayoutEntry {
    sampler: u32,
    dim: wgpu::TextureViewDimension,
    sample: wgpu::TextureSampleType,
    binding: wgpu::SamplerBindingType,
}

struct Layout {
    id: u64,
    group: wgpu::BindGroupLayout,
    pipeline: wgpu::PipelineLayout,
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
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct PipelineKey {
    vs: u64,
    ps: u64,
    layout: u64,
    buffers: Vec<VertexBufferKey>,
    topology: wgpu::PrimitiveTopology,
    strip_index: Option<wgpu::IndexFormat>,
    cull: Option<wgpu::Face>,
    colors: Vec<Option<ColorKey>>,
    depth: Option<DepthKey>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ClearKey {
    pub colors: [Option<(wgpu::TextureFormat, bool)>; MAX_RENDER_TARGETS],
    pub depth: Option<wgpu::TextureFormat>,
    pub write_depth: bool,
    pub write_stencil: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct SamplerDesc {
    address: [wgpu::AddressMode; 3],
    mag: wgpu::FilterMode,
    min: wgpu::FilterMode,
    mip: wgpu::MipmapFilterMode,
    lod_min: u32,
    lod_max: u32,
    anisotropy: u16,
    compare: bool,
}

struct Dummy {
    view: wgpu::TextureView,
    id: u64,
}

type BindGroupKey = (u64, Vec<(u64, u64)>);

pub struct Caches {
    g0_layout: wgpu::BindGroupLayout,
    g0: wgpu::BindGroup,
    vs: FastMap<(u64, VertexKey), Arc<Variant>>,
    ps: FastMap<(u64, PixelKey), Arc<Variant>>,
    layouts: FastMap<Vec<LayoutEntry>, Arc<Layout>>,
    pipelines: FastMap<PipelineKey, (u64, wgpu::RenderPipeline)>,
    samplers: FastMap<SamplerDesc, (u64, wgpu::Sampler)>,
    bind_groups: FastMap<BindGroupKey, (u64, wgpu::BindGroup)>,
    clear_pipelines: FastMap<ClearKey, wgpu::RenderPipeline>,
    dummies: FastMap<wgpu::TextureViewDimension, Dummy>,
    zero_buffer: wgpu::Buffer,
    driver: (Vec<u8>, u64, u64),
    driver_src: Option<(u64, [u32; 8], Vec<u8>)>,
    front: Option<(u64, u32, Arc<Front>)>,
    back: Option<(BackKey, Arc<Back>)>,
    /// The last translation looked up per stage, checked before hashing.
    last_vs: Option<(u64, VertexKey, Arc<Variant>)>,
    last_ps: Option<(u64, PixelKey, Arc<Variant>)>,
    next_id: u64,
}

const DRIVER_SIZE: u64 = Driver::SIZE as u64;

// Pass cache ids of objects that live as long as the core (created ids
// count up from 1).
const G0_ID: u64 = u64::MAX - 1;
const STREAM_RING_ID: u64 = u64::MAX - 2;
pub(crate) const ZERO_BUFFER_ID: u64 = u64::MAX - 3;

impl Caches {
    pub fn new(device: &wgpu::Device, uniform_ring: &wgpu::Buffer) -> Caches {
        let entry = |binding, visibility, size: u64| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: true,
                min_binding_size: wgpu::BufferSize::new(size),
            },
            count: None,
        };
        use wgpu::ShaderStages as S;
        let g0_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("d3dgpu uniforms"),
            entries: &[
                entry(0, S::VERTEX, ConstLayout::VERTEX.size() as u64),
                entry(1, S::FRAGMENT, ConstLayout::PIXEL.size() as u64),
                entry(2, S::VERTEX | S::FRAGMENT, DRIVER_SIZE),
            ],
        });
        let g0 = Self::uniform_group(device, &g0_layout, uniform_ring);
        // Inputs a declaration lacks read (0, 0, 0, 1), as Direct3D gives
        // them: a missing specular colour has alpha 1, so it does not fog
        // everything when the fog factor comes from it.
        let mut zeros = [0u8; 64];
        zeros[12..16].copy_from_slice(&1.0f32.to_le_bytes());
        let zero_buffer = wgpu::util::DeviceExt::create_buffer_init(
            device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("d3dgpu zero stream"),
                contents: &zeros,
                usage: wgpu::BufferUsages::VERTEX,
            },
        );
        Caches {
            g0_layout,
            g0,
            vs: FastMap::default(),
            ps: FastMap::default(),
            layouts: FastMap::default(),
            pipelines: FastMap::default(),
            samplers: FastMap::default(),
            bind_groups: FastMap::default(),
            clear_pipelines: FastMap::default(),
            dummies: FastMap::default(),
            zero_buffer,
            driver: (Vec::new(), 0, u64::MAX),
            driver_src: None,
            front: None,
            back: None,
            last_vs: None,
            last_ps: None,
            next_id: 1,
        }
    }

    fn uniform_group(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, ring: &wgpu::Buffer) -> wgpu::BindGroup {
        let e = |binding, size: u64| wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer: ring,
                offset: 0,
                size: wgpu::BufferSize::new(size),
            }),
        };
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("d3dgpu uniforms"),
            layout,
            entries: &[
                e(0, ConstLayout::VERTEX.size() as u64),
                e(1, ConstLayout::PIXEL.size() as u64),
                e(2, DRIVER_SIZE),
            ],
        })
    }

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn dummy(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        dim: wgpu::TextureViewDimension,
    ) -> (wgpu::TextureView, u64) {
        if let Some(d) = self.dummies.get(&dim) {
            return (d.view.clone(), d.id);
        }
        let (dimension, layers) = match dim {
            wgpu::TextureViewDimension::Cube => (wgpu::TextureDimension::D2, 6),
            wgpu::TextureViewDimension::D3 => (wgpu::TextureDimension::D3, 1),
            _ => (wgpu::TextureDimension::D2, 1),
        };
        let size = wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: layers };
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("d3dgpu dummy texture"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        // Unbound samplers read (0, 0, 0, 1), as Direct3D 9 drivers return.
        let texels: Vec<u8> = [0u8, 0, 0, 255].repeat(layers as usize);
        queue.write_texture(
            tex.as_image_copy(),
            &texels,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4), rows_per_image: Some(1) },
            size,
        );
        let view = tex.create_view(&wgpu::TextureViewDescriptor { dimension: Some(dim), ..Default::default() });
        let id = self.id();
        self.dummies.insert(dim, Dummy { view: view.clone(), id });
        (view, id)
    }

    fn sampler(&mut self, device: &wgpu::Device, d: SamplerDesc, stats: &mut Stats) -> (u64, wgpu::Sampler) {
        if let Some(s) = self.samplers.get(&d) {
            return s.clone();
        }
        let s = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("d3dgpu sampler"),
            address_mode_u: d.address[0],
            address_mode_v: d.address[1],
            address_mode_w: d.address[2],
            mag_filter: d.mag,
            min_filter: d.min,
            mipmap_filter: d.mip,
            lod_min_clamp: f32::from_bits(d.lod_min),
            lod_max_clamp: f32::from_bits(d.lod_max),
            compare: d.compare.then_some(wgpu::CompareFunction::LessEqual),
            anisotropy_clamp: d.anisotropy,
            border_color: None,
        });
        stats.samplers_created += 1;
        let id = self.id();
        self.samplers.insert(d, (id, s.clone()));
        (id, s)
    }

    fn layout(&mut self, device: &wgpu::Device, entries: Vec<LayoutEntry>) -> Arc<Layout> {
        if let Some(l) = self.layouts.get(&entries) {
            return l.clone();
        }
        let mut list = Vec::new();
        for e in &entries {
            let visibility = if e.sampler >= VERTEX_SAMPLER_BASE {
                wgpu::ShaderStages::VERTEX
            } else {
                wgpu::ShaderStages::FRAGMENT
            };
            list.push(wgpu::BindGroupLayoutEntry {
                binding: sh::texture_binding(e.sampler),
                visibility,
                ty: wgpu::BindingType::Texture { sample_type: e.sample, view_dimension: e.dim, multisampled: false },
                count: None,
            });
            list.push(wgpu::BindGroupLayoutEntry {
                binding: sh::sampler_binding(e.sampler),
                visibility,
                ty: wgpu::BindingType::Sampler(e.binding),
                count: None,
            });
        }
        let group = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("d3dgpu textures"),
            entries: &list,
        });
        let pipeline = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("d3dgpu pipeline layout"),
            bind_group_layouts: &[Some(&self.g0_layout), Some(&group)],
            immediate_size: 0,
        });
        let id = self.id();
        let l = Arc::new(Layout { id, group, pipeline });
        self.layouts.insert(entries, l.clone());
        l
    }

    pub fn clear_pipeline(&mut self, device: &wgpu::Device, key: &ClearKey, stats: &mut Stats) -> wgpu::RenderPipeline {
        if let Some(p) = self.clear_pipelines.get(key) {
            return p.clone();
        }
        let mut wgsl = String::from(
            "@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {\n\
             let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));\n\
             return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);\n}\n",
        );
        let outs: Vec<usize> = (0..MAX_RENDER_TARGETS).filter(|i| key.colors[*i].is_some()).collect();
        if outs.is_empty() {
            wgsl += "@fragment fn fs() {}\n";
        } else {
            wgsl += "struct Out {\n";
            for i in &outs {
                wgsl += &format!("@location({i}) c{i}: vec4<f32>,\n");
            }
            wgsl += "}\n@fragment fn fs() -> Out {\nvar o: Out;\n";
            for i in &outs {
                wgsl += &format!("o.c{i} = vec4<f32>(1.0);\n");
            }
            wgsl += "return o;\n}\n";
        }
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("d3dgpu clear"),
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        });
        // The colour is the blend constant: output 1 * constant + dst * 0.
        let constant = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Constant,
            dst_factor: wgpu::BlendFactor::Zero,
            operation: wgpu::BlendOperation::Add,
        };
        let used = key.colors.iter().rposition(Option::is_some).map_or(0, |i| i + 1);
        let targets: Vec<Option<wgpu::ColorTargetState>> = key.colors[..used]
            .iter()
            .map(|c| {
                c.map(|(format, write)| wgpu::ColorTargetState {
                    format,
                    blend: convert::is_blendable(format)
                        .then_some(wgpu::BlendState { color: constant, alpha: constant }),
                    write_mask: if write && convert::is_blendable(format) {
                        wgpu::ColorWrites::ALL
                    } else {
                        wgpu::ColorWrites::empty()
                    },
                })
            })
            .collect();
        let depth_stencil = key.depth.map(|format| {
            let replace = wgpu::StencilFaceState {
                compare: wgpu::CompareFunction::Always,
                fail_op: wgpu::StencilOperation::Replace,
                depth_fail_op: wgpu::StencilOperation::Replace,
                pass_op: wgpu::StencilOperation::Replace,
            };
            let stencil = if key.write_stencil && format.has_stencil_aspect() {
                wgpu::StencilState { front: replace, back: replace, read_mask: 0xff, write_mask: 0xff }
            } else {
                wgpu::StencilState::default()
            };
            wgpu::DepthStencilState {
                format,
                depth_write_enabled: Some(key.write_depth),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil,
                bias: wgpu::DepthBiasState::default(),
            }
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("d3dgpu clear"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let p = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("d3dgpu clear"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &targets,
            }),
            multiview_mask: None,
            cache: None,
        });
        stats.pipelines_created += 1;
        self.clear_pipelines.insert(*key, p.clone());
        p
    }
}

/// Information about the texture bound to a sampler, gathered before the
/// pipeline is built.
struct Bound {
    sampler: u32,
    view: wgpu::TextureView,
    view_id: u64,
    entry: LayoutEntry,
    desc: SamplerDesc,
}

fn view_dim(d: SamplerDim) -> wgpu::TextureViewDimension {
    match d {
        SamplerDim::D2 => wgpu::TextureViewDimension::D2,
        SamplerDim::Cube => wgpu::TextureViewDimension::Cube,
        SamplerDim::Volume => wgpu::TextureViewDimension::D3,
    }
}

const D3DTTFF_PROJECTED: u32 = 256;
const D3DSHADE_FLAT: u32 = 1;

impl Core {
    fn skip(&mut self, why: impl Into<String>) {
        self.stats.skipped_draws += 1;
        self.warn(why);
    }

    fn translate_ps(&mut self, h: Handle, key: &PixelKey) -> Option<Arc<Variant>> {
        let Some(Object::Shader(s)) = self.objects.get(&h.0) else { return None };
        if let Some((hash, k, v)) = &self.caches.last_ps {
            if *hash == s.hash && k == key {
                return Some(v.clone());
            }
        }
        let ck = (s.hash, key.clone());
        if let Some(v) = self.caches.ps.get(&ck) {
            self.caches.last_ps = Some((ck.0, ck.1, v.clone()));
            return Some(v.clone());
        }
        let t = match s.module.pixel(key) {
            Ok(t) => t,
            Err(e) => {
                self.warn(format!("pixel shader {:016x}: {e}", s.hash));
                return None;
            }
        };
        let v = self.finish_variant(t, ck.clone(), false);
        self.caches.last_ps = Some((ck.0, ck.1, v.clone()));
        Some(v)
    }

    fn translate_vs(&mut self, h: Handle, key: &VertexKey) -> Option<Arc<Variant>> {
        let Some(Object::Shader(s)) = self.objects.get(&h.0) else { return None };
        if let Some((hash, k, v)) = &self.caches.last_vs {
            if *hash == s.hash && k == key {
                return Some(v.clone());
            }
        }
        let ck = (s.hash, key.clone());
        if let Some(v) = self.caches.vs.get(&ck) {
            self.caches.last_vs = Some((ck.0, ck.1, v.clone()));
            return Some(v.clone());
        }
        let t = match s.module.vertex(key) {
            Ok(t) => t,
            Err(e) => {
                self.warn(format!("vertex shader {:016x}: {e}", s.hash));
                return None;
            }
        };
        let hash = ck.0;
        let v = self.finish_variant(t, (hash, PixelKey::default()), true);
        self.caches.vs.insert(ck.clone(), v.clone());
        self.caches.last_vs = Some((ck.0, ck.1, v));
        self.caches.last_vs.as_ref().map(|l| l.2.clone())
    }

    fn finish_variant(&mut self, t: Translation, key: (u64, PixelKey), vertex: bool) -> Arc<Variant> {
        let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(if vertex { "d3dgpu vs" } else { "d3dgpu ps" }),
            source: wgpu::ShaderSource::Wgsl(t.wgsl.as_str().into()),
        });
        self.stats.shader_translations += 1;
        let id = self.caches.id();
        let v = Arc::new(Variant { module, translation: t, id });
        if !vertex {
            self.caches.ps.insert(key, v.clone());
        }
        v
    }

    fn pixel_key(&self, ps: &sh::ShaderModule) -> PixelKey {
        let mut key = PixelKey::default();
        let ps1 = ps.shader.version.major == 1 && ps.shader.version.minor < 4;
        for (n, _) in &ps.reflection.samplers {
            let n = *n as usize;
            if n >= 16 {
                continue;
            }
            let mut sk = SamplerKey::d2();
            if let Some(Object::Texture(t)) = self.objects.get(&self.st.textures[n].0) {
                sk.dim = match t.desc.kind {
                    TextureKind::D2 => SamplerDim::D2,
                    TextureKind::Cube => SamplerDim::Cube,
                    TextureKind::Volume => SamplerDim::Volume,
                };
                sk.swizzle = convert::swizzle(t.plan.swizzle);
                if t.is_depth() {
                    sk.depth = true;
                    // Hardware shadow mapping: ordinary depth formats sample
                    // as a comparison; INTZ and DF16/DF24 return depth.
                    sk.compare = !matches!(t.desc.format, Format::Intz | Format::Df16 | Format::Df24);
                }
            }
            sk.projected = ps1
                && self
                    .st
                    .tss
                    .get(n)
                    .is_some_and(|t| t[TextureStageState::TextureTransformFlags.0 as usize] & D3DTTFF_PROJECTED != 0);
            key.samplers[n] = sk;
        }
        if self.st.r(RenderState::AlphaTestEnable) != 0 {
            key.alpha_test = self.st.r(RenderState::AlphaFunc).min(8) as u8;
        }
        if self.st.r(RenderState::FogEnable) != 0 && ps.shader.version.major < 3 {
            // Without table fog, D3DRS_FOGVERTEXMODE says whether the FOG
            // varying is the factor (NONE: a vertex shader's oFog) or a
            // coordinate for its equation (wined3d's fixed-function
            // vertex shaders; the adapter sends NONE for its own shaders).
            key.fog = match FogMode(self.st.r(RenderState::FogTableMode)) {
                FogMode::Linear => Fog::Linear,
                FogMode::Exp => Fog::Exp,
                FogMode::Exp2 => Fog::Exp2,
                _ => match FogMode(self.st.r(RenderState::FogVertexMode)) {
                    FogMode::Linear => Fog::VertexLinear,
                    FogMode::Exp => Fog::VertexExp,
                    FogMode::Exp2 => Fog::VertexExp2,
                    _ => Fog::Vertex,
                },
            };
            key.fog_w = matches!(key.fog, Fog::Linear | Fog::Exp | Fog::Exp2) && self.st.r(RenderState::WFog) != 0;
        }
        key.clip = self.clip_mode();
        key.flat_shading = self.st.r(RenderState::ShadeMode) == D3DSHADE_FLAT;
        key
    }

    fn clip_mode(&self) -> ClipMode {
        let mask = (self.st.r(RenderState::ClipPlaneEnable) & 0x3f) as u8;
        if mask == 0 || self.st.r(RenderState::Clipping) == 0 {
            ClipMode::None
        } else if self.opts.clip_distances {
            ClipMode::Builtin(mask)
        } else {
            ClipMode::Varying(mask)
        }
    }

    fn driver_bytes(&self, fit: &d3dgpu_emu::viewport::ViewportFit) -> Vec<u8> {
        let st = &self.st;
        let mut d = Driver {
            // The clamp fixup, plus Direct3D 9's half-pixel offset: its pixel
            // centres are at integers, WebGPU's at .5.
            pos_fixup: [fit.scale[0], fit.scale[1], fit.offset[0] + 1.0 / fit.width, fit.offset[1] - 1.0 / fit.height],
            clip_planes: st.clip_planes,
            ..Default::default()
        };
        let fc = st.r(RenderState::FogColor);
        d.fog_color =
            [((fc >> 16) & 0xff) as f32 / 255.0, ((fc >> 8) & 0xff) as f32 / 255.0, (fc & 0xff) as f32 / 255.0, 1.0];
        let (start, end) = (st.rf(RenderState::FogStart), st.rf(RenderState::FogEnd));
        let scale = if end != start { 1.0 / (end - start) } else { 0.0 };
        d.fog_params = [start, end, st.rf(RenderState::FogDensity), scale];
        d.alpha_ref = [(st.r(RenderState::AlphaRef) & 0xff) as f32, 0.0, 0.0, 0.0];
        for (i, t) in st.tss.iter().enumerate() {
            let f = |s: TextureStageState| f32::from_bits(t[s.0 as usize]);
            d.bump_env[i] = [
                f(TextureStageState::BumpEnvMat00),
                f(TextureStageState::BumpEnvMat01),
                f(TextureStageState::BumpEnvMat10),
                f(TextureStageState::BumpEnvMat11),
            ];
            d.bump_lum[i] = [f(TextureStageState::BumpEnvLScale), f(TextureStageState::BumpEnvLOffset), 0.0, 0.0];
        }
        d.to_bytes()
    }

    fn sampler_desc(&self, n: usize, filterable: bool, compare: bool) -> SamplerDesc {
        let s = &self.st.samp[n];
        let g = |st: SamplerState| s[st.0 as usize];
        let address = [
            convert::address_mode(g(SamplerState::AddressU)),
            convert::address_mode(g(SamplerState::AddressV)),
            convert::address_mode(g(SamplerState::AddressW)),
        ];
        let mut mag = convert::filter(g(SamplerState::MagFilter));
        let mut min = convert::filter(g(SamplerState::MinFilter));
        let mip_raw = TextureFilter(g(SamplerState::MipFilter));
        let mut mip = if mip_raw == TextureFilter::Linear {
            wgpu::MipmapFilterMode::Linear
        } else {
            wgpu::MipmapFilterMode::Nearest
        };
        let lod_min = g(SamplerState::MaxMipLevel) as f32;
        let lod_max = if mip_raw == TextureFilter::None { lod_min } else { 32.0 };
        let mut anisotropy = 1;
        let aniso = TextureFilter(g(SamplerState::MinFilter)) == TextureFilter::Anisotropic
            || TextureFilter(g(SamplerState::MagFilter)) == TextureFilter::Anisotropic;
        if !filterable {
            mag = wgpu::FilterMode::Nearest;
            min = wgpu::FilterMode::Nearest;
            mip = wgpu::MipmapFilterMode::Nearest;
        } else if aniso && g(SamplerState::MaxAnisotropy) > 1 {
            // WebGPU requires every filter to be linear for anisotropy.
            mag = wgpu::FilterMode::Linear;
            min = wgpu::FilterMode::Linear;
            mip = wgpu::MipmapFilterMode::Linear;
            anisotropy = g(SamplerState::MaxAnisotropy).min(16) as u16;
        }
        SamplerDesc {
            address,
            mag,
            min,
            mip,
            lod_min: lod_min.to_bits(),
            lod_max: lod_max.max(lod_min).to_bits(),
            anisotropy,
            compare,
        }
    }

    /// Texture and sampler for each texture binding of the two shaders.
    fn bound_textures(&mut self, bindings: &[sh::TextureBinding]) -> Vec<Bound> {
        let mut out = Vec::new();
        let features = self.features;
        for b in bindings {
            let n = b.sampler as usize;
            let want_dim = view_dim(b.dim);
            let tex = match self.objects.get_mut(&self.st.textures[n].0) {
                Some(Object::Texture(t)) => Some(t),
                _ => None,
            };
            let picked = tex.and_then(|t| {
                let dim = match t.desc.kind {
                    TextureKind::D2 => wgpu::TextureViewDimension::D2,
                    TextureKind::Cube => wgpu::TextureViewDimension::Cube,
                    TextureKind::Volume => wgpu::TextureViewDimension::D3,
                };
                if dim != want_dim || t.is_depth() != b.depth {
                    return None;
                }
                t.last_use = self.epoch;
                let aspect = if t.is_depth() { Some(wgpu::TextureAspect::DepthOnly) } else { None };
                let sample = t.format.sample_type(aspect, Some(features))?;
                let srgb = self.st.samp[n][SamplerState::SrgbTexture.0 as usize] != 0;
                let (view, id) = match (&t.srgb_view, srgb) {
                    (Some((v, id)), true) => (v.clone(), *id),
                    _ => (t.view.clone(), t.view_id),
                };
                Some((view, id, sample))
            });
            let (view, view_id, sample) = match picked {
                Some(p) => p,
                None => {
                    if !self.st.textures[n].is_none() {
                        self.warn(format!("sampler {n}: bound texture does not match the shader's {:?}", b.dim));
                    }
                    let (v, id) = self.caches.dummy(&self.device, &self.queue, want_dim);
                    (v, id, wgpu::TextureSampleType::Float { filterable: true })
                }
            };
            let (sample, binding, filterable) = match sample {
                wgpu::TextureSampleType::Depth if b.compare => (sample, wgpu::SamplerBindingType::Comparison, true),
                wgpu::TextureSampleType::Depth => (sample, wgpu::SamplerBindingType::NonFiltering, false),
                wgpu::TextureSampleType::Float { filterable: true } => {
                    (sample, wgpu::SamplerBindingType::Filtering, true)
                }
                _ => (
                    wgpu::TextureSampleType::Float { filterable: false },
                    wgpu::SamplerBindingType::NonFiltering,
                    false,
                ),
            };
            let desc = self.sampler_desc(n, filterable, b.compare);
            out.push(Bound {
                sampler: b.sampler,
                view,
                view_id,
                entry: LayoutEntry { sampler: b.sampler, dim: want_dim, sample, binding },
                desc,
            });
        }
        out
    }

    fn alloc_stream(&mut self, bytes: &[u8]) -> Option<u64> {
        if let Some(o) = self.streams.alloc(bytes, 256) {
            return Some(o);
        }
        self.submit();
        if bytes.len() as u64 > self.streams.size {
            self.streams.grow(&self.device, bytes.len() as u64);
            self.stats.buffers_created += 1;
        }
        self.streams.alloc(bytes, 256)
    }

    fn alloc_uniform(&mut self, bytes: &[u8]) -> u64 {
        if let Some(o) = self.uniforms.alloc(bytes, 256) {
            return o;
        }
        self.submit();
        self.uniforms.alloc(bytes, 256).expect("uniform ring smaller than one draw")
    }

    /// Shader variants, vertex layout and render targets for the current
    /// state.
    fn prepare_front(&mut self, src: &Source) -> Result<Arc<Front>, String> {
        if self.st.vs.is_none() || self.st.ps.is_none() {
            return Err(
                "draw without vertex and pixel shaders (fixed function is not implemented in the core yet)".into()
            );
        }
        let (vs_h, ps_h) = (self.st.vs, self.st.ps);
        let (Some(Object::Shader(vs)), Some(Object::Shader(ps))) =
            (self.objects.get(&vs_h.0), self.objects.get(&ps_h.0))
        else {
            return Err(String::from("draw with an unknown shader handle"));
        };
        let (vs_module, ps_module) = (vs.module.clone(), ps.module.clone());
        let vs_refl = &vs_module.reflection;
        let Some(Object::VertexDecl(decl)) = self.objects.get(&self.st.decl.0) else {
            return Err(String::from("draw without a vertex declaration"));
        };
        let decl = decl.clone();
        let Some(targets) = self.current_targets() else {
            return Err(String::from("draw with no render target bound"));
        };

        // Shaders.
        let pkey = self.pixel_key(&ps_module);
        let Some(psv) = self.translate_ps(ps_h, &pkey) else { return Err("pixel shader failed to translate".into()) };
        let mut vkey =
            VertexKey { outputs: ps_module.linkage(&pkey), clip: pkey.clip, pos_fixup: true, ..Default::default() };

        // Vertex layout: one buffer slot per stream the shader reads.
        let vopts = VertexOptions { bgra_supported: true };
        let mut slots: Vec<(u32, VertexBufferKey)> = Vec::new();
        let mut zero_attrs = Vec::new();
        let freq0 = self.st.streams[0].freq;
        let instances = if freq0 & (1 << 30) != 0 { (freq0 & 0x3fff_ffff).max(1) } else { 1 };
        for (reg, sem) in &vs_refl.vs_inputs {
            let el =
                decl.iter().find(|e| e.usage.0 == sem.usage && e.usage_index == sem.index && e.ty != DeclType::Unused);
            let Some(el) = el else {
                zero_attrs.push((wgpu::VertexFormat::Float32x4, 0, *reg));
                continue;
            };
            let Some((gf, input)) = vfmt::vertex_format(el.ty, &vopts) else {
                return Err(format!("vertex element type {:?} is not supported", el.ty));
            };
            vkey.inputs[*reg as usize] = match input {
                ShaderInput::Float => InputKind::Float,
                ShaderInput::FloatBgra => InputKind::FloatBgra,
                ShaderInput::Uint => InputKind::Uint,
                ShaderInput::Sint => InputKind::Sint,
                ShaderInput::Udec3 => InputKind::Udec3,
                ShaderInput::Dec3n => InputKind::Dec3n,
            };
            let stream = el.stream as u32;
            let stride = match src {
                Source::Up { stride, .. } => *stride,
                _ => self.st.streams.get(stream as usize).map(|s| s.stride).unwrap_or(0),
            };
            if stride % 4 != 0 || el.offset % 4 != 0 {
                return Err(format!(
                    "unaligned vertex layout (stride {stride}, offset {}) needs repacking, not done yet",
                    el.offset
                ));
            }
            let instance =
                self.st.streams.get(stream as usize).is_some_and(|s| s.freq & (1 << 31) != 0) && instances > 1;
            let attr = (convert::vertex_format(gf), el.offset as u32, *reg);
            match slots.iter_mut().find(|(s, _)| *s == stream) {
                Some((_, k)) => k.attrs.push(attr),
                None => slots.push((stream, VertexBufferKey { stride, instance, attrs: vec![attr] })),
            }
        }
        if !zero_attrs.is_empty() {
            slots.push((u32::MAX, VertexBufferKey { stride: 0, instance: false, attrs: zero_attrs }));
        }
        if slots.len() > 8 {
            return Err(String::from("more than 8 vertex streams need repacking, not done yet"));
        }
        let Some(vsv) = self.translate_vs(vs_h, &vkey) else { return Err("vertex shader failed to translate".into()) };

        Ok(Arc::new(Front { vs_module, ps_module, psv, vsv, slots, instances, targets }))
    }

    fn prepare_back(
        &mut self,
        vsv: &Arc<Variant>,
        psv: &Arc<Variant>,
        slots: &[(u32, VertexBufferKey)],
        targets: &crate::PassTargets,
        topology: wgpu::PrimitiveTopology,
        strip_index: Option<wgpu::IndexFormat>,
    ) -> Arc<Back> {
        let mut bindings = vsv.translation.textures.clone();
        bindings.extend(psv.translation.textures.iter().copied());
        let bound = self.bound_textures(&bindings);
        let layout = self.caches.layout(&self.device, bound.iter().map(|b| b.entry.clone()).collect());

        let colors = self.color_states(targets, psv.translation.color_outputs);
        let depth = self.depth_state(targets, topology);
        let cull = match Cull(self.st.r(RenderState::CullMode)) {
            Cull::Cw => Some(wgpu::Face::Front),
            Cull::Ccw => Some(wgpu::Face::Back),
            _ => None,
        };
        let pkey_full = PipelineKey {
            vs: vsv.id,
            ps: psv.id,
            layout: layout.id,
            buffers: slots.iter().map(|(_, k)| k.clone()).collect(),
            topology,
            strip_index,
            cull,
            colors,
            depth,
        };
        let (pipeline_id, pipeline) = self.pipeline(&pkey_full, vsv, psv, &layout);

        // Bind group 1.
        let mut ids = Vec::with_capacity(bound.len());
        let mut samplers = Vec::with_capacity(bound.len());
        for b in &bound {
            let (sid, s) = self.caches.sampler(&self.device, b.desc, &mut self.stats);
            ids.push((b.view_id, sid));
            samplers.push(s);
        }
        let bg_key = (layout.id, ids);
        let group1 = match self.caches.bind_groups.get(&bg_key) {
            Some(g) => g.clone(),
            None => {
                let mut entries = Vec::new();
                for (b, s) in bound.iter().zip(&samplers) {
                    entries.push(wgpu::BindGroupEntry {
                        binding: sh::texture_binding(b.sampler),
                        resource: wgpu::BindingResource::TextureView(&b.view),
                    });
                    entries.push(wgpu::BindGroupEntry {
                        binding: sh::sampler_binding(b.sampler),
                        resource: wgpu::BindingResource::Sampler(s),
                    });
                }
                let g = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("d3dgpu textures"),
                    layout: &layout.group,
                    entries: &entries,
                });
                self.stats.bind_groups_created += 1;
                let g = (self.id(), g);
                self.caches.bind_groups.insert(bg_key, g.clone());
                g
            }
        };

        Arc::new(Back { pipeline: (pipeline_id, pipeline), group1 })
    }

    pub(crate) fn draw(&mut self, prim: PrimitiveType, src: Source) {
        let t = self.now_ns();
        self.draw_inner(prim, src);
        self.stats.draw_ns += self.now_ns() - t;
    }

    fn draw_inner(&mut self, prim: PrimitiveType, src: Source) {
        self.stats.draws += 1;
        // Shaders, vertex layout and targets depend on the state only:
        // reuse them while no state command arrived (constants don't count).
        let up_stride = match &src {
            Source::Up { stride, .. } => *stride,
            _ => u32::MAX,
        };
        let front = match &self.caches.front {
            Some((v, s, f)) if *v == self.st.version && *s == up_stride => f.clone(),
            _ => {
                let t = self.now_ns();
                let f = self.prepare_front(&src);
                self.stats.prepare_ns += self.now_ns() - t;
                match f {
                    Ok(f) => {
                        self.caches.front = Some((self.st.version, up_stride, f.clone()));
                        f
                    }
                    Err(why) => return self.skip(why),
                }
            }
        };
        let (vs_module, ps_module, psv, vsv) =
            (front.vs_module.clone(), front.ps_module.clone(), front.psv.clone(), front.vsv.clone());
        let (slots, instances, targets) = (&front.slots, front.instances, front.targets);

        // Primitive rewrites.
        let fill = FillMode(self.st.r(RenderState::FillMode));
        let tri =
            matches!(prim, PrimitiveType::TriangleList | PrimitiveType::TriangleStrip | PrimitiveType::TriangleFan);
        let (count, prim_n) = match &src {
            Source::Vertices { count, .. } | Source::Up { count, .. } => (prim.vertex_count(*count), *count),
            Source::Indexed(d) => (prim.vertex_count(d.prim_count), d.prim_count),
        };
        if count == 0 {
            return;
        }
        let out_prim = if tri && fill == FillMode::Wireframe {
            PrimitiveType::LineList
        } else if tri && fill == FillMode::Point {
            PrimitiveType::PointList
        } else if prim == PrimitiveType::TriangleFan {
            PrimitiveType::TriangleList
        } else {
            prim
        };
        let topology = match out_prim {
            PrimitiveType::PointList => wgpu::PrimitiveTopology::PointList,
            PrimitiveType::LineList => wgpu::PrimitiveTopology::LineList,
            PrimitiveType::LineStrip => wgpu::PrimitiveTopology::LineStrip,
            PrimitiveType::TriangleStrip => wgpu::PrimitiveTopology::TriangleStrip,
            PrimitiveType::TriangleList => wgpu::PrimitiveTopology::TriangleList,
            _ => return self.skip(format!("primitive type {prim:?}")),
        };
        let rewrite = |indices: Option<IndexData>, first: u32| -> Option<IndexData> {
            let w = |v: Vec<u32>| IndexData::U32(v);
            Some(match indices {
                None => w(if fill == FillMode::Wireframe && tri {
                    index::triangles_to_lines(prim, first, prim_n)
                } else if fill == FillMode::Point && tri {
                    index::triangles_to_points(prim, first, prim_n)
                } else {
                    index::fan_to_list(first, prim_n)
                }),
                Some(IndexData::U16(i)) => IndexData::U16(if fill == FillMode::Wireframe && tri {
                    index::triangles_to_lines_indexed(prim, &i, prim_n)
                } else if fill == FillMode::Point && tri {
                    index::triangles_to_points_indexed(prim, &i, prim_n)
                } else if prim == PrimitiveType::TriangleFan {
                    index::fan_to_list_indexed(&i, prim_n)
                } else {
                    index::to_list_indexed(prim, &i, prim_n)
                }),
                Some(IndexData::U32(i)) => IndexData::U32(if fill == FillMode::Wireframe && tri {
                    index::triangles_to_lines_indexed(prim, &i, prim_n)
                } else if fill == FillMode::Point && tri {
                    index::triangles_to_points_indexed(prim, &i, prim_n)
                } else if prim == PrimitiveType::TriangleFan {
                    index::fan_to_list_indexed(&i, prim_n)
                } else {
                    index::to_list_indexed(prim, &i, prim_n)
                }),
            })
        };
        let needs_rewrite = out_prim != prim;
        let strip = matches!(out_prim, PrimitiveType::TriangleStrip | PrimitiveType::LineStrip);

        // Index and vertex data for this draw. Allocations may submit when a
        // ring fills up; anything allocated before that belongs to the
        // finished submission, so the draw starts over.
        let epoch0 = self.epoch;
        let mut up_vertex_offset = None;
        let mut index_src: Option<(IndexBuf, u32, i32)> = None; // (buffer, index count, base vertex)
        let mut vertex_range = (0u32, count);
        let mut final_prim = out_prim;
        match &src {
            Source::Vertices { start, .. } => {
                if needs_rewrite {
                    let Some(gen) = rewrite(None, *start) else { return };
                    let Some((buf, n)) = self.upload_indices(gen) else { return };
                    index_src = Some((buf, n, 0));
                } else {
                    vertex_range = (*start, *start + count);
                }
            }
            Source::Up { vertices, indices, .. } => {
                let Some(off) = self.alloc_stream(vertices) else { return };
                up_vertex_offset = Some(off);
                match indices {
                    None if needs_rewrite => {
                        let Some(gen) = rewrite(None, 0) else { return };
                        let Some((buf, n)) = self.upload_indices(gen) else { return };
                        index_src = Some((buf, n, 0));
                    }
                    None => {}
                    Some((bytes, f)) => {
                        let data = IndexData::from_bytes(bytes, *f, count as usize);
                        let data = if needs_rewrite || (strip && data.has_restart()) {
                            final_prim = index::list_topology(out_prim).unwrap_or(out_prim);
                            match rewrite(Some(data), 0) {
                                Some(d) => d,
                                None => return,
                            }
                        } else {
                            data
                        };
                        let Some((buf, n)) = self.upload_indices(data) else { return };
                        index_src = Some((buf, n, 0));
                    }
                }
            }
            Source::Indexed(d) => {
                let ib = self.st.indices;
                let f = self.st.index_format;
                let Some(Object::Buffer(b)) = self.objects.get(&ib.0) else {
                    return self.skip("indexed draw without an index buffer");
                };
                let isz = if f == Format::Index32 { 4 } else { 2 };
                let begin = d.start_index as usize * isz;
                let end = begin + count as usize * isz;
                if end > b.size as usize {
                    return self.skip("index range past the end of the index buffer");
                }
                let slice = &b.shadow[begin..end];
                let restart = strip && IndexData::from_bytes(slice, f, count as usize).has_restart();
                if needs_rewrite || restart {
                    let data = IndexData::from_bytes(slice, f, count as usize);
                    final_prim = index::list_topology(out_prim).unwrap_or(out_prim);
                    let Some(gen) = rewrite(Some(data), 0) else { return };
                    let Some((buf, n)) = self.upload_indices(gen) else { return };
                    index_src = Some((buf, n, d.base_vertex));
                } else {
                    let fmt = if isz == 4 { wgpu::IndexFormat::Uint32 } else { wgpu::IndexFormat::Uint16 };
                    index_src = Some((IndexBuf::Object(ib, fmt, d.start_index), count, d.base_vertex));
                }
            }
        }
        let topology = if final_prim != out_prim {
            match final_prim {
                PrimitiveType::LineList => wgpu::PrimitiveTopology::LineList,
                PrimitiveType::TriangleList => wgpu::PrimitiveTopology::TriangleList,
                _ => topology,
            }
        } else {
            topology
        };
        let strip_index = match (topology, &index_src) {
            (wgpu::PrimitiveTopology::TriangleStrip | wgpu::PrimitiveTopology::LineStrip, Some((b, _, _))) => {
                Some(b.format())
            }
            _ => None,
        };

        // Viewport and the driver uniforms that depend on it.
        let vp = self.st.viewport;
        let Some(fit) = d3dgpu_emu::viewport::fit_viewport(
            vp.x as i64,
            vp.y as i64,
            vp.width,
            vp.height,
            targets.width,
            targets.height,
        ) else {
            return; // nothing visible
        };
        let mut scissor = crate::Rect::new(0, 0, targets.width as i32, targets.height as i32);
        if self.st.r(RenderState::ScissorTestEnable) != 0 {
            scissor = crate::intersect(scissor, self.st.scissor);
        }

        // Textures, layout, pipeline and bind group: also state-only, and
        // they mark the textures used in this submission.
        let back_key = (self.st.version, self.epoch, topology, strip_index, vsv.id, psv.id);
        let back = match &self.caches.back {
            Some((k, b)) if *k == back_key => b.clone(),
            _ => {
                let t = self.now_ns();
                let b = self.prepare_back(&vsv, &psv, slots, &targets, topology, strip_index);
                self.stats.prepare_ns += self.now_ns() - t;
                self.caches.back = Some((back_key, b.clone()));
                b
            }
        };
        let (pipeline_id, pipeline) = back.pipeline.clone();
        let group1 = back.group1.clone();

        // Uniforms (uploaded only when changed this submission).
        let vs_off = self.upload_consts(Stage::Vertex, &vs_module.reflection);
        let ps_off = self.upload_consts(Stage::Pixel, &ps_module.reflection);
        let fit_key = [fit.x, fit.y, fit.width, fit.height, fit.scale[0], fit.scale[1], fit.offset[0], fit.offset[1]]
            .map(f32::to_bits);
        let drv = match &self.caches.driver_src {
            Some((v, k, d)) if *v == self.st.version && *k == fit_key => d.clone(),
            _ => {
                let d = self.driver_bytes(&fit);
                self.caches.driver_src = Some((self.st.version, fit_key, d.clone()));
                d
            }
        };
        let drv_off = if self.caches.driver.2 == self.epoch && self.caches.driver.0 == drv {
            self.caches.driver.1
        } else {
            let off = self.alloc_uniform(&drv);
            self.caches.driver = (drv, off, self.epoch);
            off
        };
        if self.epoch != epoch0 {
            self.st.vs_consts.dirty = true;
            self.st.ps_consts.dirty = true;
            self.stats.draws -= 1;
            return self.draw_inner(prim, src);
        }

        // Vertex buffers to bind.
        let epoch = self.epoch;
        let mut vbufs: Vec<(wgpu::Buffer, u64, u64)> = Vec::new();
        for (stream, _) in slots.iter() {
            if *stream == u32::MAX {
                vbufs.push((self.caches.zero_buffer.clone(), ZERO_BUFFER_ID, 0));
                continue;
            }
            if let Some(off) = up_vertex_offset {
                vbufs.push((self.streams.buffer.clone(), STREAM_RING_ID, off));
                continue;
            }
            let s = self.st.streams[*stream as usize];
            match self.objects.get_mut(&s.buffer.0) {
                Some(Object::Buffer(b)) if (s.offset as u64) < b.gpu.size() => {
                    b.last_use = epoch;
                    vbufs.push((b.gpu.clone(), b.id, s.offset as u64));
                }
                _ => return self.skip(format!("stream {stream} has no vertex buffer")),
            }
        }
        let index = match &index_src {
            Some((IndexBuf::Ring(off, f), n, base)) => {
                Some((self.streams.buffer.clone(), STREAM_RING_ID, *off, *f, 0, *n, *base))
            }
            Some((IndexBuf::Object(h, f, start), n, base)) => match self.objects.get_mut(&h.0) {
                Some(Object::Buffer(b)) => {
                    b.last_use = epoch;
                    Some((b.gpu.clone(), b.id, 0, *f, *start, *n, *base))
                }
                _ => return self.skip("index buffer vanished"),
            },
            None => None,
        };

        // Record.
        let t_record = self.now_ns();
        if self.ensure_pass().is_none() {
            return;
        }
        let blend_factor = convert::color(self.st.r(RenderState::BlendFactor));
        let stencil_ref = self.st.r(RenderState::StencilRef) & 0xff;
        let g0 = self.caches.g0.clone();
        let (pass, pc) = (self.pass.as_mut().unwrap(), &mut self.pc);
        pc.set_pipeline(pass, pipeline_id, &pipeline);
        pc.set_bind_group(pass, 0, G0_ID, &g0, &[vs_off as u32, ps_off as u32, drv_off as u32]);
        pc.set_bind_group(pass, 1, group1.0, &group1.1, &[]);
        for (i, (b, id, off)) in vbufs.iter().enumerate() {
            pc.set_vertex_buffer(pass, i as u32, *id, b, *off);
        }
        let min_z = vp.min_z.clamp(0.0, 1.0);
        pc.set_viewport(pass, [fit.x, fit.y, fit.width, fit.height, min_z, vp.max_z.clamp(min_z, 1.0)]);
        pc.set_scissor(pass, [scissor.x1 as u32, scissor.y1 as u32, scissor.width(), scissor.height()]);
        pc.set_blend_constant(pass, blend_factor);
        pc.set_stencil_reference(pass, stencil_ref);
        match index {
            Some((buf, id, off, f, start, n, base)) => {
                pc.set_index_buffer(pass, id, &buf, off, f);
                pass.draw_indexed(start..start + n, base, 0..instances);
            }
            None => pass.draw(vertex_range.0..vertex_range.1, 0..instances),
        }
        self.stats.record_ns += self.now_ns() - t_record;
    }

    /// Uploads the constants a shader reads, unless this submission already
    /// has an upload of them that is still current.
    fn upload_consts(&mut self, stage: Stage, refl: &sh::Reflection) -> u64 {
        let epoch = self.epoch;
        let c = match stage {
            Stage::Vertex => &self.st.vs_consts,
            Stage::Pixel => &self.st.ps_consts,
        };
        let count = c.layout.float_count;
        let floats = if refl.relative_consts { count } else { refl.float_consts.min(count) };
        let need = (floats, refl.int_consts != 0, refl.bool_consts != 0);
        let covered = c.copied.0 >= need.0 && (c.copied.1 || !need.1) && (c.copied.2 || !need.2);
        if !c.dirty && c.epoch == epoch && covered {
            return c.offset;
        }
        let size = c.layout.size() as usize;
        // Only what the shader reads needs to be written; the integer and
        // boolean sections sit at fixed offsets after all float constants.
        let len = if need.1 || need.2 { size } else { (floats as usize * 16).max(16) };
        let (offset, dst) = match self.uniforms.reserve_window(len, size, 256) {
            Some(r) => r,
            None => {
                self.submit();
                self.uniforms.reserve_window(len, size, 256).expect("uniform ring smaller than one constant block")
            }
        };
        let c = match stage {
            Stage::Vertex => &mut self.st.vs_consts,
            Stage::Pixel => &mut self.st.ps_consts,
        };
        let f = floats as usize * 16;
        dst[..f].copy_from_slice(&c.bytes[..f]);
        let (io, bo) = (c.layout.int_offset() as usize, c.layout.bool_offset() as usize);
        if need.1 {
            dst[io..bo].copy_from_slice(&c.bytes[io..bo]);
        }
        if need.2 {
            dst[bo..].copy_from_slice(&c.bytes[bo..]);
        }
        self.stats.bytes_uploaded += (f + if need.1 { 256 } else { 0 } + if need.2 { 64 } else { 0 }) as u64;
        c.offset = offset;
        c.epoch = self.epoch;
        c.dirty = false;
        c.copied = need;
        offset
    }

    fn upload_indices(&mut self, data: IndexData) -> Option<(IndexBuf, u32)> {
        let (bytes, f, n) = data.into_bytes();
        let off = self.alloc_stream(&bytes)?;
        Some((IndexBuf::Ring(off, f), n))
    }

    fn color_states(&self, targets: &crate::PassTargets, outputs: u8) -> Vec<Option<ColorKey>> {
        let st = &self.st;
        let blend_on = st.r(RenderState::AlphaBlendEnable) != 0;
        let separate = st.r(RenderState::SeparateAlphaBlendEnable) != 0;
        let masks = [
            st.r(RenderState::ColorWriteEnable),
            st.r(RenderState::ColorWriteEnable1),
            st.r(RenderState::ColorWriteEnable2),
            st.r(RenderState::ColorWriteEnable3),
        ];
        let mut out: Vec<Option<ColorKey>> = targets
            .colors
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let t = (*t)?;
                let Some(Object::Texture(tex)) = self.objects.get(&t.texture.0) else { return None };
                let a1 = tex.plan.alpha_is_one();
                let blend = (blend_on && convert::is_blendable(tex.format)).then(|| {
                    let color = convert::blend_component(
                        st.r(RenderState::SrcBlend),
                        st.r(RenderState::DestBlend),
                        st.r(RenderState::BlendOp),
                        a1,
                    );
                    let alpha = if separate {
                        convert::blend_component(
                            st.r(RenderState::SrcBlendAlpha),
                            st.r(RenderState::DestBlendAlpha),
                            st.r(RenderState::BlendOpAlpha),
                            a1,
                        )
                    } else {
                        color
                    };
                    (color, alpha)
                });
                // A target the shader doesn't write must have no write mask.
                let mask = if outputs & (1 << i) != 0 { masks[i] & 0xf } else { 0 };
                Some(ColorKey { format: tex.format, blend, mask })
            })
            .collect();
        while out.last().is_some_and(Option::is_none) {
            out.pop();
        }
        out
    }

    fn depth_state(&self, targets: &crate::PassTargets, topology: wgpu::PrimitiveTopology) -> Option<DepthKey> {
        let st = &self.st;
        let d = targets.depth?;
        let Some(Object::Texture(t)) = self.objects.get(&d.texture.0) else { return None };
        let format = t.format;
        let z = st.r(RenderState::ZEnable) != 0;
        let (write, compare) = if z {
            (st.r(RenderState::ZWriteEnable) != 0, convert::compare(st.r(RenderState::ZFunc)))
        } else {
            (false, wgpu::CompareFunction::Always)
        };
        let stencil = (st.r(RenderState::StencilEnable) != 0 && format.has_stencil_aspect()).then(|| {
            let face = |fail, zfail, pass, func| wgpu::StencilFaceState {
                compare: convert::compare(st.r(func)),
                fail_op: convert::stencil_op(st.r(fail)),
                depth_fail_op: convert::stencil_op(st.r(zfail)),
                pass_op: convert::stencil_op(st.r(pass)),
            };
            let front = face(
                RenderState::StencilFail,
                RenderState::StencilZFail,
                RenderState::StencilPass,
                RenderState::StencilFunc,
            );
            let back = if st.r(RenderState::TwoSidedStencilMode) != 0 {
                face(
                    RenderState::CcwStencilFail,
                    RenderState::CcwStencilZFail,
                    RenderState::CcwStencilPass,
                    RenderState::CcwStencilFunc,
                )
            } else {
                front
            };
            (front, back, st.r(RenderState::StencilMask) & 0xff, st.r(RenderState::StencilWriteMask) & 0xff)
        });
        // WebGPU rejects depth bias on point and line topologies.
        let tri = matches!(topology, wgpu::PrimitiveTopology::TriangleList | wgpu::PrimitiveTopology::TriangleStrip);
        let (bias, slope) = if tri {
            (
                (st.rf(RenderState::DepthBias) * convert::depth_bias_scale(format)) as i32,
                st.rf(RenderState::SlopeScaleDepthBias),
            )
        } else {
            (0, 0.0)
        };
        Some(DepthKey { format, write, compare, stencil, bias, slope: slope.to_bits() })
    }

    fn pipeline(
        &mut self,
        key: &PipelineKey,
        vs: &Variant,
        ps: &Variant,
        layout: &Layout,
    ) -> (u64, wgpu::RenderPipeline) {
        if let Some(p) = self.caches.pipelines.get(key) {
            return p.clone();
        }
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
            bias: wgpu::DepthBiasState { constant: d.bias, slope_scale: f32::from_bits(d.slope), clamp: 0.0 },
        });
        let p = self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("d3dgpu pipeline"),
            layout: Some(&layout.pipeline),
            vertex: wgpu::VertexState {
                module: &vs.module,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                buffers: &buffers,
            },
            primitive: wgpu::PrimitiveState {
                topology: key.topology,
                strip_index_format: key.strip_index,
                // Direct3D 9's default (D3DCULL_CCW) culls counter-clockwise
                // triangles: clockwise is front.
                front_face: wgpu::FrontFace::Cw,
                cull_mode: key.cull,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &ps.module,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                targets: &targets,
            }),
            multiview_mask: None,
            cache: None,
        });
        self.stats.pipelines_created += 1;
        let p = (self.id(), p);
        self.caches.pipelines.insert(key.clone(), p.clone());
        p
    }
}

enum IndexBuf {
    Ring(u64, wgpu::IndexFormat),
    /// An index buffer object, its format and the first index.
    Object(Handle, wgpu::IndexFormat, u32),
}

impl IndexBuf {
    fn format(&self) -> wgpu::IndexFormat {
        match self {
            IndexBuf::Ring(_, f) | IndexBuf::Object(_, f, _) => *f,
        }
    }
}

enum IndexData {
    U16(Vec<u16>),
    U32(Vec<u32>),
}

impl IndexData {
    fn from_bytes(b: &[u8], f: Format, count: usize) -> IndexData {
        if f == Format::Index32 {
            IndexData::U32(
                b.as_chunks::<4>().0.iter().take(count).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect(),
            )
        } else {
            IndexData::U16(b.as_chunks::<2>().0.iter().take(count).map(|c| u16::from_le_bytes([c[0], c[1]])).collect())
        }
    }

    fn has_restart(&self) -> bool {
        match self {
            IndexData::U16(v) => index::has_restart(v),
            IndexData::U32(v) => index::has_restart(v),
        }
    }

    fn into_bytes(self) -> (Vec<u8>, wgpu::IndexFormat, u32) {
        match self {
            IndexData::U16(v) => {
                let n = v.len() as u32;
                let mut b: Vec<u8> = v.iter().flat_map(|i| i.to_le_bytes()).collect();
                b.resize(b.len().next_multiple_of(4), 0);
                (b, wgpu::IndexFormat::Uint16, n)
            }
            IndexData::U32(v) => {
                let n = v.len() as u32;
                (v.iter().flat_map(|i| i.to_le_bytes()).collect(), wgpu::IndexFormat::Uint32, n)
            }
        }
    }
}
