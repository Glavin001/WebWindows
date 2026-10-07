//! GPU objects behind protocol handles, and the per-submission rings.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::Arc;

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
    pub module: Arc<ShaderModule>,
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
/// are ordered after earlier submissions). The CPU copy is allocated once
/// and never cleared: callers write every byte the GPU will read.
pub struct Ring {
    pub buffer: wgpu::Buffer,
    data: Vec<u8>,
    cursor: usize,
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
        Ring { buffer, data: vec![0; size as usize], cursor: 0, size, usage, label }
    }

    /// Reserves `len` bytes at an offset aligned to `align`; `None` when the
    /// ring is full for this submission. The slice holds stale bytes.
    pub fn reserve(&mut self, len: usize, align: u64) -> Option<(u64, &mut [u8])> {
        let offset = (self.cursor as u64).next_multiple_of(align);
        if offset + len as u64 > self.size {
            return None;
        }
        self.cursor = offset as usize + len;
        Some((offset, &mut self.data[offset as usize..offset as usize + len]))
    }

    /// Like [`Ring::reserve`], but the binding window at the returned offset
    /// is `window` bytes, of which only the first `len` are written. Later
    /// allocations may land inside the rest of the window: the shader
    /// bound with it never reads past `len` (uniform blocks whose shaders
    /// read only their first constants).
    pub fn reserve_window(&mut self, len: usize, window: usize, align: u64) -> Option<(u64, &mut [u8])> {
        let offset = (self.cursor as u64).next_multiple_of(align);
        if offset + window.max(len) as u64 > self.size {
            return None;
        }
        self.cursor = offset as usize + len;
        Some((offset, &mut self.data[offset as usize..offset as usize + len]))
    }

    /// Copies `bytes` in at an offset aligned to `align`.
    pub fn alloc(&mut self, bytes: &[u8], align: u64) -> Option<u64> {
        let (offset, dst) = self.reserve(bytes.len(), align)?;
        dst.copy_from_slice(bytes);
        Some(offset)
    }

    pub fn is_empty(&self) -> bool {
        self.cursor == 0
    }

    pub fn flush(&mut self, queue: &wgpu::Queue) {
        if self.cursor != 0 {
            let len = self.cursor.next_multiple_of(4).min(self.data.len());
            queue.write_buffer(&self.buffer, 0, &self.data[..len]);
            self.cursor = 0;
        }
    }

    /// Replaces the buffer with a bigger one (between submissions).
    pub fn grow(&mut self, device: &wgpu::Device, min: u64) {
        self.size = (self.size * 2).max(min.next_power_of_two());
        self.data = vec![0; self.size as usize];
        self.buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(self.label),
            size: self.size,
            usage: self.usage,
            mapped_at_creation: false,
        });
    }
}

/// The Fx hash (as used in rustc): much faster than SipHash for the small
/// fixed keys the caches use, and no external dependency.
#[derive(Default, Clone, Copy)]
pub struct FxHasher(u64);

impl Hasher for FxHasher {
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for c in &mut chunks {
            self.write_u64(u64::from_le_bytes(c.try_into().unwrap()));
        }
        for b in chunks.remainder() {
            self.write_u64(*b as u64);
        }
    }
    fn write_u8(&mut self, i: u8) {
        self.write_u64(i as u64);
    }
    fn write_u32(&mut self, i: u32) {
        self.write_u64(i as u64);
    }
    fn write_u64(&mut self, i: u64) {
        self.0 = (self.0.rotate_left(5) ^ i).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
    fn write_usize(&mut self, i: usize) {
        self.write_u64(i as u64);
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

pub type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<FxHasher>>;
