//! The d3dgpu command stream.
//!
//! A front end (wined3d's `adapter_wgpu` today, possibly a native `d3d9.dll`
//! later) turns Direct3D 9 calls into commands, writes them into a batch and
//! hands the batch to the render core, which executes it on WebGPU. Nothing
//! in the protocol knows about wined3d: objects are our own handles, and
//! state uses Direct3D 9's vocabulary ([`d3d9`]).
//!
//! # Format
//!
//! A batch is a [`BatchHeader`] followed by commands. Every command is
//!
//! ```text
//! u32 opcode      (an `Op`)
//! u32 size        total bytes including these 8, a multiple of 4
//! ...payload      little-endian u32/i32/f32 fields, then variable data
//! ```
//!
//! Variable-length data is a [`Data`]: either inline in the command, or a
//! range of the shared memory region both sides can see (the front end's
//! upload buffers, a resource's CPU shadow copy). Readback destinations are
//! always shared memory. The same layout is described for C in
//! `include/d3dgpu_proto.h`; a test keeps the two in step.
//!
//! Handles are `u32`, allocated by the front end from one namespace for all
//! object kinds; `0` means "none".

#[macro_use]
pub mod d3d9;
pub mod d3d11;

mod reader;
mod writer;

pub use reader::{f32_at, u32_at, Command, DecodeError, Reader};
pub use writer::DataSrc;
pub use writer::Writer;

use d3d9::*;

/// "D3GP" in little-endian.
pub const MAGIC: u32 = 0x5047_3344;
/// Bumped on any incompatible change to the layout of a command.
pub const VERSION: u32 = 1;

/// Size of the batch header in bytes.
pub const BATCH_HEADER_SIZE: usize = 12;
/// Size of a command header in bytes.
pub const COMMAND_HEADER_SIZE: usize = 8;

/// Start of every batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatchHeader {
    pub magic: u32,
    pub version: u32,
    /// Total length of the batch including this header.
    pub len: u32,
}

/// An object handle. `Handle(0)` is "none".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Handle(pub u32);

impl Handle {
    pub const NONE: Handle = Handle(0);
    pub fn is_none(self) -> bool {
        self.0 == 0
    }
}

/// Where the bytes of a variable-length field live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Data<'a> {
    /// In the command itself.
    Inline(&'a [u8]),
    /// `len` bytes at `offset` in the shared memory region.
    Shared { offset: u32, len: u32 },
}

