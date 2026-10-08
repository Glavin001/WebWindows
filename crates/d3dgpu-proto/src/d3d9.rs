//! Direct3D 9 vocabulary used by the protocol.
//!
//! The values are Direct3D 9's own (`D3DFORMAT`, `D3DRENDERSTATETYPE`, ...),
//! so a front end can pass them through unchanged, but they are our types:
//! nothing here depends on wined3d or on Windows headers. Values are kept as
//! open newtypes so unknown ones (FourCC formats, new states) survive the
//! trip and the core can decide what to do with them.

macro_rules! open_enum {
    ($(#[$m:meta])* $name:ident : $repr:ty { $($(#[$vm:meta])* $v:ident = $n:expr,)* }) => {
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
        #[repr(transparent)]
        pub struct $name(pub $repr);
        #[allow(non_upper_case_globals)]
        impl $name {
            $($(#[$vm])* pub const $v: $name = $name($n);)*
            /// Every named value, for tables and tests.
            pub const ALL: &'static [(&'static str, $name)] = &[$((stringify!($v), $name($n)),)*];
            /// The name of a known value.
            pub fn name(self) -> Option<&'static str> {
                Self::ALL.iter().find(|(_, v)| *v == self).map(|(n, _)| *n)
            }
        }
        impl core::fmt::Debug for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                match self.name() {
                    Some(n) => write!(f, "{}::{}", stringify!($name), n),
                    None => write!(f, "{}({:#x})", stringify!($name), self.0),
                }
            }
        }
    };
}

const fn fourcc(s: &[u8; 4]) -> u32 {
    s[0] as u32 | (s[1] as u32) << 8 | (s[2] as u32) << 16 | (s[3] as u32) << 24
}

open_enum! {
    /// `D3DFORMAT`.
    Format: u32 {
        Unknown = 0,
        R8G8B8 = 20,
        A8R8G8B8 = 21,
        X8R8G8B8 = 22,
        R5G6B5 = 23,
        X1R5G5B5 = 24,
        A1R5G5B5 = 25,
        A4R4G4B4 = 26,
        R3G3B2 = 27,
        A8 = 28,
        A8R3G3B2 = 29,
        X4R4G4B4 = 30,
        A2B10G10R10 = 31,
        A8B8G8R8 = 32,
        X8B8G8R8 = 33,
        G16R16 = 34,
        A2R10G10B10 = 35,
        A16B16G16R16 = 36,
        A8P8 = 40,
        P8 = 41,
        L8 = 50,
        A8L8 = 51,
        A4L4 = 52,
        V8U8 = 60,
        L6V5U5 = 61,
        X8L8V8U8 = 62,
        Q8W8V8U8 = 63,
        V16U16 = 64,
        A2W10V10U10 = 67,
        D16Lockable = 70,
        D32 = 71,
        D15S1 = 73,
        D24S8 = 75,
        D24X8 = 77,
        D24X4S4 = 79,
        D16 = 80,
        D32FLockable = 82,
        D24FS8 = 83,
        L16 = 81,
        Index16 = 101,
        Index32 = 102,
        Q16W16V16U16 = 110,
        R16F = 111,
        G16R16F = 112,
        A16B16G16R16F = 113,
        R32F = 114,
        G32R32F = 115,
        A32B32G32R32F = 116,
        Dxt1 = fourcc(b"DXT1"),
        Dxt2 = fourcc(b"DXT2"),
        Dxt3 = fourcc(b"DXT3"),
        Dxt4 = fourcc(b"DXT4"),
        Dxt5 = fourcc(b"DXT5"),
        Ati1 = fourcc(b"ATI1"),
        Ati2 = fourcc(b"ATI2"),
        /// Depth texture formats exposed by D3D9 drivers for shadow maps.
        Intz = fourcc(b"INTZ"),
        Df16 = fourcc(b"DF16"),
        Df24 = fourcc(b"DF24"),
        /// The "null" render target (no colour writes).
        Null = fourcc(b"NULL"),
    }
}

