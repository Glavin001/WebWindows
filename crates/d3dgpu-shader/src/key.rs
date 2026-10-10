//! What a translation depends on besides the bytecode (the variant key),
//! and the binding layout every translated shader follows.
//!
//! # Bindings
//!
//! | Group | Binding | Contents |
//! | --- | --- | --- |
//! | 0 | 0 | Vertex shader constants ([`ConstLayout`]) |
//! | 0 | 1 | Pixel shader constants ([`ConstLayout`]) |
//! | 0 | 2 | Driver uniforms ([`DRIVER_WGSL`], filled from [`Driver`]) |
//! | 1 | 2n, 2n+1 | Texture and sampler for sampler `n` (0..16 pixel, 16..20 vertex) |
//!
//! Vertex inputs use `@location(n)` for input register `v<n>`. Inter-stage
//! varyings use locations in the order of [`VertexKey::outputs`] /
//! the pixel shader's own input list (see [`crate::Reflection::ps_inputs`]).

/// A varying's meaning: `D3DDECLUSAGE` plus index (`TEXCOORD3` is
/// `Semantic { usage: usage::TEXCOORD, index: 3 }`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Semantic {
    pub usage: u8,
    pub index: u8,
}

impl Semantic {
    pub const fn new(usage: u8, index: u8) -> Semantic {
        Semantic { usage, index }
    }
}

/// How a varying is interpolated. Must match between the two stages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Interp {
    Perspective,
    Centroid,
    /// `D3DSHADE_FLAT` colours; uses the first vertex, as Direct3D does.
    Flat,
}

/// One inter-stage varying.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Varying {
    pub semantic: Semantic,
    pub interp: Interp,
}

/// How a vertex attribute arrives, decided by the vertex declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum InputKind {
    /// Declared `vec4<f32>` (float, normalized and half formats).
    #[default]
    Float,
    /// `D3DCOLOR` fetched as `unorm8x4` where `unorm8x4-bgra` is missing:
    /// swap red and blue in the shader.
    FloatBgra,
    /// Declared `vec4<u32>` and converted (`UBYTE4`).
    Uint,
    /// Declared `vec4<i32>` and converted (`SHORT2`, `SHORT4`).
    Sint,
    /// Fetched as one `u32` and unpacked: `UDEC3` (unsigned 10:10:10, w = 1).
    Udec3,
    /// Fetched as one `u32` and unpacked: `DEC3N` (signed normalized 10:10:10, w = 1).
    Dec3n,
}

/// How user clip planes are applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum ClipMode {
    #[default]
    None,
    /// `@builtin(clip_distances)` (WebGPU `clip-distances` feature); the
    /// mask says which of the six planes are enabled.
    Builtin(u8),
    /// Distances passed as two extra varyings and tested with `discard` in
    /// the pixel shader (browsers without `clip-distances`). Both stages
    /// must use the same mask.
    Varying(u8),
}

impl ClipMode {
    pub fn mask(self) -> u8 {
        match self {
            ClipMode::None => 0,
            ClipMode::Builtin(m) | ClipMode::Varying(m) => m,
        }
    }
}

/// Number of extra varyings `ClipMode::Varying` uses.
pub const CLIP_VARYINGS: u32 = 2;

/// Vertex shader variant key.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct VertexKey {
    /// Per input register `v0..v15`.
    pub inputs: [InputKind; 16],
    /// The varyings to write, in location order: the pixel shader's inputs.
    /// Semantics the vertex shader doesn't write are written as zero.
    pub outputs: Vec<Varying>,
    pub clip: ClipMode,
    /// Write `@builtin(position)` scaled and offset by
    /// `Driver::pos_fixup` (viewport clamping and the half-pixel offset).
    /// Always on in the render core; off only in tests that want raw output.
    pub pos_fixup: bool,
}

/// What kind of texture a sampler reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum SamplerDim {
    #[default]
    D2,
    Cube,
    Volume,
}

