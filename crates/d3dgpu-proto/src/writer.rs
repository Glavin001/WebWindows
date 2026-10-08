//! Building batches, with one method per command in Direct3D 9 terms.

use crate::d3d11::*;
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

    // ---- Direct3D 10/11 ----

    fn box3(&mut self, b: Option<&Box3>) {
        match b {
            Some(b) => {
                self.u32(1);
                for v in [b.left, b.top, b.front, b.right, b.bottom, b.back] {
                    self.u32(v);
                }
            }
            None => {
                self.u32(0);
                for _ in 0..6 {
                    self.u32(0);
                }
            }
        }
    }

    /// A buffer: `bind` and `misc` are `D3D11_BIND_*` / `D3D11_RESOURCE_MISC_*`,
    /// `stride` the structure stride of structured buffers.
    pub fn create_buffer11(&mut self, id: Handle, size: u32, bind: u32, misc: u32, stride: u32) {
        self.cmd(Op::CreateBuffer11, |w| {
            for v in [id.0, size, bind, misc, stride] {
                w.u32(v);
            }
        });
    }

    pub fn create_texture11(&mut self, id: Handle, d: &Texture11Desc) {
        self.cmd(Op::CreateTexture11, |w| {
            for v in
                [id.0, d.dim as u32, d.format.0, d.width, d.height, d.depth_or_array, d.mips, d.samples, d.bind, d.misc]
            {
                w.u32(v);
            }
        });
    }

    /// `UpdateSubresource` (subresource = mip + slice * mips; buffers use
    /// 0 and the box's left/right as a byte range).
    pub fn update_subresource<'a>(
        &mut self,
        resource: Handle,
        subresource: u32,
        bx: Option<&Box3>,
        row_pitch: u32,
        depth_pitch: u32,
        data: impl Into<DataSrc<'a>>,
    ) {
        let data = data.into();
        self.cmd(Op::UpdateSubresource, |w| {
            w.u32(resource.0);
            w.u32(subresource);
            w.box3(bx);
            w.u32(row_pitch);
            w.u32(depth_pitch);
            w.data(data);
        });
    }

    pub fn create_view(&mut self, id: Handle, kind: ViewKind, resource: Handle, d: &ViewDesc) {
        self.cmd(Op::CreateView, |w| {
            for v in [
                id.0,
                kind as u32,
                resource.0,
                d.format.0,
                d.dim.0,
                d.first_mip,
                d.mip_count,
                d.first_slice,
                d.slice_count,
                d.first_element,
                d.num_elements,
                d.flags,
            ] {
                w.u32(v);
            }
        });
    }

    pub fn create_sampler(&mut self, id: Handle, d: &SamplerDesc11) {
        self.cmd(Op::CreateSampler, |w| {
            w.u32(id.0);
            w.u32(d.filter);
            for a in d.address {
                w.u32(a);
            }
            w.f32(d.mip_lod_bias);
            w.u32(d.max_anisotropy);
            w.u32(d.comparison);
            for b in d.border {
                w.f32(b);
            }
            w.f32(d.min_lod);
            w.f32(d.max_lod);
        });
    }

    pub fn create_blend_state(&mut self, id: Handle, d: &BlendDesc11) {
        self.cmd(Op::CreateBlendState, |w| {
            w.u32(id.0);
            w.u32(d.alpha_to_coverage as u32);
            w.u32(d.independent as u32);
            for t in &d.targets {
                for v in [t.enable as u32, t.src, t.dst, t.op, t.src_alpha, t.dst_alpha, t.op_alpha, t.write_mask] {
                    w.u32(v);
                }
            }
        });
    }

    pub fn create_depth_stencil_state(&mut self, id: Handle, d: &DepthStencilDesc11) {
        self.cmd(Op::CreateDepthStencilState, |w| {
            for v in [
                id.0,
                d.depth_enable as u32,
                d.depth_write as u32,
                d.depth_func,
                d.stencil_enable as u32,
                d.read_mask,
                d.write_mask,
            ] {
                w.u32(v);
            }
            for f in [d.front, d.back] {
                for v in [f.fail, f.depth_fail, f.pass, f.func] {
                    w.u32(v);
                }
            }
        });
    }

    pub fn create_rasterizer_state(&mut self, id: Handle, d: &RasterizerDesc11) {
        self.cmd(Op::CreateRasterizerState, |w| {
            w.u32(id.0);
            w.u32(d.fill);
            w.u32(d.cull);
            w.u32(d.front_ccw as u32);
            w.i32(d.depth_bias);
            w.f32(d.depth_bias_clamp);
            w.f32(d.slope_scaled_depth_bias);
            w.u32(d.depth_clip as u32);
            w.u32(d.scissor as u32);
            w.u32(d.multisample as u32);
            w.u32(d.antialiased_line as u32);
        });
    }

    pub fn create_input_layout(&mut self, id: Handle, elements: &[InputElement]) {
        self.cmd(Op::CreateInputLayout, |w| {
            w.u32(id.0);
            w.u32(elements.len() as u32);
            for e in elements {
                w.u32(e.semantic.len() as u32);
                w.bytes_padded(e.semantic.as_bytes());
                for v in [e.semantic_index, e.format.0, e.slot, e.offset, e.per_instance as u32, e.step_rate] {
                    w.u32(v);
                }
            }
        });
    }

    /// Registers DXBC bytecode for `stage`.
    pub fn create_shader11<'a>(&mut self, id: Handle, stage: Stage11, hash: u64, dxbc: impl Into<DataSrc<'a>>) {
        let data = dxbc.into();
        self.cmd(Op::CreateShader11, |w| {
            w.u32(id.0);
            w.u32(stage as u32);
            w.u32(hash as u32);
            w.u32((hash >> 32) as u32);
            w.data(data);
        });
    }

    pub fn set_input_layout(&mut self, id: Handle) {
        self.cmd(Op::SetInputLayout, |w| w.u32(id.0));
    }

    pub fn set_vertex_buffers(&mut self, start: u32, buffers: &[VertexBufferBinding]) {
        self.cmd(Op::SetVertexBuffers, |w| {
            w.u32(start);
            w.u32(buffers.len() as u32);
            for b in buffers {
                w.u32(b.buffer.0);
                w.u32(b.stride);
                w.u32(b.offset);
            }
        });
    }

    /// `format` is `R16_UINT` or `R32_UINT`.
    pub fn set_index_buffer(&mut self, buffer: Handle, format: DxgiFormat, offset: u32) {
        self.cmd(Op::SetIndexBuffer, |w| {
            w.u32(buffer.0);
            w.u32(format.0);
            w.u32(offset);
        });
    }

    /// `D3D11_PRIMITIVE_TOPOLOGY`.
    pub fn set_primitive_topology(&mut self, topology: u32) {
        self.cmd(Op::SetPrimitiveTopology, |w| w.u32(topology));
    }

    pub fn set_shader11(&mut self, stage: Stage11, id: Handle) {
        self.cmd(Op::SetShader11, |w| {
            w.u32(stage as u32);
            w.u32(id.0);
        });
    }

    pub fn set_constant_buffers(&mut self, stage: Stage11, start: u32, buffers: &[ConstantBufferBinding]) {
        self.cmd(Op::SetConstantBuffers, |w| {
            w.u32(stage as u32);
            w.u32(start);
            w.u32(buffers.len() as u32);
            for b in buffers {
                w.u32(b.buffer.0);
                w.u32(b.first_constant);
                w.u32(b.num_constants);
            }
        });
    }

    fn handles(&mut self, op: Op, stage: Stage11, start: u32, hs: &[Handle]) {
        self.cmd(op, |w| {
            w.u32(stage as u32);
            w.u32(start);
            w.u32(hs.len() as u32);
            for h in hs {
                w.u32(h.0);
            }
        });
    }

    pub fn set_shader_resources(&mut self, stage: Stage11, start: u32, views: &[Handle]) {
        self.handles(Op::SetShaderResources, stage, start, views);
    }

    pub fn set_samplers(&mut self, stage: Stage11, start: u32, samplers: &[Handle]) {
        self.handles(Op::SetSamplers, stage, start, samplers);
    }

    /// UAVs for the compute stage, or for the pixel stage (`OMSetRenderTargetsAndUnorderedAccessViews`).
    pub fn set_unordered_access_views(&mut self, stage: Stage11, start: u32, views: &[Handle]) {
        self.handles(Op::SetUnorderedAccessViews, stage, start, views);
    }

    pub fn set_render_targets11(&mut self, rtvs: &[Handle], dsv: Handle) {
        self.cmd(Op::SetRenderTargets11, |w| {
            w.u32(rtvs.len() as u32);
            for h in rtvs {
                w.u32(h.0);
            }
            w.u32(dsv.0);
        });
    }

    pub fn set_blend_state(&mut self, id: Handle, factor: [f32; 4], sample_mask: u32) {
        self.cmd(Op::SetBlendState, |w| {
            w.u32(id.0);
            for f in factor {
                w.f32(f);
            }
            w.u32(sample_mask);
        });
    }

    pub fn set_depth_stencil_state(&mut self, id: Handle, stencil_ref: u32) {
        self.cmd(Op::SetDepthStencilState, |w| {
            w.u32(id.0);
            w.u32(stencil_ref);
        });
    }

    pub fn set_rasterizer_state(&mut self, id: Handle) {
        self.cmd(Op::SetRasterizerState, |w| w.u32(id.0));
    }

    pub fn set_viewports(&mut self, viewports: &[Viewport11]) {
        self.cmd(Op::SetViewports, |w| {
            w.u32(viewports.len() as u32);
            for v in viewports {
                for f in [v.x, v.y, v.width, v.height, v.min_depth, v.max_depth] {
                    w.f32(f);
                }
            }
        });
    }

    pub fn set_scissor_rects(&mut self, rects: &[Rect]) {
        self.cmd(Op::SetScissorRects, |w| {
            w.u32(rects.len() as u32);
            for r in rects {
                w.rect(*r);
            }
        });
    }

    pub fn draw11(&mut self, vertex_count: u32, start_vertex: u32, instance_count: u32, start_instance: u32) {
        self.cmd(Op::Draw11, |w| {
            for v in [vertex_count, start_vertex, instance_count, start_instance] {
                w.u32(v);
            }
        });
    }

    pub fn draw_indexed11(
        &mut self,
        index_count: u32,
        start_index: u32,
        base_vertex: i32,
        instance_count: u32,
        start_instance: u32,
    ) {
        self.cmd(Op::DrawIndexed11, |w| {
            w.u32(index_count);
            w.u32(start_index);
            w.i32(base_vertex);
            w.u32(instance_count);
            w.u32(start_instance);
        });
    }

    pub fn dispatch(&mut self, x: u32, y: u32, z: u32) {
        self.cmd(Op::Dispatch, |w| {
            w.u32(x);
            w.u32(y);
            w.u32(z);
        });
    }

    pub fn clear_render_target_view(&mut self, view: Handle, color: [f32; 4]) {
        self.cmd(Op::ClearRenderTargetView, |w| {
            w.u32(view.0);
            for c in color {
                w.f32(c);
            }
        });
    }

    /// `flags`: [`clear11`] bits.
    pub fn clear_depth_stencil_view(&mut self, view: Handle, flags: u32, depth: f32, stencil: u32) {
        self.cmd(Op::ClearDepthStencilView, |w| {
            w.u32(view.0);
            w.u32(flags);
            w.f32(depth);
            w.u32(stencil);
        });
    }

    pub fn clear_unordered_access_view_uint(&mut self, view: Handle, values: [u32; 4]) {
        self.cmd(Op::ClearUnorderedAccessViewUint, |w| {
            w.u32(view.0);
            for v in values {
                w.u32(v);
            }
        });
    }

    pub fn clear_unordered_access_view_float(&mut self, view: Handle, values: [f32; 4]) {
        self.cmd(Op::ClearUnorderedAccessViewFloat, |w| {
            w.u32(view.0);
            for v in values {
                w.f32(v);
            }
        });
    }

    pub fn copy_resource(&mut self, dst: Handle, src: Handle) {
        self.cmd(Op::CopyResource, |w| {
            w.u32(dst.0);
            w.u32(src.0);
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub fn copy_subresource_region(
        &mut self,
        dst: Handle,
        dst_sub: u32,
        x: u32,
        y: u32,
        z: u32,
        src: Handle,
        src_sub: u32,
        bx: Option<&Box3>,
    ) {
        self.cmd(Op::CopySubresourceRegion, |w| {
            for v in [dst.0, dst_sub, x, y, z, src.0, src_sub] {
                w.u32(v);
            }
            w.box3(bx);
        });
    }

    /// Reads a subresource (or a byte range of a buffer: subresource 0 and
    /// the box's left/right) into shared memory, then completes `fence`
    /// (`Map(D3D11_MAP_READ)` on a staging resource).
    #[allow(clippy::too_many_arguments)]
    pub fn read_subresource(
        &mut self,
        resource: Handle,
        subresource: u32,
        bx: Option<&Box3>,
        dest_offset: u32,
        row_pitch: u32,
        depth_pitch: u32,
        fence: u64,
    ) {
        self.cmd(Op::ReadSubresource, |w| {
            w.u32(resource.0);
            w.u32(subresource);
            w.box3(bx);
            w.u32(dest_offset);
            w.u32(row_pitch);
            w.u32(depth_pitch);
            w.u32(fence as u32);
            w.u32((fence >> 32) as u32);
        });
    }
}