impl Format {
    pub fn is_depth(self) -> bool {
        matches!(
            self,
            Format::D16Lockable
                | Format::D32
                | Format::D15S1
                | Format::D24S8
                | Format::D24X8
                | Format::D24X4S4
                | Format::D16
                | Format::D32FLockable
                | Format::D24FS8
                | Format::Intz
                | Format::Df16
                | Format::Df24
        )
    }
    pub fn has_stencil(self) -> bool {
        matches!(self, Format::D15S1 | Format::D24S8 | Format::D24X4S4 | Format::D24FS8 | Format::Intz)
    }
    pub fn is_block_compressed(self) -> bool {
        matches!(
            self,
            Format::Dxt1 | Format::Dxt2 | Format::Dxt3 | Format::Dxt4 | Format::Dxt5 | Format::Ati1 | Format::Ati2
        )
    }
    /// Bytes per block (4x4 for block-compressed formats, one pixel
    /// otherwise), or `None` for formats with no CPU layout we know.
    pub fn block_bytes(self) -> Option<u32> {
        Some(match self {
            Format::Dxt1 | Format::Ati1 => 8,
            Format::Dxt2 | Format::Dxt3 | Format::Dxt4 | Format::Dxt5 | Format::Ati2 => 16,
            Format::A8 | Format::L8 | Format::P8 | Format::A4L4 | Format::R3G3B2 => 1,
            Format::R5G6B5
            | Format::X1R5G5B5
            | Format::A1R5G5B5
            | Format::A4R4G4B4
            | Format::X4R4G4B4
            | Format::A8R3G3B2
            | Format::A8L8
            | Format::A8P8
            | Format::V8U8
            | Format::L6V5U5
            | Format::L16
            | Format::R16F
            | Format::D16
            | Format::D16Lockable
            | Format::D15S1
            | Format::Df16
            | Format::Index16 => 2,
            Format::R8G8B8 => 3,
            Format::A8R8G8B8
            | Format::X8R8G8B8
            | Format::A8B8G8R8
            | Format::X8B8G8R8
            | Format::A2B10G10R10
            | Format::A2R10G10B10
            | Format::G16R16
            | Format::X8L8V8U8
            | Format::Q8W8V8U8
            | Format::V16U16
            | Format::A2W10V10U10
            | Format::G16R16F
            | Format::R32F
            | Format::D32
            | Format::D24S8
            | Format::D24X8
            | Format::D24X4S4
            | Format::D32FLockable
            | Format::D24FS8
            | Format::Intz
            | Format::Df24
            | Format::Index32 => 4,
            Format::A16B16G16R16 | Format::Q16W16V16U16 | Format::A16B16G16R16F | Format::G32R32F => 8,
            Format::A32B32G32R32F => 16,
            _ => return None,
        })
    }
    /// Width and height of a block in pixels.
    pub fn block_dim(self) -> u32 {
        if self.is_block_compressed() {
            4
        } else {
            1
        }
    }
    /// Bytes in one row of blocks for a surface `width` pixels wide.
    pub fn row_bytes(self, width: u32) -> Option<u32> {
        let b = self.block_dim();
        Some(width.div_ceil(b) * self.block_bytes()?)
    }
    /// Number of block rows for a surface `height` pixels high.
    pub fn block_rows(self, height: u32) -> u32 {
        height.div_ceil(self.block_dim())
    }
}

open_enum! {
    /// `D3DPRIMITIVETYPE`.
    PrimitiveType: u32 {
        PointList = 1,
        LineList = 2,
        LineStrip = 3,
        TriangleList = 4,
        TriangleStrip = 5,
        TriangleFan = 6,
    }
}

impl PrimitiveType {
    /// Vertices (or indices) consumed by `count` primitives.
    pub fn vertex_count(self, count: u32) -> u32 {
        match self {
            PrimitiveType::PointList => count,
            PrimitiveType::LineList => count * 2,
            PrimitiveType::LineStrip => count + 1,
            PrimitiveType::TriangleList => count * 3,
            PrimitiveType::TriangleStrip | PrimitiveType::TriangleFan => count + 2,
            _ => 0,
        }
    }
}

