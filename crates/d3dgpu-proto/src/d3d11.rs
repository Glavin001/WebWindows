//! Direct3D 10/11 vocabulary used by the protocol.
//!
//! As with [`crate::d3d9`], values are Direct3D's own (`DXGI_FORMAT`,
//! `D3D11_FILTER`, `D3D11_BIND_*`, …) so a front end passes them through.
//! Comparison functions, blend factors and operations, stencil operations,
//! fill modes and texture address modes have the same numbering as in
//! Direct3D 9 and reuse those types.

open_enum! {
    /// `DXGI_FORMAT`.
    DxgiFormat: u32 {
        Unknown = 0,
        R32G32B32A32Typeless = 1,
        R32G32B32A32Float = 2,
        R32G32B32A32Uint = 3,
        R32G32B32A32Sint = 4,
        R32G32B32Typeless = 5,
        R32G32B32Float = 6,
        R32G32B32Uint = 7,
        R32G32B32Sint = 8,
        R16G16B16A16Typeless = 9,
        R16G16B16A16Float = 10,
        R16G16B16A16Unorm = 11,
        R16G16B16A16Uint = 12,
        R16G16B16A16Snorm = 13,
        R16G16B16A16Sint = 14,
        R32G32Typeless = 15,
        R32G32Float = 16,
        R32G32Uint = 17,
        R32G32Sint = 18,
        R32G8X24Typeless = 19,
        D32FloatS8X24Uint = 20,
        R32FloatX8X24Typeless = 21,
        X32TypelessG8X24Uint = 22,
        R10G10B10A2Typeless = 23,
        R10G10B10A2Unorm = 24,
        R10G10B10A2Uint = 25,
        R11G11B10Float = 26,
        R8G8B8A8Typeless = 27,
        R8G8B8A8Unorm = 28,
        R8G8B8A8UnormSrgb = 29,
        R8G8B8A8Uint = 30,
        R8G8B8A8Snorm = 31,
        R8G8B8A8Sint = 32,
        R16G16Typeless = 33,
        R16G16Float = 34,
        R16G16Unorm = 35,
        R16G16Uint = 36,
        R16G16Snorm = 37,
        R16G16Sint = 38,
        R32Typeless = 39,
        D32Float = 40,
        R32Float = 41,
        R32Uint = 42,
        R32Sint = 43,
        R24G8Typeless = 44,
        D24UnormS8Uint = 45,
        R24UnormX8Typeless = 46,
        X24TypelessG8Uint = 47,
        R8G8Typeless = 48,
        R8G8Unorm = 49,
        R8G8Uint = 50,
        R8G8Snorm = 51,
        R8G8Sint = 52,
        R16Typeless = 53,
        R16Float = 54,
        D16Unorm = 55,
        R16Unorm = 56,
        R16Uint = 57,
        R16Snorm = 58,
        R16Sint = 59,
        R8Typeless = 60,
        R8Unorm = 61,
        R8Uint = 62,
        R8Snorm = 63,
        R8Sint = 64,
        A8Unorm = 65,
        R9G9B9E5SharedExp = 67,
        Bc1Typeless = 70,
        Bc1Unorm = 71,
        Bc1UnormSrgb = 72,
        Bc2Typeless = 73,
        Bc2Unorm = 74,
        Bc2UnormSrgb = 75,
        Bc3Typeless = 76,
        Bc3Unorm = 77,
        Bc3UnormSrgb = 78,
        Bc4Typeless = 79,
        Bc4Unorm = 80,
        Bc4Snorm = 81,
        Bc5Typeless = 82,
        Bc5Unorm = 83,
        Bc5Snorm = 84,
        B5G6R5Unorm = 85,
        B5G5R5A1Unorm = 86,
        B8G8R8A8Unorm = 87,
        B8G8R8X8Unorm = 88,
        B8G8R8A8Typeless = 90,
        B8G8R8A8UnormSrgb = 91,
        B8G8R8X8Typeless = 92,
        B8G8R8X8UnormSrgb = 93,
        Bc6hTypeless = 94,
        Bc6hUf16 = 95,
        Bc6hSf16 = 96,
        Bc7Typeless = 97,
        Bc7Unorm = 98,
        Bc7UnormSrgb = 99,
        B4G4R4A4Unorm = 115,
    }
}

