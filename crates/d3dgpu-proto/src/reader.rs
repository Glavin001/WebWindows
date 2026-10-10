//! Decoding batches without copying.

use crate::d3d11::*;
use crate::d3d9::*;
use crate::*;

/// One decoded command. Variable-length fields borrow from the batch.
#[derive(Clone, Debug, PartialEq)]
pub enum Command<'a> {
    CreateBuffer {
        id: Handle,
        size: u32,
        usage: u32,
    },
    Destroy {
        id: Handle,
    },
    WriteBuffer {
        id: Handle,
        offset: u32,
        data: Data<'a>,
        /// Written by WriteBufferNoOverwrite.
        no_overwrite: bool,
    },
    CreateTexture {
        id: Handle,
        desc: TextureDesc,
    },
    WriteTexture {
        region: TextureRegion,
        row_pitch: u32,
        slice_pitch: u32,
        data: Data<'a>,
    },
    CreateShader {
        id: Handle,
        stage: Stage,
        hash: u64,
        bytecode: Data<'a>,
    },
    CreateVertexDecl {
        id: Handle,
        elements: Vec<VertexElement>,
    },
    /// 256 entries of red, green, blue, flags.
    SetPalette {
        index: u32,
        entries: &'a [u8],
    },

    SetRenderTarget {
        index: u32,
        texture: Handle,
        face: u32,
        level: u32,
    },
    SetDepthStencil {
        texture: Handle,
        face: u32,
        level: u32,
    },
    SetViewport(Viewport),
    SetScissor(Rect),
    SetRenderState {
        state: RenderState,
        value: u32,
    },
    SetSamplerState {
        sampler: u32,
        state: SamplerState,
        value: u32,
    },
    SetTexture {
        sampler: u32,
        texture: Handle,
    },
    SetTextureStageState {
        stage: u32,
        state: TextureStageState,
        value: u32,
    },
    SetVertexShader(Handle),
    SetPixelShader(Handle),
    SetVertexDecl(Handle),
    SetStreamSource {
        stream: u32,
        buffer: Handle,
        offset: u32,
        stride: u32,
    },
    SetStreamFreq {
        stream: u32,
        value: u32,
    },
    SetIndices {
        buffer: Handle,
        format: Format,
    },
    /// `count` vec4s of little-endian f32.
    SetShaderConstF {
        stage: Stage,
        start: u32,
        count: u32,
        data: &'a [u8],
    },
    /// `count` vec4s of little-endian i32.
    SetShaderConstI {
        stage: Stage,
        start: u32,
        count: u32,
        data: &'a [u8],
    },
    /// `count` u32 booleans.
    SetShaderConstB {
        stage: Stage,
        start: u32,
        count: u32,
        data: &'a [u8],
    },
    SetClipPlane {
        index: u32,
        plane: [f32; 4],
    },

    Clear {
        flags: u32,
        color: u32,
        z: f32,
        stencil: u32,
        rects: Vec<Rect>,
    },
    Draw {
        prim: PrimitiveType,
        start_vertex: u32,
        prim_count: u32,
    },
    DrawIndexed {
        prim: PrimitiveType,
        draw: IndexedDraw,
    },
    DrawUp {
        prim: PrimitiveType,
        prim_count: u32,
        stride: u32,
        vertices: Data<'a>,
    },
    DrawIndexedUp {
        prim: PrimitiveType,
        min_index: u32,
        num_vertices: u32,
        prim_count: u32,
        index_format: Format,
        stride: u32,
        indices: Data<'a>,
        vertices: Data<'a>,
    },
    StretchRect {
        src: Handle,
        src_face: u32,
        src_level: u32,
        src_rect: Rect,
        dst: Handle,
        dst_face: u32,
        dst_level: u32,
        dst_rect: Rect,
        filter: TextureFilter,
    },

    Present {
        texture: Handle,
        window: u32,
        flags: u32,
    },
    /// 768 little-endian u16: red, green, blue ramps.
    SetGammaRamp {
        window: u32,
        ramp: &'a [u8],
    },
    ReadTexture {
        region: TextureRegion,
        dest_offset: u32,
        row_pitch: u32,
        slice_pitch: u32,
        fence: u64,
    },
    Signal {
        fence: u64,
    },
    Marker(&'a str),

    // Direct3D 10/11.
    CreateBuffer11 {
        id: Handle,
        size: u32,
        bind: u32,
        misc: u32,
        stride: u32,
    },
    CreateTexture11 {
        id: Handle,
        desc: Texture11Desc,
    },
    UpdateSubresource {
        resource: Handle,
        subresource: u32,
        bx: Option<Box3>,
        row_pitch: u32,
        depth_pitch: u32,
        data: Data<'a>,
    },
    CreateView {
        id: Handle,
        kind: ViewKind,
        resource: Handle,
        desc: ViewDesc,
    },
    CreateSampler {
        id: Handle,
        desc: SamplerDesc11,
    },
    CreateBlendState {
        id: Handle,
        desc: Box<BlendDesc11>,
    },
    CreateDepthStencilState {
        id: Handle,
        desc: DepthStencilDesc11,
    },
    CreateRasterizerState {
        id: Handle,
        desc: RasterizerDesc11,
    },
    CreateInputLayout {
        id: Handle,
        elements: Vec<InputElement>,
    },
    CreateShader11 {
        id: Handle,
        stage: Stage11,
        hash: u64,
        dxbc: Data<'a>,
    },
    SetInputLayout(Handle),
    SetVertexBuffers {
        start: u32,
        buffers: Vec<VertexBufferBinding>,
    },
    SetIndexBuffer {
        buffer: Handle,
        format: DxgiFormat,
        offset: u32,
    },
    SetPrimitiveTopology(u32),
    SetShader11 {
        stage: Stage11,
        id: Handle,
    },
    SetConstantBuffers {
        stage: Stage11,
        start: u32,
        buffers: Vec<ConstantBufferBinding>,
    },
    SetShaderResources {
        stage: Stage11,
        start: u32,
        views: Vec<Handle>,
    },
    SetSamplers {
        stage: Stage11,
        start: u32,
        samplers: Vec<Handle>,
    },
    SetUnorderedAccessViews {
        stage: Stage11,
        start: u32,
        views: Vec<Handle>,
    },
    SetRenderTargets11 {
        rtvs: Vec<Handle>,
        dsv: Handle,
    },
    SetBlendState {
        id: Handle,
        factor: [f32; 4],
        sample_mask: u32,
    },
    SetDepthStencilState {
        id: Handle,
        stencil_ref: u32,
    },
    SetRasterizerState(Handle),
    SetViewports(Vec<Viewport11>),
    SetScissorRects(Vec<Rect>),
    Draw11 {
        vertex_count: u32,
        start_vertex: u32,
        instance_count: u32,
        start_instance: u32,
    },
    DrawIndexed11 {
        index_count: u32,
        start_index: u32,
        base_vertex: i32,
        instance_count: u32,
        start_instance: u32,
    },
    Dispatch {
        x: u32,
        y: u32,
        z: u32,
    },
    ClearRenderTargetView {
        view: Handle,
        color: [f32; 4],
    },
    ClearDepthStencilView {
        view: Handle,
        flags: u32,
        depth: f32,
        stencil: u32,
    },
    ClearUnorderedAccessViewUint {
        view: Handle,
        values: [u32; 4],
    },
    ClearUnorderedAccessViewFloat {
        view: Handle,
        values: [f32; 4],
    },
    CopyResource {
        dst: Handle,
        src: Handle,
    },
    CopySubresourceRegion {
        dst: Handle,
        dst_sub: u32,
        x: u32,
        y: u32,
        z: u32,
        src: Handle,
        src_sub: u32,
        bx: Option<Box3>,
    },
    ReadSubresource {
        resource: Handle,
        subresource: u32,
        bx: Option<Box3>,
        dest_offset: u32,
        row_pitch: u32,
        depth_pitch: u32,
        fence: u64,
    },
}

