//! Building batches, with one method per command in Direct3D 9 terms.

use crate::d3d9::*;
use crate::*;

/// Writes one batch. Call the command methods, then [`Writer::finish`].
///
/// ```
/// use d3dgpu_proto::{Writer, Reader, Handle, d3d9::*};
/// let mut w = Writer::new();
/// w.set_render_state(RenderState::ZEnable, 0);
/// w.clear(clear::TARGET, 0xff00ff00, 1.0, 0, &[]);
/// let batch = w.finish();
/// assert_eq!(Reader::new(&batch).unwrap().count(), 2);
/// ```
pub struct Writer {
    buf: Vec<u8>,
    commands: usize,
}

impl Default for Writer {
    fn default() -> Self {
        Self::new()
    }
}

/// A variable-length field to write.
#[derive(Clone, Copy, Debug)]
pub enum DataSrc<'a> {
    Inline(&'a [u8]),
    Shared { offset: u32, len: u32 },
}

impl<'a> From<&'a [u8]> for DataSrc<'a> {
    fn from(b: &'a [u8]) -> Self {
        DataSrc::Inline(b)
    }
}

impl<'a, const N: usize> From<&'a [u8; N]> for DataSrc<'a> {
    fn from(b: &'a [u8; N]) -> Self {
        DataSrc::Inline(b)
    }
}

impl<'a> From<&'a Vec<u8>> for DataSrc<'a> {
    fn from(b: &'a Vec<u8>) -> Self {
        DataSrc::Inline(b)
    }
}

impl Writer {
    pub fn new() -> Writer {
        let mut buf = Vec::with_capacity(4096);
        buf.extend_from_slice(&MAGIC.to_le_bytes());
        buf.extend_from_slice(&VERSION.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        Writer { buf, commands: 0 }
    }

    /// Number of commands written so far.
    pub fn command_count(&self) -> usize {
        self.commands
    }

    /// Bytes written so far, including the header.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commands == 0
    }

    /// Finishes the batch and returns its bytes.
    pub fn finish(mut self) -> Vec<u8> {
        let len = self.buf.len() as u32;
        self.buf[8..12].copy_from_slice(&len.to_le_bytes());
        self.buf
    }

