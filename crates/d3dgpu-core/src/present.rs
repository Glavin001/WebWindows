//! Presentation and texture-to-texture blits.
//!
//! The back buffer is a texture the core owns; presenting blits it into the
//! output (a canvas's current texture, a native surface, or a headless
//! front buffer), forcing alpha to one for back buffers without alpha and
//! applying the gamma ramp. Keeping the back buffer owned means frames that
//! draw without clearing still see the previous frame, as Direct3D does.

use std::any::Any;

use d3dgpu_proto::Rect;

use crate::resources::Ring;

/// Everything a presenter may use while recording a present.
pub struct PresentContext<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub encoder: &'a mut wgpu::CommandEncoder,
    pub(crate) blitter: &'a mut Blitter,
    pub(crate) uniforms: &'a mut Ring,
}

/// Parameters of one blit.
#[derive(Clone, Copy, Debug)]
pub struct BlitParams<'a> {
    pub src_rect: Rect,
    pub src_size: (u32, u32),
    pub dst_rect: Rect,
    /// Linear filtering when scaling.
    pub linear: bool,
    /// Whether the source can be sampled with a filtering sampler.
    pub filterable: bool,
    /// Write alpha = 1 (back buffers without alpha).
    pub alpha_one: bool,
    /// Gamma lookup table (256 x 1, red/green/blue in the first three
    /// channels).
    pub gamma: Option<&'a wgpu::TextureView>,
}

impl PresentContext<'_> {
    /// Draws `src_rect` of `src` into `dst_rect` of `dst`.
    pub fn blit(
        &mut self,
        src: &wgpu::TextureView,
        dst: &wgpu::TextureView,
        dst_format: wgpu::TextureFormat,
        p: &BlitParams,
    ) {
        let b = &mut *self.blitter;
        let pipeline = b.pipeline(self.device, dst_format, p.filterable);
        let (sw, sh) = (p.src_size.0.max(1) as f32, p.src_size.1.max(1) as f32);
        let mut params = [0f32; 8];
        params[0] = p.src_rect.x1 as f32 / sw;
        params[1] = p.src_rect.y1 as f32 / sh;
        params[2] = p.src_rect.width() as f32 / sw;
        params[3] = p.src_rect.height() as f32 / sh;
        params[4] = p.alpha_one as u32 as f32;
        params[5] = p.gamma.is_some() as u32 as f32;
        let bytes: Vec<u8> = params.iter().flat_map(|f| f.to_le_bytes()).collect();
        let offset = match self.uniforms.alloc(&bytes, 256) {
            Some(o) => o,
            None => return, // the ring is sized far beyond a frame's blits
        };
        let lut = p.gamma.cloned().unwrap_or_else(|| b.identity_lut(self.device, self.queue));
        let sampler = if p.linear && p.filterable { &b.linear } else { &b.nearest };
        let layout = if p.filterable { &b.layout_filterable } else { &b.layout_unfilterable };
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("d3dgpu blit"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &self.uniforms.buffer,
                        offset: 0,
                        size: wgpu::BufferSize::new(32),
                    }),
                },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(src) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(sampler) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&lut) },
            ],
        });
        let mut pass = self.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("d3dgpu blit"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: dst,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[offset as u32]);
        let r = p.dst_rect;
        pass.set_viewport(r.x1 as f32, r.y1 as f32, r.width() as f32, r.height() as f32, 0.0, 1.0);
        pass.set_scissor_rect(r.x1.max(0) as u32, r.y1.max(0) as u32, r.width(), r.height());
        pass.draw(0..3, 0..1);
    }
}