/// Everything about a bound texture that changes the shader.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct SamplerKey {
    pub dim: SamplerDim,
    /// A depth texture: sampled with a comparison sampler (shadow map,
    /// `compare = true`) or read as raw depth.
    pub depth: bool,
    pub compare: bool,
    /// Components to return, per output channel: 0..4 = r,g,b,a of the
    /// sampled value, 4 = 0.0, 5 = 1.0. `[0, 1, 2, 3]` is identity.
    pub swizzle: [u8; 4],
    /// Shader model 1.x only: `D3DTTFF_PROJECTED` on this texture stage.
    pub projected: bool,
}

impl SamplerKey {
    pub const IDENTITY: [u8; 4] = [0, 1, 2, 3];
    pub fn d2() -> SamplerKey {
        SamplerKey { swizzle: Self::IDENTITY, ..Default::default() }
    }
}

/// `D3DCMPFUNC` values for alpha test.
pub mod cmp {
    pub const NEVER: u8 = 1;
    pub const LESS: u8 = 2;
    pub const EQUAL: u8 = 3;
    pub const LESS_EQUAL: u8 = 4;
    pub const GREATER: u8 = 5;
    pub const NOT_EQUAL: u8 = 6;
    pub const GREATER_EQUAL: u8 = 7;
    pub const ALWAYS: u8 = 8;
}

/// Fog applied after a pre-3.0 pixel shader, as fixed-function hardware
/// does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Fog {
    #[default]
    None,
    /// Vertex fog: the factor arrives in the `FOG` varying.
    Vertex,
    /// Table (pixel) fog computed from depth: `D3DFOG_LINEAR`, `EXP`, `EXP2`.
    Linear,
    Exp,
    Exp2,
    /// Vertex fog whose `FOG` varying carries the fog coordinate (eye
    /// distance or depth), turned into the factor per pixel by
    /// `D3DRS_FOGVERTEXMODE`'s equation: wined3d's fixed-function vertex
    /// shaders work this way.
    VertexLinear,
    VertexExp,
    VertexExp2,
}

impl Fog {
    /// Whether the fog reads the `FOG` varying.
    pub fn uses_varying(self) -> bool {
        matches!(self, Fog::Vertex | Fog::VertexLinear | Fog::VertexExp | Fog::VertexExp2)
    }
}

/// Pixel shader variant key.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PixelKey {
    pub samplers: [SamplerKey; 16],
    /// `D3DCMPFUNC` of the alpha test; `cmp::ALWAYS` disables it.
    pub alpha_test: u8,
    pub fog: Fog,
    /// Table fog reads eye depth (W) rather than pixel Z.
    pub fog_w: bool,
    pub clip: ClipMode,
    /// `D3DSHADE_FLAT`: colour varyings use flat interpolation.
    pub flat_shading: bool,
    /// Depth bias: the shader writes the depth plus `Driver::depth_bias`,
    /// as Direct3D adds it, after clipping and in depth units. WebGPU's own
    /// bias is in units of the depth format's precision, which for float
    /// formats (what `depth24plus` is on many GPUs) scales with the depth.
    pub depth_bias: bool,
    /// `D3DRS_SRGBWRITEENABLE` on a target with an sRGB form: colour output
    /// 0 is written sRGB-encoded (by the shader, as wined3d's GLSL backend
    /// does; Direct3D 9 hardware blends the encoded value).
    pub srgb_write: bool,
}

impl Default for PixelKey {
    fn default() -> Self {
        PixelKey {
            samplers: [SamplerKey::d2(); 16],
            alpha_test: cmp::ALWAYS,
            fog: Fog::None,
            fog_w: false,
            clip: ClipMode::None,
            flat_shading: false,
            depth_bias: false,
            srgb_write: false,
        }
    }
}

/// The constant buffer layout of one stage, as WGSL declares it:
/// `f: array<vec4<f32>, F>`, then `i: array<vec4<i32>, 16>`, then
/// `b: array<vec4<u32>, 4>` (16 booleans, one per u32).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConstLayout {
    pub float_count: u32,
}