open_enum! {
    /// `D3DRENDERSTATETYPE`.
    RenderState: u32 {
        /// d3dgpu's own (no Direct3D 9 state is 1): non-zero when table fog
        /// reads eye depth (W), as Direct3D does under a perspective
        /// projection; zero for pixel Z (orthographic projections).
        WFog = 1,
        ZEnable = 7,
        FillMode = 8,
        ShadeMode = 9,
        ZWriteEnable = 14,
        AlphaTestEnable = 15,
        LastPixel = 16,
        SrcBlend = 19,
        DestBlend = 20,
        CullMode = 22,
        ZFunc = 23,
        AlphaRef = 24,
        AlphaFunc = 25,
        DitherEnable = 26,
        AlphaBlendEnable = 27,
        FogEnable = 28,
        SpecularEnable = 29,
        FogColor = 34,
        FogTableMode = 35,
        FogStart = 36,
        FogEnd = 37,
        FogDensity = 38,
        RangeFogEnable = 48,
        StencilEnable = 52,
        StencilFail = 53,
        StencilZFail = 54,
        StencilPass = 55,
        StencilFunc = 56,
        StencilRef = 57,
        StencilMask = 58,
        StencilWriteMask = 59,
        TextureFactor = 60,
        Wrap0 = 128,
        Clipping = 136,
        Lighting = 137,
        Ambient = 139,
        FogVertexMode = 140,
        ColorVertex = 141,
        LocalViewer = 142,
        NormalizeNormals = 143,
        DiffuseMaterialSource = 145,
        SpecularMaterialSource = 146,
        AmbientMaterialSource = 147,
        EmissiveMaterialSource = 148,
        VertexBlend = 151,
        ClipPlaneEnable = 152,
        PointSize = 154,
        PointSizeMin = 155,
        PointSpriteEnable = 156,
        PointScaleEnable = 157,
        PointScaleA = 158,
        PointScaleB = 159,
        PointScaleC = 160,
        MultisampleAntialias = 161,
        MultisampleMask = 162,
        PatchEdgeStyle = 163,
        DebugMonitorToken = 165,
        PointSizeMax = 166,
        IndexedVertexBlendEnable = 167,
        ColorWriteEnable = 168,
        TweenFactor = 170,
        BlendOp = 171,
        PositionDegree = 172,
        NormalDegree = 173,
        ScissorTestEnable = 174,
        SlopeScaleDepthBias = 175,
        AntialiasedLineEnable = 176,
        MinTessellationLevel = 178,
        MaxTessellationLevel = 179,
        AdaptiveTessX = 180,
        AdaptiveTessY = 181,
        AdaptiveTessZ = 182,
        AdaptiveTessW = 183,
        EnableAdaptiveTessellation = 184,
        TwoSidedStencilMode = 185,
        CcwStencilFail = 186,
        CcwStencilZFail = 187,
        CcwStencilPass = 188,
        CcwStencilFunc = 189,
        ColorWriteEnable1 = 190,
        ColorWriteEnable2 = 191,
        ColorWriteEnable3 = 192,
        BlendFactor = 193,
        SrgbWriteEnable = 194,
        DepthBias = 195,
        Wrap8 = 198,
        SeparateAlphaBlendEnable = 206,
        SrcBlendAlpha = 207,
        DestBlendAlpha = 208,
        BlendOpAlpha = 209,
    }
}

/// Highest `D3DRENDERSTATETYPE` value plus one.
pub const RENDER_STATE_COUNT: usize = 210;

open_enum! {
    /// `D3DSAMPLERSTATETYPE`.
    SamplerState: u32 {
        AddressU = 1,
        AddressV = 2,
        AddressW = 3,
        BorderColor = 4,
        MagFilter = 5,
        MinFilter = 6,
        MipFilter = 7,
        MipMapLodBias = 8,
        MaxMipLevel = 9,
        MaxAnisotropy = 10,
        SrgbTexture = 11,
        ElementIndex = 12,
        DmapOffset = 13,
    }
}

pub const SAMPLER_STATE_COUNT: usize = 14;

/// Pixel shader samplers are 0..16; vertex texture samplers 16..20
/// (Direct3D's `D3DVERTEXTEXTURESAMPLER0..3`).
pub const MAX_SAMPLERS: usize = 20;
pub const VERTEX_SAMPLER_BASE: u32 = 16;
pub const MAX_STREAMS: usize = 16;
pub const MAX_RENDER_TARGETS: usize = 4;
pub const MAX_CLIP_PLANES: usize = 6;
pub const MAX_TEXTURE_STAGES: usize = 8;