const BLIT_WGSL: &str = "
struct P { src: vec4<f32>, flags: vec4<f32> }
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var smp: sampler;
@group(0) @binding(3) var lut: texture_2d<f32>;
struct V { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex fn vs(@builtin(vertex_index) i: u32) -> V {
    let t = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var o: V;
    o.pos = vec4<f32>(t.x * 2.0 - 1.0, 1.0 - t.y * 2.0, 0.0, 1.0);
    o.uv = p.src.xy + t * p.src.zw;
    return o;
}
fn ramp(v: f32, c: i32) -> f32 {
    let i = i32(clamp(v, 0.0, 1.0) * 255.0 + 0.5);
    return textureLoad(lut, vec2<i32>(i, 0), 0)[c];
}
@fragment fn fs(v: V) -> @location(0) vec4<f32> {
    var c = textureSampleLevel(src, smp, v.uv, 0.0);
    if (p.flags.x > 0.5) { c.w = 1.0; }
    if (p.flags.y > 0.5) { c = vec4<f32>(ramp(c.x, 0), ramp(c.y, 1), ramp(c.z, 2), c.w); }
    return c;
}
";

pub struct Blitter {
    module: wgpu::ShaderModule,
    layout_filterable: wgpu::BindGroupLayout,
    layout_unfilterable: wgpu::BindGroupLayout,
    pipe_filterable: wgpu::PipelineLayout,
    pipe_unfilterable: wgpu::PipelineLayout,
    linear: wgpu::Sampler,
    nearest: wgpu::Sampler,
    pipelines: std::collections::HashMap<(wgpu::TextureFormat, bool), wgpu::RenderPipeline>,
    identity: Option<wgpu::TextureView>,
}

impl Blitter {
    pub fn new(device: &wgpu::Device) -> Blitter {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("d3dgpu blit"),
            source: wgpu::ShaderSource::Wgsl(BLIT_WGSL.into()),
        });
        let layout = |filterable: bool| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("d3dgpu blit"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: true,
                            min_binding_size: wgpu::BufferSize::new(32),
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(if filterable {
                            wgpu::SamplerBindingType::Filtering
                        } else {
                            wgpu::SamplerBindingType::NonFiltering
                        }),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                ],
            })
        };
        let layout_filterable = layout(true);
        let layout_unfilterable = layout(false);
        let pl = |l: &wgpu::BindGroupLayout| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("d3dgpu blit"),
                bind_group_layouts: &[Some(l)],
                immediate_size: 0,
            })
        };
        let pipe_filterable = pl(&layout_filterable);
        let pipe_unfilterable = pl(&layout_unfilterable);
        let sampler = |f: wgpu::FilterMode| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("d3dgpu blit"),
                mag_filter: f,
                min_filter: f,
                ..Default::default()
            })
        };
        Blitter {
            module,
            layout_filterable,
            layout_unfilterable,
            pipe_filterable,
            pipe_unfilterable,
            linear: sampler(wgpu::FilterMode::Linear),
            nearest: sampler(wgpu::FilterMode::Nearest),
            pipelines: Default::default(),
            identity: None,
        }
    }

    fn pipeline(
        &mut self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        filterable: bool,
    ) -> wgpu::RenderPipeline {
        if let Some(p) = self.pipelines.get(&(format, filterable)) {
            return p.clone();
        }
        let p = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("d3dgpu blit"),
            layout: Some(if filterable { &self.pipe_filterable } else { &self.pipe_unfilterable }),
            vertex: wgpu::VertexState {
                module: &self.module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &self.module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });
        self.pipelines.insert((format, filterable), p.clone());
        p
    }

    fn identity_lut(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::TextureView {
        if let Some(v) = &self.identity {
            return v.clone();
        }
        let ramp: [[u16; 256]; 3] = std::array::from_fn(|_| std::array::from_fn(|i| (i * 257) as u16));
        let v = gamma_lut(device, queue, &ramp);
        self.identity = Some(v.clone());
        v
    }
}

/// A 256 x 1 lookup texture from a `D3DGAMMARAMP`.
pub fn gamma_lut(device: &wgpu::Device, queue: &wgpu::Queue, ramp: &[[u16; 256]; 3]) -> wgpu::TextureView {
    let size = wgpu::Extent3d { width: 256, height: 1, depth_or_array_layers: 1 };
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("d3dgpu gamma"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let mut texels = Vec::with_capacity(1024);
    for ((r, g), b) in ramp[0].iter().zip(&ramp[1]).zip(&ramp[2]) {
        texels.extend_from_slice(&[(r >> 8) as u8, (g >> 8) as u8, (b >> 8) as u8, 255]);
    }
    queue.write_texture(
        tex.as_image_copy(),
        &texels,
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(1024), rows_per_image: Some(1) },
        size,
    );
    tex.create_view(&Default::default())
}

/// Where frames for one window go.
pub trait Presenter: Any {
    /// Records the blit of a frame (`size` is the source's size).
    fn present(&mut self, ctx: &mut PresentContext, src: &wgpu::TextureView, size: (u32, u32), alpha_is_one: bool);
    /// Called after the submit that carries the frame.
    fn after_submit(&mut self, _queue: &wgpu::Queue) {}
    fn set_gamma_ramp(&mut self, _ramp: &[[u16; 256]; 3]) {}
    fn as_any(&mut self) -> &mut dyn Any;
}

struct Gamma {
    ramp: Option<[[u16; 256]; 3]>,
    lut: Option<wgpu::TextureView>,
}

impl Gamma {
    fn new() -> Gamma {
        Gamma { ramp: None, lut: None }
    }
    fn set(&mut self, ramp: &[[u16; 256]; 3]) {
        let identity = ramp.iter().all(|c| c.iter().enumerate().all(|(i, v)| (*v >> 8) as usize == i));
        self.ramp = (!identity).then_some(*ramp);
        self.lut = None;
    }
    fn view(&mut self, ctx: &PresentContext) -> Option<wgpu::TextureView> {
        let ramp = self.ramp.as_ref()?;
        Some(self.lut.get_or_insert_with(|| gamma_lut(ctx.device, ctx.queue, ramp)).clone())
    }
}