    /// Finishes the batch into `out` and starts a new one, keeping the
    /// allocation.
    pub fn take(&mut self) -> Vec<u8> {
        let next = Writer::new();
        std::mem::replace(self, next).finish()
    }

    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.u32(v as u32);
    }
    fn f32(&mut self, v: f32) {
        self.u32(v.to_bits());
    }
    fn bytes_padded(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
        while !self.buf.len().is_multiple_of(4) {
            self.buf.push(0);
        }
    }
    fn data(&mut self, d: DataSrc) {
        match d {
            DataSrc::Inline(b) => {
                self.u32(DATA_INLINE);
                self.u32(b.len() as u32);
                self.bytes_padded(b);
            }
            DataSrc::Shared { offset, len } => {
                self.u32(DATA_SHARED);
                self.u32(offset);
                self.u32(len);
            }
        }
    }
    fn rect(&mut self, r: Rect) {
        self.i32(r.x1);
        self.i32(r.y1);
        self.i32(r.x2);
        self.i32(r.y2);
    }
    fn region(&mut self, r: &TextureRegion) {
        for v in [r.texture.0, r.face, r.level, r.x, r.y, r.z, r.width, r.height, r.depth] {
            self.u32(v);
        }
    }

    fn cmd(&mut self, op: Op, body: impl FnOnce(&mut Self)) {
        let start = self.buf.len();
        self.u32(op as u32);
        self.u32(0);
        body(self);
        let size = (self.buf.len() - start) as u32;
        self.buf[start + 4..start + 8].copy_from_slice(&size.to_le_bytes());
        self.commands += 1;
    }

    // ---- Objects ----

    pub fn create_buffer(&mut self, id: Handle, size: u32, usage: u32) {
        self.cmd(Op::CreateBuffer, |w| {
            w.u32(id.0);
            w.u32(size);
            w.u32(usage);
        });
    }

    /// Destroys any object.
    pub fn destroy(&mut self, id: Handle) {
        self.cmd(Op::Destroy, |w| w.u32(id.0));
    }

    pub fn write_buffer<'a>(&mut self, id: Handle, offset: u32, data: impl Into<DataSrc<'a>>) {
        let data = data.into();
        self.cmd(Op::WriteBuffer, |w| {
            w.u32(id.0);
            w.u32(offset);
            w.data(data);
        });
    }

    pub fn create_texture(&mut self, id: Handle, desc: &TextureDesc) {
        self.cmd(Op::CreateTexture, |w| {
            w.u32(id.0);
            w.u32(desc.kind as u32);
            w.u32(desc.format.0);
            w.u32(desc.width);
            w.u32(desc.height);
            w.u32(desc.depth);
            w.u32(desc.levels);
            w.u32(desc.usage);
        });
    }

    /// Uploads texels in the texture's Direct3D format; the core converts.
    /// `row_pitch` is bytes per row of blocks, `slice_pitch` bytes per
    /// depth slice (volume textures).
    pub fn write_texture<'a>(
        &mut self,
        region: &TextureRegion,
        row_pitch: u32,
        slice_pitch: u32,
        data: impl Into<DataSrc<'a>>,
    ) {
        let data = data.into();
        self.cmd(Op::WriteTexture, |w| {
            w.region(region);
            w.u32(row_pitch);
            w.u32(slice_pitch);
            w.data(data);
        });
    }

    /// Registers shader bytecode (SM1–3 token stream) under `id`. `hash`
    /// identifies the bytecode for caches.
    pub fn create_shader<'a>(&mut self, id: Handle, stage: Stage, hash: u64, bytecode: impl Into<DataSrc<'a>>) {
        let data = bytecode.into();
        self.cmd(Op::CreateShader, |w| {
            w.u32(id.0);
            w.u32(stage as u32);
            w.u32(hash as u32);
            w.u32((hash >> 32) as u32);
            w.data(data);
        });
    }

    pub fn create_vertex_decl(&mut self, id: Handle, elements: &[VertexElement]) {
        self.cmd(Op::CreateVertexDecl, |w| {
            w.u32(id.0);
            w.u32(elements.len() as u32);
            for e in elements {
                w.buf.extend_from_slice(&e.stream.to_le_bytes());
                w.buf.extend_from_slice(&e.offset.to_le_bytes());
                w.buf.extend_from_slice(&[e.ty.0, e.method, e.usage.0, e.usage_index]);
            }
        });
    }

    /// Sets palette `index` (for P8 textures). Entries are
    /// `PALETTEENTRY` bytes: red, green, blue, flags (alpha).
    pub fn set_palette(&mut self, index: u32, entries: &[[u8; 4]; 256]) {
        self.cmd(Op::SetPalette, |w| {
            w.u32(index);
            for e in entries {
                w.buf.extend_from_slice(e);
            }
        });
    }

    // ---- State ----

    pub fn set_render_target(&mut self, index: u32, texture: Handle, face: u32, level: u32) {
        self.cmd(Op::SetRenderTarget, |w| {
            w.u32(index);
            w.u32(texture.0);
            w.u32(face);
            w.u32(level);
        });
    }

    pub fn set_depth_stencil(&mut self, texture: Handle, face: u32, level: u32) {
        self.cmd(Op::SetDepthStencil, |w| {
            w.u32(texture.0);
            w.u32(face);
            w.u32(level);
        });
    }

    pub fn set_viewport(&mut self, vp: &Viewport) {
        self.cmd(Op::SetViewport, |w| {
            w.u32(vp.x);
            w.u32(vp.y);
            w.u32(vp.width);
            w.u32(vp.height);
            w.f32(vp.min_z);
            w.f32(vp.max_z);
        });
    }

    pub fn set_scissor(&mut self, r: Rect) {
        self.cmd(Op::SetScissor, |w| w.rect(r));
    }

    pub fn set_render_state(&mut self, state: RenderState, value: u32) {
        self.cmd(Op::SetRenderState, |w| {
            w.u32(state.0);
            w.u32(value);
        });
    }

    /// `sampler` is 0..16 for pixel shaders and 16..20 for vertex texture
    /// fetch ([`VERTEX_SAMPLER_BASE`]).
    pub fn set_sampler_state(&mut self, sampler: u32, state: SamplerState, value: u32) {
        self.cmd(Op::SetSamplerState, |w| {
            w.u32(sampler);
            w.u32(state.0);
            w.u32(value);
        });
    }

    pub fn set_texture(&mut self, sampler: u32, texture: Handle) {
        self.cmd(Op::SetTexture, |w| {
            w.u32(sampler);
            w.u32(texture.0);
        });
    }

    pub fn set_texture_stage_state(&mut self, stage: u32, state: TextureStageState, value: u32) {
        self.cmd(Op::SetTextureStageState, |w| {
            w.u32(stage);
            w.u32(state.0);
            w.u32(value);
        });
    }

    pub fn set_vertex_shader(&mut self, id: Handle) {
        self.cmd(Op::SetVertexShader, |w| w.u32(id.0));
    }

    pub fn set_pixel_shader(&mut self, id: Handle) {
        self.cmd(Op::SetPixelShader, |w| w.u32(id.0));
    }

    pub fn set_vertex_decl(&mut self, id: Handle) {
        self.cmd(Op::SetVertexDecl, |w| w.u32(id.0));
    }

    pub fn set_stream_source(&mut self, stream: u32, buffer: Handle, offset: u32, stride: u32) {
        self.cmd(Op::SetStreamSource, |w| {
            w.u32(stream);
            w.u32(buffer.0);
            w.u32(offset);
            w.u32(stride);
        });
    }

    /// `D3DSTREAMSOURCE_INDEXEDDATA | n` / `INSTANCEDATA | n` values, as
    /// Direct3D 9 passes them.
    pub fn set_stream_freq(&mut self, stream: u32, value: u32) {
        self.cmd(Op::SetStreamFreq, |w| {
            w.u32(stream);
            w.u32(value);
        });
    }

    /// `format` is [`Format::Index16`] or [`Format::Index32`].
    pub fn set_indices(&mut self, buffer: Handle, format: Format) {
        self.cmd(Op::SetIndices, |w| {
            w.u32(buffer.0);
            w.u32(format.0);
        });
    }

    pub fn set_shader_const_f(&mut self, stage: Stage, start: u32, data: &[[f32; 4]]) {
        self.cmd(Op::SetShaderConstF, |w| {
            w.u32(stage as u32);
            w.u32(start);
            w.u32(data.len() as u32);
            for v in data.iter().flatten() {
                w.f32(*v);
            }
        });
    }

    pub fn set_shader_const_i(&mut self, stage: Stage, start: u32, data: &[[i32; 4]]) {
        self.cmd(Op::SetShaderConstI, |w| {
            w.u32(stage as u32);
            w.u32(start);
            w.u32(data.len() as u32);
            for v in data.iter().flatten() {
                w.i32(*v);
            }
        });
    }

    pub fn set_shader_const_b(&mut self, stage: Stage, start: u32, data: &[bool]) {
        self.cmd(Op::SetShaderConstB, |w| {
            w.u32(stage as u32);
            w.u32(start);
            w.u32(data.len() as u32);
            for v in data {
                w.u32(*v as u32);
            }
        });
    }

    /// A user clip plane in clip space (as Direct3D 9 takes them when a
    /// vertex shader is bound).
    pub fn set_clip_plane(&mut self, index: u32, plane: [f32; 4]) {
        self.cmd(Op::SetClipPlane, |w| {
            w.u32(index);
            for v in plane {
                w.f32(v);
            }
        });
    }

    // ---- Drawing ----

    /// `color` is `D3DCOLOR` (0xAARRGGBB). An empty `rects` clears the
    /// whole viewport (intersected with the scissor when enabled), as
    /// `IDirect3DDevice9::Clear` does.
    pub fn clear(&mut self, flags: u32, color: u32, z: f32, stencil: u32, rects: &[Rect]) {
        self.cmd(Op::Clear, |w| {
            w.u32(flags);
            w.u32(color);
            w.f32(z);
            w.u32(stencil);
            w.u32(rects.len() as u32);
            for r in rects {
                w.rect(*r);
            }
        });
    }

    pub fn draw(&mut self, prim: PrimitiveType, start_vertex: u32, prim_count: u32) {
        self.cmd(Op::Draw, |w| {
            w.u32(prim.0);
            w.u32(start_vertex);
            w.u32(prim_count);
        });
    }

    pub fn draw_indexed(&mut self, prim: PrimitiveType, d: &IndexedDraw) {
        self.cmd(Op::DrawIndexed, |w| {
            w.u32(prim.0);
            w.i32(d.base_vertex);
            w.u32(d.min_index);
            w.u32(d.num_vertices);
            w.u32(d.start_index);
            w.u32(d.prim_count);
        });
    }

    /// `DrawPrimitiveUP`: vertices for stream 0 travel with the command.
    pub fn draw_up<'a>(&mut self, prim: PrimitiveType, prim_count: u32, stride: u32, vertices: impl Into<DataSrc<'a>>) {
        let data = vertices.into();
        self.cmd(Op::DrawUp, |w| {
            w.u32(prim.0);
            w.u32(prim_count);
            w.u32(stride);
            w.data(data);
        });
    }

    /// `DrawIndexedPrimitiveUP`.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_indexed_up<'a, 'b>(
        &mut self,
        prim: PrimitiveType,
        min_index: u32,
        num_vertices: u32,
        prim_count: u32,
        index_format: Format,
        indices: impl Into<DataSrc<'a>>,
        stride: u32,
        vertices: impl Into<DataSrc<'b>>,
    ) {
        let (i, v) = (indices.into(), vertices.into());
        self.cmd(Op::DrawIndexedUp, |w| {
            w.u32(prim.0);
            w.u32(min_index);
            w.u32(num_vertices);
            w.u32(prim_count);
            w.u32(index_format.0);
            w.u32(stride);
            w.data(i);
            w.data(v);
        });
    }

    /// `StretchRect`. `filter` is a [`TextureFilter`] value.
    #[allow(clippy::too_many_arguments)]
    pub fn stretch_rect(
        &mut self,
        src: Handle,
        src_face: u32,
        src_level: u32,
        src_rect: Rect,
        dst: Handle,
        dst_face: u32,
        dst_level: u32,
        dst_rect: Rect,
        filter: TextureFilter,
    ) {
        self.cmd(Op::StretchRect, |w| {
            w.u32(src.0);
            w.u32(src_face);
            w.u32(src_level);
            w.rect(src_rect);
            w.u32(dst.0);
            w.u32(dst_face);
            w.u32(dst_level);
            w.rect(dst_rect);
            w.u32(filter.0);
        });
    }

    // ---- Frames and synchronisation ----

    /// Shows `texture` (level 0) in output `window` (a top-level window;
    /// the core maps it to a canvas or native surface). [`present`] flags.
    pub fn present(&mut self, texture: Handle, window: u32, flags: u32) {
        self.cmd(Op::Present, |w| {
            w.u32(texture.0);
            w.u32(window);
            w.u32(flags);
        });
    }

    /// The gamma ramp applied when presenting to `window`
    /// (`D3DGAMMARAMP`: 256 red, 256 green, 256 blue 16-bit entries).
    pub fn set_gamma_ramp(&mut self, window: u32, ramp: &[[u16; 256]; 3]) {
        self.cmd(Op::SetGammaRamp, |w| {
            w.u32(window);
            for v in ramp.iter().flatten() {
                w.buf.extend_from_slice(&v.to_le_bytes());
            }
        });
    }

    /// Copies a region of a texture, in its Direct3D format, to shared
    /// memory at `dest_offset`, then completes `fence` (as [`Signal`]
    /// would). The front end waits for the fence before reading.
    ///
    /// [`Signal`]: Op::Signal
    pub fn read_texture(
        &mut self,
        region: &TextureRegion,
        dest_offset: u32,
        row_pitch: u32,
        slice_pitch: u32,
        fence: u64,
    ) {
        self.cmd(Op::ReadTexture, |w| {
            w.region(region);
            w.u32(dest_offset);
            w.u32(row_pitch);
            w.u32(slice_pitch);
            w.u32(fence as u32);
            w.u32((fence >> 32) as u32);
        });
    }

    /// Marks `fence` complete once the GPU has finished everything before
    /// it. Fences increase monotonically.
    pub fn signal(&mut self, fence: u64) {
        self.cmd(Op::Signal, |w| {
            w.u32(fence as u32);
            w.u32((fence >> 32) as u32);
        });
    }

    /// A debug label (shows up in traces and GPU captures).
    pub fn marker(&mut self, text: &str) {
        self.cmd(Op::Marker, |w| w.data(DataSrc::Inline(text.as_bytes())));
    }
}