open_enum! {
    /// `D3DTEXTURESTAGESTATETYPE`.
    TextureStageState: u32 {
        ColorOp = 1,
        ColorArg1 = 2,
        ColorArg2 = 3,
        AlphaOp = 4,
        AlphaArg1 = 5,
        AlphaArg2 = 6,
        BumpEnvMat00 = 7,
        BumpEnvMat01 = 8,
        BumpEnvMat10 = 9,
        BumpEnvMat11 = 10,
        TexCoordIndex = 11,
        BumpEnvLScale = 22,
        BumpEnvLOffset = 23,
        TextureTransformFlags = 24,
        ColorArg0 = 26,
        AlphaArg0 = 27,
        ResultArg = 28,
        Constant = 32,
    }
}

pub const TEXTURE_STAGE_STATE_COUNT: usize = 33;

open_enum! {
    /// `D3DDECLTYPE`.
    DeclType: u8 {
        Float1 = 0,
        Float2 = 1,
        Float3 = 2,
        Float4 = 3,
        D3dColor = 4,
        UByte4 = 5,
        Short2 = 6,
        Short4 = 7,
        UByte4N = 8,
        Short2N = 9,
        Short4N = 10,
        UShort2N = 11,
        UShort4N = 12,
        UDec3 = 13,
        Dec3N = 14,
        Float16x2 = 15,
        Float16x4 = 16,
        Unused = 17,
    }
}

impl DeclType {
    /// Size of one element in the vertex buffer.
    pub fn size(self) -> u32 {
        match self {
            DeclType::Float1 => 4,
            DeclType::Float2 => 8,
            DeclType::Float3 => 12,
            DeclType::Float4 => 16,
            DeclType::D3dColor | DeclType::UByte4 | DeclType::UByte4N => 4,
            DeclType::Short2 | DeclType::Short2N | DeclType::UShort2N => 4,
            DeclType::Short4 | DeclType::Short4N | DeclType::UShort4N => 8,
            DeclType::UDec3 | DeclType::Dec3N => 4,
            DeclType::Float16x2 => 4,
            DeclType::Float16x4 => 8,
            _ => 0,
        }
    }
}

open_enum! {
    /// `D3DDECLUSAGE`; also the usage in shader `dcl` instructions.
    DeclUsage: u8 {
        Position = 0,
        BlendWeight = 1,
        BlendIndices = 2,
        Normal = 3,
        PSize = 4,
        TexCoord = 5,
        Tangent = 6,
        Binormal = 7,
        TessFactor = 8,
        PositionT = 9,
        Color = 10,
        Fog = 11,
        Depth = 12,
        Sample = 13,
    }
}

open_enum! {
    /// `D3DCMPFUNC`.
    CmpFunc: u32 {
        Never = 1,
        Less = 2,
        Equal = 3,
        LessEqual = 4,
        Greater = 5,
        NotEqual = 6,
        GreaterEqual = 7,
        Always = 8,
    }
}

open_enum! {
    /// `D3DBLEND`.
    Blend: u32 {
        Zero = 1,
        One = 2,
        SrcColor = 3,
        InvSrcColor = 4,
        SrcAlpha = 5,
        InvSrcAlpha = 6,
        DestAlpha = 7,
        InvDestAlpha = 8,
        DestColor = 9,
        InvDestColor = 10,
        SrcAlphaSat = 11,
        BothSrcAlpha = 12,
        BothInvSrcAlpha = 13,
        BlendFactor = 14,
        InvBlendFactor = 15,
        SrcColor2 = 16,
        InvSrcColor2 = 17,
    }
}

open_enum! {
    /// `D3DBLENDOP`.
    BlendOp: u32 {
        Add = 1,
        Subtract = 2,
        RevSubtract = 3,
        Min = 4,
        Max = 5,
    }
}

open_enum! {
    /// `D3DSTENCILOP`.
    StencilOp: u32 {
        Keep = 1,
        Zero = 2,
        Replace = 3,
        IncrSat = 4,
        DecrSat = 5,
        Invert = 6,
        Incr = 7,
        Decr = 8,
    }
}

open_enum! {
    /// `D3DCULL`.
    Cull: u32 {
        None = 1,
        Cw = 2,
        Ccw = 3,
    }
}

open_enum! {
    /// `D3DFILLMODE`.
    FillMode: u32 {
        Point = 1,
        Wireframe = 2,
        Solid = 3,
    }
}