impl DxgiFormat {
    /// Bytes per element (per 4x4 block for BC formats); `None` when unknown.
    pub fn block_bytes(self) -> Option<u32> {
        use DxgiFormat as F;
        Some(match self.0 {
            1..=4 => 16,
            5..=8 => 12,
            9..=22 => 8,
            23..=47 => 4,
            48..=59 => 2,
            60..=65 => 1,
            67 => 4,
            _ => match self {
                F::Bc1Typeless | F::Bc1Unorm | F::Bc1UnormSrgb | F::Bc4Typeless | F::Bc4Unorm | F::Bc4Snorm => 8,
                F::Bc2Typeless
                | F::Bc2Unorm
                | F::Bc2UnormSrgb
                | F::Bc3Typeless
                | F::Bc3Unorm
                | F::Bc3UnormSrgb
                | F::Bc5Typeless
                | F::Bc5Unorm
                | F::Bc5Snorm
                | F::Bc6hTypeless
                | F::Bc6hUf16
                | F::Bc6hSf16
                | F::Bc7Typeless
                | F::Bc7Unorm
                | F::Bc7UnormSrgb => 16,
                F::B5G6R5Unorm | F::B5G5R5A1Unorm | F::B4G4R4A4Unorm => 2,
                F::B8G8R8A8Unorm
                | F::B8G8R8X8Unorm
                | F::B8G8R8A8Typeless
                | F::B8G8R8A8UnormSrgb
                | F::B8G8R8X8Typeless
                | F::B8G8R8X8UnormSrgb => 4,
                _ => return None,
            },
        })
    }

    pub fn is_block_compressed(self) -> bool {
        (70..=84).contains(&self.0) || (94..=99).contains(&self.0)
    }

    /// Bytes in one row of blocks.
    pub fn row_bytes(self, width: u32) -> Option<u32> {
        let b = if self.is_block_compressed() { 4 } else { 1 };
        Some(width.div_ceil(b) * self.block_bytes()?)
    }

    pub fn block_rows(self, height: u32) -> u32 {
        if self.is_block_compressed() {
            height.div_ceil(4)
        } else {
            height
        }
    }
}

/// `D3D11_BIND_*`.
pub mod bind {
    pub const VERTEX_BUFFER: u32 = 0x1;
    pub const INDEX_BUFFER: u32 = 0x2;
    pub const CONSTANT_BUFFER: u32 = 0x4;
    pub const SHADER_RESOURCE: u32 = 0x8;
    pub const STREAM_OUTPUT: u32 = 0x10;
    pub const RENDER_TARGET: u32 = 0x20;
    pub const DEPTH_STENCIL: u32 = 0x40;
    pub const UNORDERED_ACCESS: u32 = 0x80;
}

/// `D3D11_RESOURCE_MISC_*` flags the protocol uses.
pub mod misc {
    pub const GENERATE_MIPS: u32 = 0x1;
    pub const TEXTURECUBE: u32 = 0x4;
    pub const DRAWINDIRECT_ARGS: u32 = 0x10;
    pub const BUFFER_ALLOW_RAW_VIEWS: u32 = 0x20;
    pub const BUFFER_STRUCTURED: u32 = 0x40;
}

/// Shader stages, numbered as in DXBC program types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum Stage11 {
    Pixel = 0,
    Vertex = 1,
    Geometry = 2,
    Hull = 3,
    Domain = 4,
    Compute = 5,
}

impl Stage11 {
    pub fn from_u32(v: u32) -> Option<Stage11> {
        Some(match v {
            0 => Stage11::Pixel,
            1 => Stage11::Vertex,
            2 => Stage11::Geometry,
            3 => Stage11::Hull,
            4 => Stage11::Domain,
            5 => Stage11::Compute,
            _ => return None,
        })
    }
}

/// Resource dimension of a texture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum TextureDim {
    D1 = 1,
    D2 = 2,
    D3 = 3,
}

impl TextureDim {
    pub fn from_u32(v: u32) -> Option<TextureDim> {
        Some(match v {
            1 => TextureDim::D1,
            2 => TextureDim::D2,
            3 => TextureDim::D3,
            _ => return None,
        })
    }
}