/// Keeps the last presented frame in a texture of its own (RGBA8), for
/// tests, screenshots and hosts without a surface.
pub struct HeadlessPresenter {
    front: Option<(wgpu::Texture, (u32, u32))>,
    gamma: Gamma,
    pub frames: u64,
}

impl Default for HeadlessPresenter {
    fn default() -> Self {
        Self::new()
    }
}

impl HeadlessPresenter {
    pub fn new() -> HeadlessPresenter {
        HeadlessPresenter { front: None, gamma: Gamma::new(), frames: 0 }
    }

    pub fn front_buffer(&self) -> Option<&wgpu::Texture> {
        self.front.as_ref().map(|(t, _)| t)
    }

    /// The last frame as tightly packed RGBA8 rows (blocks; native only).
    pub fn read_pixels(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Option<(u32, u32, Vec<u8>)> {
        let (tex, (w, h)) = self.front.as_ref()?;
        Some((*w, *h, read_rgba8(device, queue, tex, *w, *h)))
    }
}

impl Presenter for HeadlessPresenter {
    fn present(&mut self, ctx: &mut PresentContext, src: &wgpu::TextureView, size: (u32, u32), alpha_is_one: bool) {
        if self.front.as_ref().is_none_or(|(_, s)| *s != size) {
            let tex = ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("d3dgpu front buffer"),
                size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            self.front = Some((tex, size));
        }
        let gamma = self.gamma.view(ctx);
        let (tex, _) = self.front.as_ref().unwrap();
        let dst = tex.create_view(&Default::default());
        let full = Rect::new(0, 0, size.0 as i32, size.1 as i32);
        let p = BlitParams {
            src_rect: full,
            src_size: size,
            dst_rect: full,
            linear: false,
            filterable: true,
            alpha_one: alpha_is_one,
            gamma: gamma.as_ref(),
        };
        ctx.blit(src, &dst, wgpu::TextureFormat::Rgba8Unorm, &p);
        self.frames += 1;
    }

    fn set_gamma_ramp(&mut self, ramp: &[[u16; 256]; 3]) {
        self.gamma.set(ramp);
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// Presents to a `wgpu::Surface`: a native window, or an `OffscreenCanvas`
/// in the browser render worker. The host configures the surface; frames
/// are scaled to its size.
pub struct SurfacePresenter {
    pub surface: wgpu::Surface<'static>,
    pub config: wgpu::SurfaceConfiguration,
    frame: Option<wgpu::SurfaceTexture>,
    gamma: Gamma,
}

impl SurfacePresenter {
    pub fn new(
        device: &wgpu::Device,
        surface: wgpu::Surface<'static>,
        config: wgpu::SurfaceConfiguration,
    ) -> SurfacePresenter {
        surface.configure(device, &config);
        SurfacePresenter { surface, config, frame: None, gamma: Gamma::new() }
    }

    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(device, &self.config);
    }
}

impl Presenter for SurfacePresenter {
    fn present(&mut self, ctx: &mut PresentContext, src: &wgpu::TextureView, size: (u32, u32), alpha_is_one: bool) {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            _ => {
                self.surface.configure(ctx.device, &self.config);
                return;
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let gamma = self.gamma.view(ctx);
        let p = BlitParams {
            src_rect: Rect::new(0, 0, size.0 as i32, size.1 as i32),
            src_size: size,
            dst_rect: Rect::new(0, 0, self.config.width as i32, self.config.height as i32),
            linear: true,
            filterable: true,
            alpha_one: alpha_is_one,
            gamma: gamma.as_ref(),
        };
        ctx.blit(src, &view, self.config.format, &p);
        self.frame = Some(frame);
    }

    fn after_submit(&mut self, queue: &wgpu::Queue) {
        if let Some(f) = self.frame.take() {
            queue.present(f);
        }
    }

    fn set_gamma_ramp(&mut self, ramp: &[[u16; 256]; 3]) {
        self.gamma.set(ramp);
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// Reads an RGBA8/BGRA8 texture back, blocking (native only).
pub fn read_rgba8(device: &wgpu::Device, queue: &wgpu::Queue, tex: &wgpu::Texture, w: u32, h: u32) -> Vec<u8> {
    let row = (w * 4).next_multiple_of(256);
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("d3dgpu read"),
        size: (row * h) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        tex.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) },
        },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    queue.submit([enc.finish()]);
    buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let data = buf.slice(..).get_mapped_range().map(|v| v.to_vec()).unwrap_or_default();
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h as usize {
        if let Some(r) = data.get(y * row as usize..y * row as usize + w as usize * 4) {
            out.extend_from_slice(r);
        }
    }
    out
}