impl ConstLayout {
    /// Vertex shaders: 256 float constants (`vs_2_0`..`vs_3_0` minimum).
    pub const VERTEX: ConstLayout = ConstLayout { float_count: 256 };
    /// Pixel shaders: 224 float constants (`ps_3_0`).
    pub const PIXEL: ConstLayout = ConstLayout { float_count: 224 };

    pub fn for_stage(stage: crate::Stage) -> ConstLayout {
        match stage {
            crate::Stage::Vertex => Self::VERTEX,
            crate::Stage::Pixel => Self::PIXEL,
        }
    }
    pub fn int_offset(&self) -> u32 {
        self.float_count * 16
    }
    pub fn bool_offset(&self) -> u32 {
        self.int_offset() + 16 * 16
    }
    pub fn size(&self) -> u32 {
        self.bool_offset() + 16 * 4
    }
}

/// WGSL declaration of the driver uniforms, shared by both stages.
pub const DRIVER_WGSL: &str = "struct Driver {
    pos_fixup: vec4<f32>,
    clip_planes: array<vec4<f32>, 6>,
    fog_color: vec4<f32>,
    fog_params: vec4<f32>,
    alpha_ref: vec4<f32>,
    bump_env: array<vec4<f32>, 8>,
    bump_lum: array<vec4<f32>, 8>,
    depth_bias: vec4<f32>,
}
@group(0) @binding(2) var<uniform> drv: Driver;
";

/// The driver uniforms, filled by the render core per draw.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Driver {
    /// Clip-space position fixup: `pos.xy = pos.xy * scale + pos.w * offset`
    /// as `[scale.x, scale.y, offset.x, offset.y]`.
    pub pos_fixup: [f32; 4],
    /// User clip planes in clip space.
    pub clip_planes: [[f32; 4]; 6],
    /// Fog colour (r, g, b, unused) in 0..1.
    pub fog_color: [f32; 4],
    /// Fog start, end, density, 1 / (end - start).
    pub fog_params: [f32; 4],
    /// Alpha test reference in 0..1 (x).
    pub alpha_ref: [f32; 4],
    /// `D3DTSS_BUMPENVMAT00, 01, 10, 11` per texture stage.
    pub bump_env: [[f32; 4]; 8],
    /// `D3DTSS_BUMPENVLSCALE, LOFFSET` per texture stage.
    pub bump_lum: [[f32; 4]; 8],
    /// `D3DRS_DEPTHBIAS` (x) and `D3DRS_SLOPESCALEDEPTHBIAS` (y), applied
    /// by the pixel shader ([`PixelKey::depth_bias`]).
    pub depth_bias: [f32; 4],
}

impl Default for Driver {
    fn default() -> Self {
        Driver {
            pos_fixup: [1.0, 1.0, 0.0, 0.0],
            clip_planes: [[0.0; 4]; 6],
            fog_color: [0.0; 4],
            fog_params: [0.0, 1.0, 1.0, 1.0],
            alpha_ref: [0.0; 4],
            bump_env: [[0.0; 4]; 8],
            bump_lum: [[0.0; 4]; 8],
            depth_bias: [0.0; 4],
        }
    }
}

impl Driver {
    pub const SIZE: usize = 16 * (1 + 6 + 1 + 1 + 1 + 8 + 8 + 1);

    /// The uniform buffer bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::SIZE);
        let mut put = |v: &[f32; 4]| {
            for f in v {
                out.extend_from_slice(&f.to_le_bytes());
            }
        };
        put(&self.pos_fixup);
        self.clip_planes.iter().for_each(&mut put);
        put(&self.fog_color);
        put(&self.fog_params);
        put(&self.alpha_ref);
        self.bump_env.iter().for_each(&mut put);
        self.bump_lum.iter().for_each(&mut put);
        put(&self.depth_bias);
        out
    }
}

/// Bind group 1 binding numbers for sampler `n`.
pub fn texture_binding(sampler: u32) -> u32 {
    sampler * 2
}
pub fn sampler_binding(sampler: u32) -> u32 {
    sampler * 2 + 1
}