impl<'a> Data<'a> {
    pub fn len(&self) -> usize {
        match self {
            Data::Inline(b) => b.len(),
            Data::Shared { len, .. } => *len as usize,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// The bytes, given the shared region. `None` if the range is outside it.
    pub fn resolve<'s>(&self, shared: &'s [u8]) -> Option<&'s [u8]>
    where
        'a: 's,
    {
        match *self {
            Data::Inline(b) => Some(b),
            Data::Shared { offset, len } => shared.get(offset as usize..offset as usize + len as usize),
        }
    }
}

/// `Data` tag values.
pub const DATA_INLINE: u32 = 0;
pub const DATA_SHARED: u32 = 1;

macro_rules! ops {
    ($($(#[$m:meta])* $name:ident = $n:expr,)*) => {
        /// Command opcodes.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[repr(u32)]
        pub enum Op { $($(#[$m])* $name = $n,)* }
        impl Op {
            pub const ALL: &'static [Op] = &[$(Op::$name,)*];
            pub fn from_u32(v: u32) -> Option<Op> {
                match v { $($n => Some(Op::$name),)* _ => None }
            }
            pub fn name(self) -> &'static str {
                match self { $(Op::$name => stringify!($name),)* }
            }
        }
    };
}

ops! {
    // Objects.
    CreateBuffer = 0x0001,
    Destroy = 0x0002,
    WriteBuffer = 0x0003,
    CreateTexture = 0x0004,
    WriteTexture = 0x0005,
    CreateShader = 0x0006,
    CreateVertexDecl = 0x0007,
    SetPalette = 0x0008,
    // State.
    SetRenderTarget = 0x0010,
    SetDepthStencil = 0x0011,
    SetViewport = 0x0012,
    SetScissor = 0x0013,
    SetRenderState = 0x0014,
    SetSamplerState = 0x0015,
    SetTexture = 0x0016,
    SetTextureStageState = 0x0017,
    SetVertexShader = 0x0018,
    SetPixelShader = 0x0019,
    SetVertexDecl = 0x001A,
    SetStreamSource = 0x001B,
    SetStreamFreq = 0x001C,
    SetIndices = 0x001D,
    SetShaderConstF = 0x001E,
    SetShaderConstI = 0x001F,
    SetShaderConstB = 0x0020,
    SetClipPlane = 0x0021,
    // Drawing.
    Clear = 0x0030,
    Draw = 0x0031,
    DrawIndexed = 0x0032,
    DrawUp = 0x0033,
    DrawIndexedUp = 0x0034,
    StretchRect = 0x0035,
    // Frames and synchronisation.
    Present = 0x0040,
    SetGammaRamp = 0x0041,
    ReadTexture = 0x0050,
    Signal = 0x0051,
    // Debugging.
    Marker = 0x0060,
    // Direct3D 10/11 objects.
    CreateBuffer11 = 0x0100,
    CreateTexture11 = 0x0101,
    UpdateSubresource = 0x0102,
    CreateView = 0x0103,
    CreateSampler = 0x0104,
    CreateBlendState = 0x0105,
    CreateDepthStencilState = 0x0106,
    CreateRasterizerState = 0x0107,
    CreateInputLayout = 0x0108,
    CreateShader11 = 0x0109,
    // Direct3D 10/11 state.
    SetInputLayout = 0x0110,
    SetVertexBuffers = 0x0111,
    SetIndexBuffer = 0x0112,
    SetPrimitiveTopology = 0x0113,
    SetShader11 = 0x0114,
    SetConstantBuffers = 0x0115,
    SetShaderResources = 0x0116,
    SetSamplers = 0x0117,
    SetUnorderedAccessViews = 0x0118,
    SetRenderTargets11 = 0x0119,
    SetBlendState = 0x011A,
    SetDepthStencilState = 0x011B,
    SetRasterizerState = 0x011C,
    SetViewports = 0x011D,
    SetScissorRects = 0x011E,
    // Direct3D 10/11 work.
    Draw11 = 0x0130,
    DrawIndexed11 = 0x0131,
    Dispatch = 0x0132,
    ClearRenderTargetView = 0x0133,
    ClearDepthStencilView = 0x0134,
    ClearUnorderedAccessViewUint = 0x0135,
    ClearUnorderedAccessViewFloat = 0x0136,
    CopyResource = 0x0137,
    CopySubresourceRegion = 0x0138,
    ReadSubresource = 0x0139,
}

/// `CreateBuffer` usage bits.
pub mod buffer_usage {
    pub const VERTEX: u32 = 1;
    pub const INDEX: u32 = 2;
    /// Rewritten often (D3DUSAGE_DYNAMIC); a hint.
    pub const DYNAMIC: u32 = 4;
}

/// `CreateTexture` usage bits.
pub mod texture_usage {
    pub const RENDER_TARGET: u32 = 1;
    pub const DEPTH_STENCIL: u32 = 2;
    pub const DYNAMIC: u32 = 4;
}

/// Texture kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum TextureKind {
    D2 = 0,
    Cube = 1,
    Volume = 2,
}

impl TextureKind {
    pub fn from_u32(v: u32) -> Option<TextureKind> {
        Some(match v {
            0 => TextureKind::D2,
            1 => TextureKind::Cube,
            2 => TextureKind::Volume,
            _ => return None,
        })
    }
}

/// Shader stages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum Stage {
    Vertex = 0,
    Pixel = 1,
}

impl Stage {
    pub fn from_u32(v: u32) -> Option<Stage> {
        Some(match v {
            0 => Stage::Vertex,
            1 => Stage::Pixel,
            _ => return None,
        })
    }
}

/// `Present` flags.
pub mod present {
    /// Wait for the frame to reach the screen before executing later
    /// commands (only meaningful in hosts that can wait).
    pub const VSYNC: u32 = 1;
}

/// A rectangle in pixels, `D3DRECT` style (right and bottom exclusive).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Rect {
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
}

impl Rect {
    pub const fn new(x1: i32, y1: i32, x2: i32, y2: i32) -> Rect {
        Rect { x1, y1, x2, y2 }
    }
    pub fn width(&self) -> u32 {
        (self.x2 - self.x1).max(0) as u32
    }
    pub fn height(&self) -> u32 {
        (self.y2 - self.y1).max(0) as u32
    }
}

/// `D3DVIEWPORT9`.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Viewport {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub min_z: f32,
    pub max_z: f32,
}

/// A region of one subresource of a texture (a box in volume textures).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct TextureRegion {
    pub texture: Handle,
    /// Cube face (0..6), 0 otherwise.
    pub face: u32,
    pub level: u32,
    pub x: u32,
    pub y: u32,
    pub z: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
}

/// Everything about a texture fixed at creation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextureDesc {
    pub kind: TextureKind,
    pub format: Format,
    pub width: u32,
    pub height: u32,
    /// 1 unless `kind` is `Volume`.
    pub depth: u32,
    pub levels: u32,
    /// [`texture_usage`] bits.
    pub usage: u32,
}

impl TextureDesc {
    pub fn d2(format: Format, width: u32, height: u32, levels: u32, usage: u32) -> TextureDesc {
        TextureDesc { kind: TextureKind::D2, format, width, height, depth: 1, levels, usage }
    }
}

/// The parameters of an indexed draw (`DrawIndexedPrimitive`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct IndexedDraw {
    pub base_vertex: i32,
    pub min_index: u32,
    pub num_vertices: u32,
    pub start_index: u32,
    pub prim_count: u32,
}