/// Why a batch could not be decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    BadMagic(u32),
    UnsupportedVersion(u32),
    /// The header's length disagrees with the buffer.
    BadLength,
    /// A command at `offset` is malformed.
    Malformed {
        offset: usize,
        op: u32,
    },
    UnknownOp {
        offset: usize,
        op: u32,
    },
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::BadMagic(m) => write!(f, "bad batch magic {m:#010x}"),
            DecodeError::UnsupportedVersion(v) => write!(f, "unsupported protocol version {v}"),
            DecodeError::BadLength => write!(f, "batch length does not match"),
            DecodeError::Malformed { offset, op } => write!(f, "malformed command {op:#x} at offset {offset}"),
            DecodeError::UnknownOp { offset, op } => write!(f, "unknown command {op:#x} at offset {offset}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Iterates over the commands of a batch.
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    failed: bool,
}

impl<'a> Reader<'a> {
    /// Checks the header. The buffer may be longer than the batch.
    pub fn new(buf: &'a [u8]) -> Result<Reader<'a>, DecodeError> {
        let h = Self::header(buf)?;
        Ok(Reader { buf: &buf[..h.len as usize], pos: BATCH_HEADER_SIZE, failed: false })
    }

    pub fn header(buf: &[u8]) -> Result<BatchHeader, DecodeError> {
        if buf.len() < BATCH_HEADER_SIZE {
            return Err(DecodeError::BadLength);
        }
        let w = |i: usize| u32::from_le_bytes(buf[i..i + 4].try_into().unwrap());
        let h = BatchHeader { magic: w(0), version: w(4), len: w(8) };
        if h.magic != MAGIC {
            return Err(DecodeError::BadMagic(h.magic));
        }
        if h.version != VERSION {
            return Err(DecodeError::UnsupportedVersion(h.version));
        }
        if (h.len as usize) < BATCH_HEADER_SIZE || h.len as usize > buf.len() || !h.len.is_multiple_of(4) {
            return Err(DecodeError::BadLength);
        }
        Ok(h)
    }

    /// Offset of the next command in the batch.
    pub fn position(&self) -> usize {
        self.pos
    }
}

impl<'a> Iterator for Reader<'a> {
    type Item = Result<Command<'a>, DecodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.pos >= self.buf.len() {
            return None;
        }
        let offset = self.pos;
        let result = decode_at(self.buf, offset);
        match &result {
            Ok((_, size)) => self.pos += size,
            Err(_) => self.failed = true,
        }
        Some(result.map(|(c, _)| c))
    }
}