/// What a view is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum ViewKind {
    ShaderResource = 0,
    RenderTarget = 1,
    DepthStencil = 2,
    UnorderedAccess = 3,
}

impl ViewKind {
    pub fn from_u32(v: u32) -> Option<ViewKind> {
        Some(match v {
            0 => ViewKind::ShaderResource,
            1 => ViewKind::RenderTarget,
            2 => ViewKind::DepthStencil,
            3 => ViewKind::UnorderedAccess,
            _ => return None,
        })
    }
}

open_enum! {
    /// View dimensions (one numbering for all view kinds).
    ViewDim: u32 {
        Buffer = 1,
        Texture1D = 2,
        Texture1DArray = 3,
        Texture2D = 4,
        Texture2DArray = 5,
        Texture2DMs = 6,
        Texture2DMsArray = 7,
        Texture3D = 8,
        TextureCube = 9,
        TextureCubeArray = 10,
    }
}

/// View flags.
pub mod view_flags {
    /// Raw (byte address) buffer view.
    pub const RAW: u32 = 0x1;
    /// Depth-stencil views: read-only depth / stencil.
    pub const READ_ONLY_DEPTH: u32 = 0x2;
    pub const READ_ONLY_STENCIL: u32 = 0x4;
    /// UAV with an append/consume counter.
    pub const COUNTER: u32 = 0x8;
}

/// Everything about a view fixed at creation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ViewDesc {
    pub format: DxgiFormat,
    pub dim: ViewDim,
    pub first_mip: u32,
    /// `u32::MAX`: all remaining.
    pub mip_count: u32,
    pub first_slice: u32,
    /// Array slices, cube faces for cube views, depth slices for 3D RTVs.
    pub slice_count: u32,
    /// Buffer views: first element and count.
    pub first_element: u32,
    pub num_elements: u32,
    pub flags: u32,
}

/// Everything about a texture fixed at creation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Texture11Desc {
    pub dim: TextureDim,
    pub format: DxgiFormat,
    pub width: u32,
    pub height: u32,
    /// Depth for 3D textures, array size otherwise.
    pub depth_or_array: u32,
    /// 0: the full chain.
    pub mips: u32,
    pub samples: u32,
    pub bind: u32,
    pub misc: u32,
}

impl Texture11Desc {
    pub fn d2(format: DxgiFormat, width: u32, height: u32, mips: u32, bind: u32) -> Texture11Desc {
        Texture11Desc { dim: TextureDim::D2, format, width, height, depth_or_array: 1, mips, samples: 1, bind, misc: 0 }
    }
}

/// `D3D11_BOX`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Box3 {
    pub left: u32,
    pub top: u32,
    pub front: u32,
    pub right: u32,
    pub bottom: u32,
    pub back: u32,
}

/// `D3D11_SAMPLER_DESC`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SamplerDesc11 {
    /// `D3D11_FILTER`.
    pub filter: u32,
    pub address: [u32; 3],
    pub mip_lod_bias: f32,
    pub max_anisotropy: u32,
    /// `D3D11_COMPARISON_FUNC`.
    pub comparison: u32,
    pub border: [f32; 4],
    pub min_lod: f32,
    pub max_lod: f32,
}

impl Default for SamplerDesc11 {
    fn default() -> Self {
        SamplerDesc11 {
            filter: filter::MIN_MAG_MIP_LINEAR,
            address: [3; 3],
            mip_lod_bias: 0.0,
            max_anisotropy: 1,
            comparison: 1,
            border: [1.0; 4],
            min_lod: f32::MIN,
            max_lod: f32::MAX,
        }
    }
}

/// `D3D11_FILTER` bits.
pub mod filter {
    pub const MIN_MAG_MIP_POINT: u32 = 0x0;
    pub const MIP_LINEAR: u32 = 0x1;
    pub const MAG_LINEAR: u32 = 0x4;
    pub const MIN_LINEAR: u32 = 0x10;
    pub const MIN_MAG_MIP_LINEAR: u32 = 0x15;
    pub const ANISOTROPIC: u32 = 0x55;
    pub const COMPARISON: u32 = 0x80;
}

