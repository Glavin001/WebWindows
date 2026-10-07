//! GPU objects behind protocol handles, and the per-submission rings.

use std::collections::HashMap;

use d3dgpu_emu::format::FormatPlan;
use d3dgpu_proto::d3d9::VertexElement;
use d3dgpu_proto::{TextureDesc, TextureKind};
use d3dgpu_shader::ShaderModule;

pub struct Buffer {
    pub gpu: wgpu::Buffer,
    pub size: u32,
    /// CPU copy: index rewrites (fans, wireframe) and repacking read it.
    pub shadow: Vec<u8>,
    /// Submission epoch this buffer was last read in by recorded commands.
    pub last_use: u64,
}

pub struct Texture {
    pub gpu: wgpu::Texture,
    pub desc: TextureDesc,
    pub plan: FormatPlan,
    pub format: wgpu::TextureFormat,
    pub levels: u32,
    /// The view samplers read (all levels, cube or 3D as created).
    pub view: wgpu::TextureView,
    pub view_id: u64,
    /// sRGB-decoding view for `D3DSAMP_SRGBTEXTURE`, where the format has one.
    pub srgb_view: Option<(wgpu::TextureView, u64)>,
    /// Single-subresource views for rendering, by (face, level).
    pub rt_views: HashMap<(u32, u32), wgpu::TextureView>,
    pub last_use: u64,
}

impl Texture {
    pub fn level_size(&self, level: u32) -> (u32, u32) {
        ((self.desc.width >> level).max(1), (self.desc.height >> level).max(1))
    }

    pub fn rt_view(&mut self, face: u32, level: u32) -> wgpu::TextureView {
        let (gpu, format) = (&self.gpu, self.format);
        let kind = self.desc.kind;
        self.rt_views
            .entry((face, level))
            .or_insert_with(|| {
                gpu.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("d3dgpu rt view"),
                    format: Some(format),
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    usage: None,
                    aspect: wgpu::TextureAspect::All,
                    base_mip_level: level,
                    mip_level_count: Some(1),
                    base_array_layer: if kind == TextureKind::Cube { face } else { 0 },
                    array_layer_count: Some(1),
                })
            })
            .clone()
    }

    pub fn is_depth(&self) -> bool {
        self.format.is_depth_stencil_format()
    }
}

pub struct Shader {
    pub module: ShaderModule,
    pub hash: u64,
}

#[allow(clippy::large_enum_variant)] // few objects; textures dominate anyway
pub enum Object {
    Buffer(Buffer),
    Texture(Texture),
    Shader(Box<Shader>),
    VertexDecl(Vec<VertexElement>),
}

/// A buffer filled from the CPU once per submission: everything allocated
/// during a submission lands at its own offset and is written with one
/// `write_buffer` just before the submit, so it is visible to every command
/// in it and the next submission can start again at offset 0 (queue writes
/// are ordered after earlier submissions).
pub struct Ring {
    pub buffer: wgpu::Buffer,
    pub data: Vec<u8>,
    pub size: u64,
    usage: wgpu::BufferUsages,
    label: &'static str,
}

impl Ring {
    pub fn new(device: &wgpu::Device, label: &'static str, size: u64, usage: wgpu::BufferUsages) -> Ring {
        let usage = usage | wgpu::BufferUsages::COPY_DST;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size,
            usage,
            mapped_at_creation: false,
        });
        Ring { buffer, data: Vec::with_capacity(size as usize), size, usage, label }
    }

    /// Reserves `bytes` at an offset aligned to `align`; `None` when the
    /// ring is full for this submission.
    pub fn alloc(&mut self, bytes: &[u8], align: u64) -> Option<u64> {
        let offset = (self.data.len() as u64).next_multiple_of(align);
        if offset + bytes.len() as u64 > self.size {
            return None;
        }
        self.data.resize(offset as usize, 0);
        self.data.extend_from_slice(bytes);
        Some(offset)
    }

    /// Reserves `len` zeroed bytes and returns the offset and the slice.
    pub fn alloc_zeroed(&mut self, len: usize, align: u64) -> Option<(u64, &mut [u8])> {
        let offset = (self.data.len() as u64).next_multiple_of(align);
        if offset + len as u64 > self.size {
            return None;
        }
        self.data.resize(offset as usize + len, 0);
        Some((offset, &mut self.data[offset as usize..]))
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn flush(&mut self, queue: &wgpu::Queue) {
        if !self.data.is_empty() {
            let len = self.data.len().next_multiple_of(4);
            self.data.resize(len, 0);
            queue.write_buffer(&self.buffer, 0, &self.data);
            self.data.clear();
        }
    }

    /// Replaces the buffer with a bigger one (between submissions).
    pub fn grow(&mut self, device: &wgpu::Device, min: u64) {
        self.size = (self.size * 2).max(min.next_power_of_two());
        self.buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(self.label),
            size: self.size,
            usage: self.usage,
            mapped_at_creation: false,
        });
    }
}
