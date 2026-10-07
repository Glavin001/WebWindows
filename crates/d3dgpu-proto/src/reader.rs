//! Decoding batches without copying.

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
        Op::WriteBuffer => Command::WriteBuffer { id: c.h()?, offset: c.u32()?, data: c.data()? },
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