struct Cursor<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Cursor<'a> {
    fn u32(&mut self) -> Option<u32> {
        let v = u32::from_le_bytes(self.b.get(self.p..self.p + 4)?.try_into().ok()?);
        self.p += 4;
        Some(v)
    }
    fn i32(&mut self) -> Option<i32> {
        self.u32().map(|v| v as i32)
    }
    fn f32(&mut self) -> Option<f32> {
        self.u32().map(f32::from_bits)
    }
    fn h(&mut self) -> Option<Handle> {
        self.u32().map(Handle)
    }
    fn u64(&mut self) -> Option<u64> {
        let lo = self.u32()? as u64;
        let hi = self.u32()? as u64;
        Some(lo | hi << 32)
    }
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.p..self.p.checked_add(n)?)?;
        self.p += n.div_ceil(4) * 4;
        Some(s)
    }
    fn data(&mut self) -> Option<Data<'a>> {
        match self.u32()? {
            DATA_INLINE => {
                let n = self.u32()? as usize;
                Some(Data::Inline(self.bytes(n)?))
            }
            DATA_SHARED => {
                let offset = self.u32()?;
                let len = self.u32()?;
                offset.checked_add(len)?;
                Some(Data::Shared { offset, len })
            }
            _ => None,
        }
    }
    fn rect(&mut self) -> Option<Rect> {
        Some(Rect { x1: self.i32()?, y1: self.i32()?, x2: self.i32()?, y2: self.i32()? })
    }
    fn box3(&mut self) -> Option<Option<Box3>> {
        let has = self.u32()?;
        let b = Box3 {
            left: self.u32()?,
            top: self.u32()?,
            front: self.u32()?,
            right: self.u32()?,
            bottom: self.u32()?,
            back: self.u32()?,
        };
        Some((has != 0).then_some(b))
    }
    fn f4(&mut self) -> Option<[f32; 4]> {
        Some([self.f32()?, self.f32()?, self.f32()?, self.f32()?])
    }
    fn stage(&mut self) -> Option<Stage11> {
        Stage11::from_u32(self.u32()?)
    }
    fn handles(&mut self) -> Option<(Stage11, u32, Vec<Handle>)> {
        let stage = self.stage()?;
        let start = self.u32()?;
        let n = self.u32()?;
        if n > 128 {
            return None;
        }
        Some((stage, start, (0..n).map(|_| self.h()).collect::<Option<_>>()?))
    }
    fn region(&mut self) -> Option<TextureRegion> {
        Some(TextureRegion {
            texture: self.h()?,
            face: self.u32()?,
            level: self.u32()?,
            x: self.u32()?,
            y: self.u32()?,
            z: self.u32()?,
            width: self.u32()?,
            height: self.u32()?,
            depth: self.u32()?,
        })
    }
}