open_enum! {
    /// `D3DFOGMODE`.
    FogMode: u32 {
        None = 0,
        Exp = 1,
        Exp2 = 2,
        Linear = 3,
    }
}

open_enum! {
    /// `D3DTEXTUREADDRESS`.
    TextureAddress: u32 {
        Wrap = 1,
        Mirror = 2,
        Clamp = 3,
        Border = 4,
        MirrorOnce = 5,
    }
}

open_enum! {
    /// `D3DTEXTUREFILTERTYPE`.
    TextureFilter: u32 {
        None = 0,
        Point = 1,
        Linear = 2,
        Anisotropic = 3,
        PyramidalQuad = 6,
        GaussianQuad = 7,
    }
}

/// `D3DCLEAR_*` flags.
pub mod clear {
    pub const TARGET: u32 = 1;
    pub const ZBUFFER: u32 = 2;
    pub const STENCIL: u32 = 4;
}

/// The render state values Direct3D 9 starts with
/// (`IDirect3DDevice9` after `Reset`), from the D3D9 documentation. State
/// caches must start here, not at zero: a redundant-set filter against an
/// all-zero cache drops a game's first `ZENABLE=FALSE`.
pub fn default_render_states() -> [u32; RENDER_STATE_COUNT] {
    let mut s = [0u32; RENDER_STATE_COUNT];
    let f = |x: f32| x.to_bits();
    let set = |s: &mut [u32; RENDER_STATE_COUNT], r: RenderState, v: u32| s[r.0 as usize] = v;
    // ZENABLE is TRUE when the device was created with an automatic depth
    // stencil; front ends send the real value at creation.
    set(&mut s, RenderState::ZEnable, 1);
    set(&mut s, RenderState::FillMode, FillMode::Solid.0);
    set(&mut s, RenderState::ShadeMode, 2); // D3DSHADE_GOURAUD
    set(&mut s, RenderState::ZWriteEnable, 1);
    set(&mut s, RenderState::LastPixel, 1);
    set(&mut s, RenderState::SrcBlend, Blend::One.0);
    set(&mut s, RenderState::DestBlend, Blend::Zero.0);
    set(&mut s, RenderState::CullMode, Cull::Ccw.0);
    set(&mut s, RenderState::ZFunc, CmpFunc::LessEqual.0);
    set(&mut s, RenderState::AlphaFunc, CmpFunc::Always.0);
    set(&mut s, RenderState::FogStart, f(0.0));
    set(&mut s, RenderState::FogEnd, f(1.0));
    set(&mut s, RenderState::FogDensity, f(1.0));
    set(&mut s, RenderState::StencilFail, StencilOp::Keep.0);
    set(&mut s, RenderState::StencilZFail, StencilOp::Keep.0);
    set(&mut s, RenderState::StencilPass, StencilOp::Keep.0);
    set(&mut s, RenderState::StencilFunc, CmpFunc::Always.0);
    set(&mut s, RenderState::StencilMask, 0xffff_ffff);
    set(&mut s, RenderState::StencilWriteMask, 0xffff_ffff);
    set(&mut s, RenderState::TextureFactor, 0xffff_ffff);
    set(&mut s, RenderState::Clipping, 1);
    set(&mut s, RenderState::Lighting, 1);
    set(&mut s, RenderState::ColorVertex, 1);
    set(&mut s, RenderState::LocalViewer, 1);
    set(&mut s, RenderState::DiffuseMaterialSource, 1); // D3DMCS_COLOR1
    set(&mut s, RenderState::SpecularMaterialSource, 2); // D3DMCS_COLOR2
    set(&mut s, RenderState::PointSize, f(1.0));
    set(&mut s, RenderState::PointSizeMin, f(1.0));
    set(&mut s, RenderState::PointScaleA, f(1.0));
    set(&mut s, RenderState::MultisampleAntialias, 1);
    set(&mut s, RenderState::MultisampleMask, 0xffff_ffff);
    set(&mut s, RenderState::DebugMonitorToken, 0xbaad_cafe);
    set(&mut s, RenderState::PointSizeMax, f(64.0));
    set(&mut s, RenderState::ColorWriteEnable, 0xf);
    set(&mut s, RenderState::BlendOp, BlendOp::Add.0);
    set(&mut s, RenderState::PositionDegree, 3); // D3DDEGREE_CUBIC
    set(&mut s, RenderState::NormalDegree, 1); // D3DDEGREE_LINEAR
    set(&mut s, RenderState::MinTessellationLevel, f(1.0));
    set(&mut s, RenderState::MaxTessellationLevel, f(1.0));
    set(&mut s, RenderState::AdaptiveTessW, f(1.0));
    set(&mut s, RenderState::CcwStencilFail, StencilOp::Keep.0);
    set(&mut s, RenderState::CcwStencilZFail, StencilOp::Keep.0);
    set(&mut s, RenderState::CcwStencilPass, StencilOp::Keep.0);
    set(&mut s, RenderState::CcwStencilFunc, CmpFunc::Always.0);
    set(&mut s, RenderState::ColorWriteEnable1, 0xf);
    set(&mut s, RenderState::ColorWriteEnable2, 0xf);
    set(&mut s, RenderState::ColorWriteEnable3, 0xf);
    set(&mut s, RenderState::BlendFactor, 0xffff_ffff);
    set(&mut s, RenderState::SrcBlendAlpha, Blend::One.0);
    set(&mut s, RenderState::DestBlendAlpha, Blend::Zero.0);
    set(&mut s, RenderState::BlendOpAlpha, BlendOp::Add.0);
    s
}