/// One render target's part of `D3D11_BLEND_DESC`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RtBlend {
    pub enable: bool,
    pub src: u32,
    pub dst: u32,
    pub op: u32,
    pub src_alpha: u32,
    pub dst_alpha: u32,
    pub op_alpha: u32,
    pub write_mask: u32,
}

impl Default for RtBlend {
    fn default() -> Self {
        RtBlend { enable: false, src: 2, dst: 1, op: 1, src_alpha: 2, dst_alpha: 1, op_alpha: 1, write_mask: 0xf }
    }
}

/// `D3D11_BLEND_DESC`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct BlendDesc11 {
    pub alpha_to_coverage: bool,
    pub independent: bool,
    pub targets: [RtBlend; 8],
}

/// One face of `D3D11_DEPTH_STENCILOP_DESC`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StencilFace {
    pub fail: u32,
    pub depth_fail: u32,
    pub pass: u32,
    pub func: u32,
}

impl Default for StencilFace {
    fn default() -> Self {
        StencilFace { fail: 1, depth_fail: 1, pass: 1, func: 8 }
    }
}

/// `D3D11_DEPTH_STENCIL_DESC`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DepthStencilDesc11 {
    pub depth_enable: bool,
    pub depth_write: bool,
    pub depth_func: u32,
    pub stencil_enable: bool,
    pub read_mask: u32,
    pub write_mask: u32,
    pub front: StencilFace,
    pub back: StencilFace,
}

impl Default for DepthStencilDesc11 {
    fn default() -> Self {
        DepthStencilDesc11 {
            depth_enable: true,
            depth_write: true,
            depth_func: 2,
            stencil_enable: false,
            read_mask: 0xff,
            write_mask: 0xff,
            front: StencilFace::default(),
            back: StencilFace::default(),
        }
    }
}

/// `D3D11_CULL_MODE`.
pub mod cull {
    pub const NONE: u32 = 1;
    pub const FRONT: u32 = 2;
    pub const BACK: u32 = 3;
}

/// `D3D11_RASTERIZER_DESC`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RasterizerDesc11 {
    /// 2 wireframe, 3 solid.
    pub fill: u32,
    pub cull: u32,
    pub front_ccw: bool,
    pub depth_bias: i32,
    pub depth_bias_clamp: f32,
    pub slope_scaled_depth_bias: f32,
    pub depth_clip: bool,
    pub scissor: bool,
    pub multisample: bool,
    pub antialiased_line: bool,
}

impl Default for RasterizerDesc11 {
    fn default() -> Self {
        RasterizerDesc11 {
            fill: 3,
            cull: cull::BACK,
            front_ccw: false,
            depth_bias: 0,
            depth_bias_clamp: 0.0,
            slope_scaled_depth_bias: 0.0,
            depth_clip: true,
            scissor: false,
            multisample: false,
            antialiased_line: false,
        }
    }
}

/// `D3D11_INPUT_ELEMENT_DESC`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct InputElement {
    pub semantic: String,
    pub semantic_index: u32,
    pub format: DxgiFormat,
    pub slot: u32,
    /// `D3D11_APPEND_ALIGNED_ELEMENT` (0xffffffff) follows the previous element.
    pub offset: u32,
    /// Per-instance data (`D3D11_INPUT_PER_INSTANCE_DATA`).
    pub per_instance: bool,
    pub step_rate: u32,
}

pub const APPEND_ALIGNED_ELEMENT: u32 = 0xffff_ffff;

/// `D3D11_VIEWPORT`.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Viewport11 {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub min_depth: f32,
    pub max_depth: f32,
}

/// `D3D11_CLEAR_*`.
pub mod clear11 {
    pub const DEPTH: u32 = 1;
    pub const STENCIL: u32 = 2;
}

/// A vertex buffer binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct VertexBufferBinding {
    pub buffer: crate::Handle,
    pub stride: u32,
    pub offset: u32,
}

/// A constant buffer binding (D3D11.1 ranges in 16-byte constants; a
/// count of 0 binds the whole buffer).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ConstantBufferBinding {
    pub buffer: crate::Handle,
    pub first_constant: u32,
    pub num_constants: u32,
}