fn decode_at(buf: &[u8], offset: usize) -> Result<(Command<'_>, usize), DecodeError> {
    let word = |i: usize| buf.get(i..i + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let op_raw = word(offset).ok_or(DecodeError::Malformed { offset, op: 0 })?;
    let malformed = DecodeError::Malformed { offset, op: op_raw };
    let size = word(offset + 4).ok_or(malformed.clone())? as usize;
    if size < COMMAND_HEADER_SIZE || !size.is_multiple_of(4) || offset + size > buf.len() {
        return Err(malformed);
    }
    let op = Op::from_u32(op_raw).ok_or(DecodeError::UnknownOp { offset, op: op_raw })?;
    let mut c = Cursor { b: &buf[offset + COMMAND_HEADER_SIZE..offset + size], p: 0 };
    let cmd = decode_body(op, &mut c).ok_or(malformed.clone())?;
    Ok((cmd, size))
}

fn decode_body<'a>(op: Op, c: &mut Cursor<'a>) -> Option<Command<'a>> {
    Some(match op {
        Op::CreateBuffer => Command::CreateBuffer { id: c.h()?, size: c.u32()?, usage: c.u32()? },
        Op::Destroy => Command::Destroy { id: c.h()? },
        Op::WriteBuffer | Op::WriteBufferNoOverwrite => Command::WriteBuffer {
            id: c.h()?,
            offset: c.u32()?,
            data: c.data()?,
            no_overwrite: op == Op::WriteBufferNoOverwrite,
        },
        Op::CreateTexture => {
            let id = c.h()?;
            let kind = TextureKind::from_u32(c.u32()?)?;
            Command::CreateTexture {
                id,
                desc: TextureDesc {
                    kind,
                    format: Format(c.u32()?),
                    width: c.u32()?,
                    height: c.u32()?,
                    depth: c.u32()?,
                    levels: c.u32()?,
                    usage: c.u32()?,
                },
            }
        }
        Op::WriteTexture => {
            Command::WriteTexture { region: c.region()?, row_pitch: c.u32()?, slice_pitch: c.u32()?, data: c.data()? }
        }
        Op::CreateShader => {
            Command::CreateShader { id: c.h()?, stage: Stage::from_u32(c.u32()?)?, hash: c.u64()?, bytecode: c.data()? }
        }
        Op::CreateVertexDecl => {
            let id = c.h()?;
            let n = c.u32()? as usize;
            let raw = c.bytes(n.checked_mul(8)?)?;
            let elements = raw
                .as_chunks::<8>()
                .0
                .iter()
                .map(|e| VertexElement {
                    stream: u16::from_le_bytes([e[0], e[1]]),
                    offset: u16::from_le_bytes([e[2], e[3]]),
                    ty: DeclType(e[4]),
                    method: e[5],
                    usage: DeclUsage(e[6]),
                    usage_index: e[7],
                })
                .collect();
            Command::CreateVertexDecl { id, elements }
        }
        Op::SetPalette => Command::SetPalette { index: c.u32()?, entries: c.bytes(1024)? },
        Op::SetRenderTarget => {
            Command::SetRenderTarget { index: c.u32()?, texture: c.h()?, face: c.u32()?, level: c.u32()? }
        }
        Op::SetDepthStencil => Command::SetDepthStencil { texture: c.h()?, face: c.u32()?, level: c.u32()? },
        Op::SetViewport => Command::SetViewport(Viewport {
            x: c.u32()?,
            y: c.u32()?,
            width: c.u32()?,
            height: c.u32()?,
            min_z: c.f32()?,
            max_z: c.f32()?,
        }),
        Op::SetScissor => Command::SetScissor(c.rect()?),
        Op::SetRenderState => Command::SetRenderState { state: RenderState(c.u32()?), value: c.u32()? },
        Op::SetSamplerState => {
            Command::SetSamplerState { sampler: c.u32()?, state: SamplerState(c.u32()?), value: c.u32()? }
        }
        Op::SetTexture => Command::SetTexture { sampler: c.u32()?, texture: c.h()? },
        Op::SetTextureStageState => {
            Command::SetTextureStageState { stage: c.u32()?, state: TextureStageState(c.u32()?), value: c.u32()? }
        }
        Op::SetVertexShader => Command::SetVertexShader(c.h()?),
        Op::SetPixelShader => Command::SetPixelShader(c.h()?),
        Op::SetVertexDecl => Command::SetVertexDecl(c.h()?),
        Op::SetStreamSource => {
            Command::SetStreamSource { stream: c.u32()?, buffer: c.h()?, offset: c.u32()?, stride: c.u32()? }
        }
        Op::SetStreamFreq => Command::SetStreamFreq { stream: c.u32()?, value: c.u32()? },
        Op::SetIndices => Command::SetIndices { buffer: c.h()?, format: Format(c.u32()?) },
        Op::SetShaderConstF | Op::SetShaderConstI | Op::SetShaderConstB => {
            let stage = Stage::from_u32(c.u32()?)?;
            let start = c.u32()?;
            let count = c.u32()?;
            let words = if op == Op::SetShaderConstB { count } else { count.checked_mul(4)? };
            let data = c.bytes(words.checked_mul(4)? as usize)?;
            match op {
                Op::SetShaderConstF => Command::SetShaderConstF { stage, start, count, data },
                Op::SetShaderConstI => Command::SetShaderConstI { stage, start, count, data },
                _ => Command::SetShaderConstB { stage, start, count, data },
            }
        }
        Op::SetClipPlane => Command::SetClipPlane { index: c.u32()?, plane: [c.f32()?, c.f32()?, c.f32()?, c.f32()?] },
        Op::Clear => {
            let flags = c.u32()?;
            let color = c.u32()?;
            let z = c.f32()?;
            let stencil = c.u32()?;
            let n = c.u32()?;
            let rects = (0..n).map(|_| c.rect()).collect::<Option<Vec<_>>>()?;
            Command::Clear { flags, color, z, stencil, rects }
        }
        Op::Draw => Command::Draw { prim: PrimitiveType(c.u32()?), start_vertex: c.u32()?, prim_count: c.u32()? },
        Op::DrawIndexed => Command::DrawIndexed {
            prim: PrimitiveType(c.u32()?),
            draw: IndexedDraw {
                base_vertex: c.i32()?,
                min_index: c.u32()?,
                num_vertices: c.u32()?,
                start_index: c.u32()?,
                prim_count: c.u32()?,
            },
        },
        Op::DrawUp => Command::DrawUp {
            prim: PrimitiveType(c.u32()?),
            prim_count: c.u32()?,
            stride: c.u32()?,
            vertices: c.data()?,
        },
        Op::DrawIndexedUp => Command::DrawIndexedUp {
            prim: PrimitiveType(c.u32()?),
            min_index: c.u32()?,
            num_vertices: c.u32()?,
            prim_count: c.u32()?,
            index_format: Format(c.u32()?),
            stride: c.u32()?,
            indices: c.data()?,
            vertices: c.data()?,
        },
        Op::StretchRect => Command::StretchRect {
            src: c.h()?,
            src_face: c.u32()?,
            src_level: c.u32()?,
            src_rect: c.rect()?,
            dst: c.h()?,
            dst_face: c.u32()?,
            dst_level: c.u32()?,
            dst_rect: c.rect()?,
            filter: TextureFilter(c.u32()?),
        },
        Op::Present => Command::Present { texture: c.h()?, window: c.u32()?, flags: c.u32()? },
        Op::SetGammaRamp => Command::SetGammaRamp { window: c.u32()?, ramp: c.bytes(1536)? },
        Op::ReadTexture => Command::ReadTexture {
            region: c.region()?,
            dest_offset: c.u32()?,
            row_pitch: c.u32()?,
            slice_pitch: c.u32()?,
            fence: c.u64()?,
        },
        Op::Signal => Command::Signal { fence: c.u64()? },
        Op::CreateBuffer11 => {
            Command::CreateBuffer11 { id: c.h()?, size: c.u32()?, bind: c.u32()?, misc: c.u32()?, stride: c.u32()? }
        }
        Op::CreateTexture11 => {
            let id = c.h()?;
            let dim = TextureDim::from_u32(c.u32()?)?;
            Command::CreateTexture11 {
                id,
                desc: Texture11Desc {
                    dim,
                    format: DxgiFormat(c.u32()?),
                    width: c.u32()?,
                    height: c.u32()?,
                    depth_or_array: c.u32()?,
                    mips: c.u32()?,
                    samples: c.u32()?,
                    bind: c.u32()?,
                    misc: c.u32()?,
                },
            }
        }
        Op::UpdateSubresource => Command::UpdateSubresource {
            resource: c.h()?,
            subresource: c.u32()?,
            bx: c.box3()?,
            row_pitch: c.u32()?,
            depth_pitch: c.u32()?,
            data: c.data()?,
        },
        Op::CreateView => {
            let id = c.h()?;
            let kind = ViewKind::from_u32(c.u32()?)?;
            let resource = c.h()?;
            Command::CreateView {
                id,
                kind,
                resource,
                desc: ViewDesc {
                    format: DxgiFormat(c.u32()?),
                    dim: ViewDim(c.u32()?),
                    first_mip: c.u32()?,
                    mip_count: c.u32()?,
                    first_slice: c.u32()?,
                    slice_count: c.u32()?,
                    first_element: c.u32()?,
                    num_elements: c.u32()?,
                    flags: c.u32()?,
                },
            }
        }
        Op::CreateSampler => Command::CreateSampler {
            id: c.h()?,
            desc: SamplerDesc11 {
                filter: c.u32()?,
                address: [c.u32()?, c.u32()?, c.u32()?],
                mip_lod_bias: c.f32()?,
                max_anisotropy: c.u32()?,
                comparison: c.u32()?,
                border: c.f4()?,
                min_lod: c.f32()?,
                max_lod: c.f32()?,
            },
        },
        Op::CreateBlendState => {
            let id = c.h()?;
            let alpha_to_coverage = c.u32()? != 0;
            let independent = c.u32()? != 0;
            let mut targets = [RtBlend::default(); 8];
            for t in &mut targets {
                *t = RtBlend {
                    enable: c.u32()? != 0,
                    src: c.u32()?,
                    dst: c.u32()?,
                    op: c.u32()?,
                    src_alpha: c.u32()?,
                    dst_alpha: c.u32()?,
                    op_alpha: c.u32()?,
                    write_mask: c.u32()?,
                };
            }
            Command::CreateBlendState { id, desc: Box::new(BlendDesc11 { alpha_to_coverage, independent, targets }) }
        }
        Op::CreateDepthStencilState => {
            let id = c.h()?;
            let mut d = DepthStencilDesc11 {
                depth_enable: c.u32()? != 0,
                depth_write: c.u32()? != 0,
                depth_func: c.u32()?,
                stencil_enable: c.u32()? != 0,
                read_mask: c.u32()?,
                write_mask: c.u32()?,
                ..Default::default()
            };
            for f in [&mut d.front, &mut d.back] {
                *f = StencilFace { fail: c.u32()?, depth_fail: c.u32()?, pass: c.u32()?, func: c.u32()? };
            }
            Command::CreateDepthStencilState { id, desc: d }
        }
        Op::CreateRasterizerState => Command::CreateRasterizerState {
            id: c.h()?,
            desc: RasterizerDesc11 {
                fill: c.u32()?,
                cull: c.u32()?,
                front_ccw: c.u32()? != 0,
                depth_bias: c.i32()?,
                depth_bias_clamp: c.f32()?,
                slope_scaled_depth_bias: c.f32()?,
                depth_clip: c.u32()? != 0,
                scissor: c.u32()? != 0,
                multisample: c.u32()? != 0,
                antialiased_line: c.u32()? != 0,
            },
        },
        Op::CreateInputLayout => {
            let id = c.h()?;
            let n = c.u32()?;
            if n > 64 {
                return None;
            }
            let mut elements = Vec::with_capacity(n as usize);
            for _ in 0..n {
                let len = c.u32()? as usize;
                let semantic = std::str::from_utf8(c.bytes(len)?).ok()?.to_string();
                elements.push(InputElement {
                    semantic,
                    semantic_index: c.u32()?,
                    format: DxgiFormat(c.u32()?),
                    slot: c.u32()?,
                    offset: c.u32()?,
                    per_instance: c.u32()? != 0,
                    step_rate: c.u32()?,
                });
            }
            Command::CreateInputLayout { id, elements }
        }
        Op::CreateShader11 => {
            Command::CreateShader11 { id: c.h()?, stage: c.stage()?, hash: c.u64()?, dxbc: c.data()? }
        }
        Op::SetInputLayout => Command::SetInputLayout(c.h()?),
        Op::SetVertexBuffers => {
            let start = c.u32()?;
            let n = c.u32()?;
            if n > 32 {
                return None;
            }
            let buffers = (0..n)
                .map(|_| Some(VertexBufferBinding { buffer: c.h()?, stride: c.u32()?, offset: c.u32()? }))
                .collect::<Option<_>>()?;
            Command::SetVertexBuffers { start, buffers }
        }
        Op::SetIndexBuffer => {
            Command::SetIndexBuffer { buffer: c.h()?, format: DxgiFormat(c.u32()?), offset: c.u32()? }
        }
        Op::SetPrimitiveTopology => Command::SetPrimitiveTopology(c.u32()?),
        Op::SetShader11 => Command::SetShader11 { stage: c.stage()?, id: c.h()? },
        Op::SetConstantBuffers => {
            let stage = c.stage()?;
            let start = c.u32()?;
            let n = c.u32()?;
            if n > 16 {
                return None;
            }
            let buffers = (0..n)
                .map(|_| {
                    Some(ConstantBufferBinding { buffer: c.h()?, first_constant: c.u32()?, num_constants: c.u32()? })
                })
                .collect::<Option<_>>()?;
            Command::SetConstantBuffers { stage, start, buffers }
        }
        Op::SetShaderResources => {
            let (stage, start, views) = c.handles()?;
            Command::SetShaderResources { stage, start, views }
        }
        Op::SetSamplers => {
            let (stage, start, samplers) = c.handles()?;
            Command::SetSamplers { stage, start, samplers }
        }
        Op::SetUnorderedAccessViews => {
            let (stage, start, views) = c.handles()?;
            Command::SetUnorderedAccessViews { stage, start, views }
        }
        Op::SetRenderTargets11 => {
            let n = c.u32()?;
            if n > 8 {
                return None;
            }
            let rtvs = (0..n).map(|_| c.h()).collect::<Option<_>>()?;
            Command::SetRenderTargets11 { rtvs, dsv: c.h()? }
        }
        Op::SetBlendState => Command::SetBlendState { id: c.h()?, factor: c.f4()?, sample_mask: c.u32()? },
        Op::SetDepthStencilState => Command::SetDepthStencilState { id: c.h()?, stencil_ref: c.u32()? },
        Op::SetRasterizerState => Command::SetRasterizerState(c.h()?),
        Op::SetViewports => {
            let n = c.u32()?;
            if n > 16 {
                return None;
            }
            let v = (0..n)
                .map(|_| {
                    Some(Viewport11 {
                        x: c.f32()?,
                        y: c.f32()?,
                        width: c.f32()?,
                        height: c.f32()?,
                        min_depth: c.f32()?,
                        max_depth: c.f32()?,
                    })
                })
                .collect::<Option<_>>()?;
            Command::SetViewports(v)
        }
        Op::SetScissorRects => {
            let n = c.u32()?;
            if n > 16 {
                return None;
            }
            Command::SetScissorRects((0..n).map(|_| c.rect()).collect::<Option<_>>()?)
        }
        Op::Draw11 => Command::Draw11 {
            vertex_count: c.u32()?,
            start_vertex: c.u32()?,
            instance_count: c.u32()?,
            start_instance: c.u32()?,
        },
        Op::DrawIndexed11 => Command::DrawIndexed11 {
            index_count: c.u32()?,
            start_index: c.u32()?,
            base_vertex: c.i32()?,
            instance_count: c.u32()?,
            start_instance: c.u32()?,
        },
        Op::Dispatch => Command::Dispatch { x: c.u32()?, y: c.u32()?, z: c.u32()? },
        Op::ClearRenderTargetView => Command::ClearRenderTargetView { view: c.h()?, color: c.f4()? },
        Op::ClearDepthStencilView => {
            Command::ClearDepthStencilView { view: c.h()?, flags: c.u32()?, depth: c.f32()?, stencil: c.u32()? }
        }
        Op::ClearUnorderedAccessViewUint => {
            Command::ClearUnorderedAccessViewUint { view: c.h()?, values: [c.u32()?, c.u32()?, c.u32()?, c.u32()?] }
        }
        Op::ClearUnorderedAccessViewFloat => Command::ClearUnorderedAccessViewFloat { view: c.h()?, values: c.f4()? },
        Op::CopyResource => Command::CopyResource { dst: c.h()?, src: c.h()? },
        Op::CopySubresourceRegion => Command::CopySubresourceRegion {
            dst: c.h()?,
            dst_sub: c.u32()?,
            x: c.u32()?,
            y: c.u32()?,
            z: c.u32()?,
            src: c.h()?,
            src_sub: c.u32()?,
            bx: c.box3()?,
        },
        Op::ReadSubresource => Command::ReadSubresource {
            resource: c.h()?,
            subresource: c.u32()?,
            bx: c.box3()?,
            dest_offset: c.u32()?,
            row_pitch: c.u32()?,
            depth_pitch: c.u32()?,
            fence: c.u64()?,
        },
        Op::Marker => match c.data()? {
            Data::Inline(b) => Command::Marker(std::str::from_utf8(b).ok()?),
            Data::Shared { .. } => return None,
        },
    })
}

/// Little-endian f32 at index `i` of a constant payload.
pub fn f32_at(data: &[u8], i: usize) -> f32 {
    f32::from_bits(u32::from_le_bytes(data[i * 4..i * 4 + 4].try_into().unwrap()))
}

/// Little-endian u32 at index `i` of a payload.
pub fn u32_at(data: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(data[i * 4..i * 4 + 4].try_into().unwrap())
}