/// Direct3D 9's default sampler states.
pub fn default_sampler_states() -> [u32; SAMPLER_STATE_COUNT] {
    let mut s = [0u32; SAMPLER_STATE_COUNT];
    s[SamplerState::AddressU.0 as usize] = TextureAddress::Wrap.0;
    s[SamplerState::AddressV.0 as usize] = TextureAddress::Wrap.0;
    s[SamplerState::AddressW.0 as usize] = TextureAddress::Wrap.0;
    s[SamplerState::MagFilter.0 as usize] = TextureFilter::Point.0;
    s[SamplerState::MinFilter.0 as usize] = TextureFilter::Point.0;
    s[SamplerState::MipFilter.0 as usize] = TextureFilter::None.0;
    s[SamplerState::MaxAnisotropy.0 as usize] = 1;
    s
}

/// Direct3D 9's default texture stage states for `stage`.
pub fn default_texture_stage_states(stage: u32) -> [u32; TEXTURE_STAGE_STATE_COUNT] {
    const TA_DIFFUSE: u32 = 0;
    const TA_CURRENT: u32 = 1;
    const TA_TEXTURE: u32 = 2;
    const TOP_DISABLE: u32 = 1;
    const TOP_SELECTARG1: u32 = 2;
    const TOP_MODULATE: u32 = 4;
    let mut s = [0u32; TEXTURE_STAGE_STATE_COUNT];
    let (cop, aop) = if stage == 0 { (TOP_MODULATE, TOP_SELECTARG1) } else { (TOP_DISABLE, TOP_DISABLE) };
    s[TextureStageState::ColorOp.0 as usize] = cop;
    s[TextureStageState::ColorArg1.0 as usize] = TA_TEXTURE;
    s[TextureStageState::ColorArg2.0 as usize] = TA_CURRENT;
    s[TextureStageState::AlphaOp.0 as usize] = aop;
    s[TextureStageState::AlphaArg1.0 as usize] = if stage == 0 { TA_TEXTURE } else { TA_DIFFUSE };
    s[TextureStageState::AlphaArg2.0 as usize] = TA_CURRENT;
    s[TextureStageState::TexCoordIndex.0 as usize] = stage;
    s[TextureStageState::ColorArg0.0 as usize] = TA_CURRENT;
    s[TextureStageState::AlphaArg0.0 as usize] = TA_CURRENT;
    s[TextureStageState::ResultArg.0 as usize] = TA_CURRENT;
    s
}

/// One element of a vertex declaration, laid out exactly like
/// `D3DVERTEXELEMENT9` (8 bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VertexElement {
    pub stream: u16,
    pub offset: u16,
    pub ty: DeclType,
    pub method: u8,
    pub usage: DeclUsage,
    pub usage_index: u8,
}

impl VertexElement {
    pub const fn new(stream: u16, offset: u16, ty: DeclType, usage: DeclUsage, usage_index: u8) -> Self {
        VertexElement { stream, offset, ty, method: 0, usage, usage_index }
    }
}
