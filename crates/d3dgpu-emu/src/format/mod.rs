//! Texture formats: which WebGPU format holds each `D3DFORMAT`, what the CPU does to the bytes on upload and
//! readback, and how the shader swizzles a sample to get Direct3D's channel semantics.
//!
//! The rules, in order of preference:
//!
//! - Store natively when WebGPU has a format with the same bytes: `A8R8G8B8` is `bgra8unorm`, `A8L8` is
//!   `rg8unorm`, `A2B10G10R10` is `rgb10a2unorm`, the float formats are themselves.
//! - Otherwise convert on upload to the closest format core WebGPU has: packed 8- and 16-bit colour to
//!   `rgba8unorm`, 16-bit normalized to float16 (core WebGPU has no `r16unorm`), palettes through the palette, and
//!   BC formats to plain 8-bit texels when the device lacks `texture-compression-bc`.
//! - Channels the Direct3D format lacks read back as Direct3D defines them, through a [`Swizzle`] the shader
//!   applies after sampling, never by baking constants into texels: render targets are written by the GPU, and a
//!   baked alpha would not survive the first draw.
//!
//! # Missing channels
//!
//! Direct3D 9 fills the channels a format lacks with 1, except that luminance replicates into RGB and `A8` reads
//! RGB as 0. Per format (and what wined3d's `format_fixups` table and DXVK's `d3d9_format.cpp` do, which agree
//! where both have the format):
//!
//! | Format | Sample | Notes |
//! |---|---|---|
//! | `X8R8G8B8`, `R5G6B5`, `X1R5G5B5`, other `X` formats | `(r, g, b, 1)` | |
//! | `L8`, `L16` | `(l, l, l, 1)` | |
//! | `A8L8`, `A4L4` | `(l, l, l, a)` | |
//! | `A8` | `(0, 0, 0, a)` | |
//! | `R16F`, `R32F` | `(r, 1, 1, 1)` | wined3d/DXVK: X, ONE, ONE, ONE |
//! | `G16R16`, `G16R16F`, `G32R32F` | `(r, g, 1, 1)` | |
//! | `V8U8`, `V16U16` | `(u, v, 1, 1)` | wined3d/DXVK: X, Y, ONE, ONE |
//! | `L6V5U5`, `X8L8V8U8` | `(u, v, l, 1)` | luminance in blue; less certain, see below |
//! | `Q8W8V8U8`, `Q16W16V16U16`, `A2W10V10U10` | `(u, v, w, q/a)` | |
//! | `ATI1` | `(r, r, r, r)` | wined3d fixup X, X, X, X; DXVK R, R, R, R |
//! | `ATI2` | `(g, r, 1, 1)` | wined3d fixup Y, X, ONE, ONE; DXVK G, R, ONE, ONE. The first BC5 block lands in `.g` |
//! | depth (`INTZ`, `DF16`, `DF24`, `D16`, ...) | `(d, d, d, d)` | INTZ replicates; DF16/DF24 only promise `.r` |
//!
//! Less certain: for `L6V5U5` and `X8L8V8U8` wined3d's fixups place the luminance in blue (its `X8L8V8U8` fixup
//! even passes the X byte through as alpha); `(u, v, l, 1)` follows the Direct3D documentation's channel order and
//! the fixed-function `BUMPENVMAPLUMINANCE` reading `L` separately. Signed 8-bit values use WebGPU's snorm rule
//! (`-128` and `-127` are both -1.0); some Direct3D 9 hardware divided by 128 instead.

pub mod dxt;

use crate::half::{f16_to_f32, f32_to_f16};
use d3dgpu_proto::d3d9::Format;
use dxt::Bc;
use std::borrow::Cow;

/// The WebGPU texture formats Direct3D 9 surfaces are stored in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GpuFormat {
    R8Unorm,
    Rg8Unorm,
    Rg8Snorm,
    Rgba8Unorm,
    Rgba8UnormSrgb,
    Rgba8Snorm,
    Bgra8Unorm,
    Bgra8UnormSrgb,
    Rgb10a2Unorm,
    R16Float,
    Rg16Float,
    Rgba16Float,
    R32Float,
    Rg32Float,
    Rgba32Float,
    Depth16Unorm,
    Depth24Plus,
    Depth24PlusStencil8,
    Depth32Float,
    Bc1RgbaUnorm,
    Bc1RgbaUnormSrgb,
    Bc2RgbaUnorm,
    Bc2RgbaUnormSrgb,
    Bc3RgbaUnorm,
    Bc3RgbaUnormSrgb,
    Bc4RUnorm,
    Bc5RgUnorm,
}

impl GpuFormat {
    /// Bytes per texel, or per 4x4 block for BC formats. `depth24plus` has no defined layout (it cannot be copied
    /// to or from buffers); 4 is a nominal size for budgeting.
    pub fn bytes_per_block(self) -> u32 {
        use GpuFormat::*;
        match self {
            R8Unorm => 1,
            Rg8Unorm | Rg8Snorm | R16Float | Depth16Unorm => 2,
            Rgba8Unorm | Rgba8UnormSrgb | Rgba8Snorm | Bgra8Unorm | Bgra8UnormSrgb | Rgb10a2Unorm | Rg16Float
            | R32Float | Depth24Plus | Depth24PlusStencil8 | Depth32Float => 4,
            Rgba16Float | Rg32Float => 8,
            Rgba32Float => 16,
            Bc1RgbaUnorm | Bc1RgbaUnormSrgb | Bc4RUnorm => 8,
            Bc2RgbaUnorm | Bc2RgbaUnormSrgb | Bc3RgbaUnorm | Bc3RgbaUnormSrgb | Bc5RgUnorm => 16,
        }
    }

    /// Block width and height in texels: 4 for BC formats, 1 otherwise.
    pub fn block_dim(self) -> u32 {
        if self.is_compressed() {
            4
        } else {
            1
        }
    }

    /// Bytes in one tightly packed row of blocks `width` texels wide.
    pub fn row_bytes(self, width: u32) -> u32 {
        width.div_ceil(self.block_dim()) * self.bytes_per_block()
    }

    /// Rows of blocks in a surface `height` texels high.
    pub fn block_rows(self, height: u32) -> u32 {
        height.div_ceil(self.block_dim())
    }

    pub fn is_compressed(self) -> bool {
        use GpuFormat::*;
        matches!(
            self,
            Bc1RgbaUnorm
                | Bc1RgbaUnormSrgb
                | Bc2RgbaUnorm
                | Bc2RgbaUnormSrgb
                | Bc3RgbaUnorm
                | Bc3RgbaUnormSrgb
                | Bc4RUnorm
                | Bc5RgUnorm
        )
    }

    pub fn is_depth(self) -> bool {
        use GpuFormat::*;
        matches!(self, Depth16Unorm | Depth24Plus | Depth24PlusStencil8 | Depth32Float)
    }

    pub fn has_stencil(self) -> bool {
        self == GpuFormat::Depth24PlusStencil8
    }

    pub fn is_srgb(self) -> bool {
        use GpuFormat::*;
        matches!(self, Rgba8UnormSrgb | Bgra8UnormSrgb | Bc1RgbaUnormSrgb | Bc2RgbaUnormSrgb | Bc3RgbaUnormSrgb)
    }

    /// Whether core WebGPU can sample this with a filtering sampler. 32-bit float needs `float32-filterable`;
    /// depth formats sample as `depth` (comparison or non-filtering).
    pub fn is_filterable_float(self) -> bool {
        use GpuFormat::*;
        !self.is_depth() && !matches!(self, R32Float | Rg32Float | Rgba32Float)
    }

    /// Whether core WebGPU can render into this (colour or depth attachment).
    pub fn is_renderable(self) -> bool {
        use GpuFormat::*;
        !self.is_compressed() && !matches!(self, Rg8Snorm | Rgba8Snorm)
    }

    /// The sRGB view of this format, for `D3DSAMP_SRGBTEXTURE` and `D3DRS_SRGBWRITEENABLE`. `None` when WebGPU has
    /// no sRGB variant (the shader then has to linearize). The texture must list the view format in
    /// `viewFormats` at creation.
    pub fn srgb(self) -> Option<GpuFormat> {
        use GpuFormat::*;
        Some(match self {
            Rgba8Unorm | Rgba8UnormSrgb => Rgba8UnormSrgb,
            Bgra8Unorm | Bgra8UnormSrgb => Bgra8UnormSrgb,
            Bc1RgbaUnorm | Bc1RgbaUnormSrgb => Bc1RgbaUnormSrgb,
            Bc2RgbaUnorm | Bc2RgbaUnormSrgb => Bc2RgbaUnormSrgb,
            Bc3RgbaUnorm | Bc3RgbaUnormSrgb => Bc3RgbaUnormSrgb,
            _ => return None,
        })
    }

    /// The linear counterpart of an sRGB format; other formats are returned unchanged.
    pub fn linear(self) -> GpuFormat {
        use GpuFormat::*;
        match self {
            Rgba8UnormSrgb => Rgba8Unorm,
            Bgra8UnormSrgb => Bgra8Unorm,
            Bc1RgbaUnormSrgb => Bc1RgbaUnorm,
            Bc2RgbaUnormSrgb => Bc2RgbaUnorm,
            Bc3RgbaUnormSrgb => Bc3RgbaUnorm,
            f => f,
        }
    }

    /// The format's name in the WebGPU specification (`GPUTextureFormat`).
    pub fn webgpu_name(self) -> &'static str {
        use GpuFormat::*;
        match self {
            R8Unorm => "r8unorm",
            Rg8Unorm => "rg8unorm",
            Rg8Snorm => "rg8snorm",
            Rgba8Unorm => "rgba8unorm",
            Rgba8UnormSrgb => "rgba8unorm-srgb",
            Rgba8Snorm => "rgba8snorm",
            Bgra8Unorm => "bgra8unorm",
            Bgra8UnormSrgb => "bgra8unorm-srgb",
            Rgb10a2Unorm => "rgb10a2unorm",
            R16Float => "r16float",
            Rg16Float => "rg16float",
            Rgba16Float => "rgba16float",
            R32Float => "r32float",
            Rg32Float => "rg32float",
            Rgba32Float => "rgba32float",
            Depth16Unorm => "depth16unorm",
            Depth24Plus => "depth24plus",
            Depth24PlusStencil8 => "depth24plus-stencil8",
            Depth32Float => "depth32float",
            Bc1RgbaUnorm => "bc1-rgba-unorm",
            Bc1RgbaUnormSrgb => "bc1-rgba-unorm-srgb",
            Bc2RgbaUnorm => "bc2-rgba-unorm",
            Bc2RgbaUnormSrgb => "bc2-rgba-unorm-srgb",
            Bc3RgbaUnorm => "bc3-rgba-unorm",
            Bc3RgbaUnormSrgb => "bc3-rgba-unorm-srgb",
            Bc4RUnorm => "bc4-r-unorm",
            Bc5RgUnorm => "bc5-rg-unorm",
        }
    }

    /// Channels per texel for the uncompressed colour formats (used by the 16-bit conversions).
    fn channels(self) -> u32 {
        use GpuFormat::*;
        match self {
            R8Unorm | R16Float | R32Float => 1,
            Rg8Unorm | Rg8Snorm | Rg16Float | Rg32Float => 2,
            _ => 4,
        }
    }
}

/// Where one channel of a swizzled sample comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Component {
    R,
    G,
    B,
    A,
    Zero,
    One,
}

/// The swizzle the shader applies to a sample of the GPU texture to get what Direct3D 9 returns: output channel
/// `i` (r, g, b, a) is `self.0[i]` of the raw sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Swizzle(pub [Component; 4]);

impl Swizzle {
    pub const IDENTITY: Swizzle = Swizzle::new(Component::R, Component::G, Component::B, Component::A);
    /// Formats without alpha (`X8R8G8B8`, `R5G6B5`, ...): alpha reads 1 whatever the GPU stored.
    pub const OPAQUE: Swizzle = Swizzle::new(Component::R, Component::G, Component::B, Component::One);
    /// `L8`, `L16` stored in red.
    pub const LUMINANCE: Swizzle = Swizzle::new(Component::R, Component::R, Component::R, Component::One);
    /// `A8L8`, `A4L4` stored as red = L, green = A.
    pub const LUMINANCE_ALPHA: Swizzle = Swizzle::new(Component::R, Component::R, Component::R, Component::G);
    /// `A8` stored in red.
    pub const ALPHA: Swizzle = Swizzle::new(Component::Zero, Component::Zero, Component::Zero, Component::R);
    /// One-channel formats (`R16F`, `R32F`).
    pub const RED: Swizzle = Swizzle::new(Component::R, Component::One, Component::One, Component::One);
    /// Two-channel formats (`G16R16`, `V8U8`, ...).
    pub const RG: Swizzle = Swizzle::new(Component::R, Component::G, Component::One, Component::One);
    /// Red replicated everywhere (`ATI1`, depth).
    pub const REPLICATE: Swizzle = Swizzle::new(Component::R, Component::R, Component::R, Component::R);

    pub const fn new(r: Component, g: Component, b: Component, a: Component) -> Swizzle {
        Swizzle([r, g, b, a])
    }

    pub fn is_identity(self) -> bool {
        self == Swizzle::IDENTITY
    }

    /// Applies the swizzle to a sample, with `zero` and `one` as the constants (`0u8`/`255u8`, `0.0`/`1.0`).
    pub fn apply<T: Copy>(self, v: [T; 4], zero: T, one: T) -> [T; 4] {
        self.0.map(|c| match c {
            Component::R => v[0],
            Component::G => v[1],
            Component::B => v[2],
            Component::A => v[3],
            Component::Zero => zero,
            Component::One => one,
        })
    }

    /// The swizzle as a WGSL `vec4<f32>` expression over `v`, which should be a plain variable since it may
    /// appear several times. The identity returns `v` itself.
    pub fn wgsl(self, v: &str) -> String {
        if self.is_identity() {
            return v.to_string();
        }
        let c = self.0.map(|c| match c {
            Component::R => format!("{v}.r"),
            Component::G => format!("{v}.g"),
            Component::B => format!("{v}.b"),
            Component::A => format!("{v}.a"),
            Component::Zero => "0.0".to_string(),
            Component::One => "1.0".to_string(),
        });
        format!("vec4<f32>({}, {}, {}, {})", c[0], c[1], c[2], c[3])
    }
}

/// What the CPU does to texel data between Direct3D's layout and the GPU format, on upload ([`convert`]) and,
/// where it makes sense, readback ([`convert_back`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Conversion {
    /// Same bytes; rows are only repacked to the tight pitch.
    None,
    /// The GPU layout differs and cannot be written from the CPU (`depth24plus` has no defined layout; `D32`'s
    /// 32-bit unorm is not `depth32float`). Direct3D 9 cannot lock these either; the texture starts cleared.
    NoUpload,
    /// `R8G8B8` (bytes B, G, R) to rgba8.
    R8G8B8ToRgba8,
    R5G6B5ToRgba8,
    X1R5G5B5ToRgba8,
    A1R5G5B5ToRgba8,
    A4R4G4B4ToRgba8,
    X4R4G4B4ToRgba8,
    R3G3B2ToRgba8,
    A8R3G3B2ToRgba8,
    /// `A4L4` to rg8 (red = L, green = A).
    A4L4ToRg8,
    /// Palette lookup to rgba8; alpha is the entry's `peFlags` byte.
    P8ToRgba8,
    /// Palette lookup for colour, the `A8` byte for alpha.
    A8P8ToRgba8,
    /// U5 V5 signed, L6 unsigned to rgba8snorm as `(u, v, l, 1)`; all bits survive.
    L6V5U5ToRgba8Snorm,
    /// U8 V8 signed, L8 unsigned to rgba8snorm as `(u, v, l, 1)`. L loses its lowest bit (snorm has 7 positive).
    X8L8V8U8ToRgba8Snorm,
    /// Signed 10-bit U, V, W and 2-bit A to rgba16float.
    A2W10V10U10ToRgba16Float,
    /// Swaps the 10-bit R and B fields so the bits match `rgb10a2unorm`.
    A2R10G10B10ToRgb10a2,
    /// Each 16-bit unorm channel (`L16`, `G16R16`, `A16B16G16R16`) to float16. Float16 has 11 significant bits, so
    /// values near 1.0 are 32x coarser than the source (fine for colour, visible in heightmaps).
    Unorm16ToHalf,
    /// Each 16-bit snorm channel (`V16U16`, `Q16W16V16U16`) to float16, with the same precision loss.
    Snorm16ToHalf,
    /// Block-compressed data decoded on the CPU (no `texture-compression-bc`).
    Decode(Bc),
}

/// What the device can do, as far as format choice is concerned.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FormatOptions {
    /// The device has `texture-compression-bc`.
    pub bc_supported: bool,
}

/// How a `D3DFORMAT` is stored and read on WebGPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FormatPlan {
    pub gpu: GpuFormat,
    /// Applied by the shader after sampling.
    pub swizzle: Swizzle,
    pub conversion: Conversion,
    /// Direct3D 9 can render into this format (colour target or depth-stencil) and the plan supports it: the
    /// core renders into `gpu` and reads back through [`convert_back`]. Swizzles do not apply to writes; see
    /// [`FormatPlan::alpha_is_one`] and [`FormatPlan::needs_output_saturate`].
    pub renderable: bool,
}

impl FormatPlan {
    /// The GPU texture size for a `width` x `height` Direct3D surface. WebGPU requires BC textures' base level to be
    /// a whole number of blocks; Direct3D 9 does not.
    pub fn gpu_extent(&self, width: u32, height: u32) -> (u32, u32) {
        let b = self.gpu.block_dim();
        (width.div_ceil(b) * b, height.div_ceil(b) * b)
    }

    /// The format has no alpha, so alpha reads as 1. As a render target the GPU still stores whatever alpha the
    /// shader writes, so the core must treat destination alpha as 1 in blending (`D3DBLEND_DESTALPHA` becomes
    /// `ONE`, `INVDESTALPHA` becomes `ZERO`), as Direct3D does for `X8R8G8B8`, and may mask alpha writes.
    pub fn alpha_is_one(&self) -> bool {
        self.swizzle.0[3] == Component::One
    }

    /// The GPU format is float standing in for a unorm format (`G16R16`, `A16B16G16R16`): as a render target the
    /// shader must saturate its output, since Direct3D would clamp to [0, 1].
    pub fn needs_output_saturate(&self) -> bool {
        self.conversion == Conversion::Unorm16ToHalf
    }

    /// The texels depend on the palette, so a palette change means converting the texture again.
    pub fn uses_palette(&self) -> bool {
        matches!(self.conversion, Conversion::P8ToRgba8 | Conversion::A8P8ToRgba8)
    }
}

/// How `format` is stored on a device with `opts`, or `None` for formats with no texture representation
/// (`Unknown`, `NULL`, index formats, unknown FourCCs).
pub fn plan(format: Format, opts: &FormatOptions) -> Option<FormatPlan> {
    use Conversion as C;
    use GpuFormat as G;
    let p = |gpu, swizzle, conversion, renderable| FormatPlan { gpu, swizzle, conversion, renderable };
    let bc = |native: G, decoded: G, bc: Bc, swizzle| {
        if opts.bc_supported {
            p(native, swizzle, C::None, false)
        } else {
            p(decoded, swizzle, C::Decode(bc), false)
        }
    };
    let id = Swizzle::IDENTITY;
    let opaque = Swizzle::OPAQUE;
    Some(match format {
        Format::A8R8G8B8 => p(G::Bgra8Unorm, id, C::None, true),
        Format::X8R8G8B8 => p(G::Bgra8Unorm, opaque, C::None, true),
        Format::A8B8G8R8 => p(G::Rgba8Unorm, id, C::None, true),
        Format::X8B8G8R8 => p(G::Rgba8Unorm, opaque, C::None, true),
        Format::R8G8B8 => p(G::Rgba8Unorm, opaque, C::R8G8B8ToRgba8, false),
        Format::R5G6B5 => p(G::Rgba8Unorm, opaque, C::R5G6B5ToRgba8, true),
        Format::X1R5G5B5 => p(G::Rgba8Unorm, opaque, C::X1R5G5B5ToRgba8, true),
        Format::A1R5G5B5 => p(G::Rgba8Unorm, id, C::A1R5G5B5ToRgba8, true),
        Format::A4R4G4B4 => p(G::Rgba8Unorm, id, C::A4R4G4B4ToRgba8, true),
        Format::X4R4G4B4 => p(G::Rgba8Unorm, opaque, C::X4R4G4B4ToRgba8, true),
        Format::R3G3B2 => p(G::Rgba8Unorm, opaque, C::R3G3B2ToRgba8, false),
        Format::A8R3G3B2 => p(G::Rgba8Unorm, id, C::A8R3G3B2ToRgba8, false),
        Format::A2B10G10R10 => p(G::Rgb10a2Unorm, id, C::None, true),
        Format::A2R10G10B10 => p(G::Rgb10a2Unorm, id, C::A2R10G10B10ToRgb10a2, true),
        Format::G16R16 => p(G::Rg16Float, Swizzle::RG, C::Unorm16ToHalf, true),
        Format::A16B16G16R16 => p(G::Rgba16Float, id, C::Unorm16ToHalf, true),
        Format::P8 => p(G::Rgba8Unorm, id, C::P8ToRgba8, false),
        Format::A8P8 => p(G::Rgba8Unorm, id, C::A8P8ToRgba8, false),
        Format::L8 => p(G::R8Unorm, Swizzle::LUMINANCE, C::None, false),
        Format::A8L8 => p(G::Rg8Unorm, Swizzle::LUMINANCE_ALPHA, C::None, false),
        Format::A4L4 => p(G::Rg8Unorm, Swizzle::LUMINANCE_ALPHA, C::A4L4ToRg8, false),
        Format::A8 => p(G::R8Unorm, Swizzle::ALPHA, C::None, false),
        Format::L16 => p(G::R16Float, Swizzle::LUMINANCE, C::Unorm16ToHalf, false),
        Format::V8U8 => p(G::Rg8Snorm, Swizzle::RG, C::None, false),
        Format::L6V5U5 => p(G::Rgba8Snorm, opaque, C::L6V5U5ToRgba8Snorm, false),
        Format::X8L8V8U8 => p(G::Rgba8Snorm, opaque, C::X8L8V8U8ToRgba8Snorm, false),
        Format::Q8W8V8U8 => p(G::Rgba8Snorm, id, C::None, false),
        Format::V16U16 => p(G::Rg16Float, Swizzle::RG, C::Snorm16ToHalf, false),
        Format::Q16W16V16U16 => p(G::Rgba16Float, id, C::Snorm16ToHalf, false),
        Format::A2W10V10U10 => p(G::Rgba16Float, id, C::A2W10V10U10ToRgba16Float, false),
        Format::R16F => p(G::R16Float, Swizzle::RED, C::None, true),
        Format::G16R16F => p(G::Rg16Float, Swizzle::RG, C::None, true),
        Format::A16B16G16R16F => p(G::Rgba16Float, id, C::None, true),
        Format::R32F => p(G::R32Float, Swizzle::RED, C::None, true),
        Format::G32R32F => p(G::Rg32Float, Swizzle::RG, C::None, true),
        Format::A32B32G32R32F => p(G::Rgba32Float, id, C::None, true),
        Format::Dxt1 => bc(G::Bc1RgbaUnorm, G::Rgba8Unorm, Bc::Bc1, id),
        Format::Dxt2 | Format::Dxt3 => bc(G::Bc2RgbaUnorm, G::Rgba8Unorm, Bc::Bc2, id),
        Format::Dxt4 | Format::Dxt5 => bc(G::Bc3RgbaUnorm, G::Rgba8Unorm, Bc::Bc3, id),
        Format::Ati1 => bc(G::Bc4RUnorm, G::R8Unorm, Bc::Bc4, Swizzle::REPLICATE),
        Format::Ati2 => bc(
            G::Bc5RgUnorm,
            G::Rg8Unorm,
            Bc::Bc5,
            Swizzle::new(Component::G, Component::R, Component::One, Component::One),
        ),
        // Only D16_LOCKABLE and D32F_LOCKABLE can be locked in Direct3D 9; those (and D16, same bytes) keep their
        // layout. depth32float cannot be a copy destination, so writing D32F_LOCKABLE data takes a shader copy.
        Format::D16 | Format::D16Lockable | Format::Df16 => p(G::Depth16Unorm, Swizzle::REPLICATE, C::None, true),
        Format::D32FLockable => p(G::Depth32Float, Swizzle::REPLICATE, C::None, true),
        Format::D32 => p(G::Depth32Float, Swizzle::REPLICATE, C::NoUpload, true),
        Format::D24X8 | Format::Df24 => p(G::Depth24Plus, Swizzle::REPLICATE, C::NoUpload, true),
        Format::D24S8 | Format::D24X4S4 | Format::D15S1 | Format::D24FS8 | Format::Intz => {
            p(G::Depth24PlusStencil8, Swizzle::REPLICATE, C::NoUpload, true)
        }
        _ => return None,
    })
}

/// `v` (an unsigned `bits`-bit value) expanded to 8 bits by bit replication: exact at 0 and full scale, and the
/// standard (Direct3D, wined3d, DXVK) expansion: `(v << 3) | (v >> 2)` for 5 bits, `v * 17` for 4, and so on.
fn expand(v: u32, bits: u32) -> u8 {
    let mut out = 0;
    let mut shift = 8 - bits as i32;
    while shift > -(bits as i32) {
        out |= if shift >= 0 { v << shift } else { v >> -shift };
        shift -= bits as i32;
    }
    out as u8
}

/// The nearest `bits`-bit value to the 8-bit `v`; the inverse of [`expand`].
fn quantize(v: u8, bits: u32) -> u32 {
    let max = (1 << bits) - 1;
    (v as u32 * max + 127) / 255
}

/// A signed `bits`-bit field (sign-extended) as snorm8, using the `max(v / max, -1)` rule.
fn snorm8(v: i32, max: i32) -> u8 {
    ((v as f32 / max as f32).max(-1.0) * 127.0).round() as i8 as u8
}

fn sext(v: u32, bits: u32) -> i32 {
    ((v << (32 - bits)) as i32) >> (32 - bits)
}

fn u16_at(s: &[u8]) -> u32 {
    u16::from_le_bytes([s[0], s[1]]) as u32
}

fn u32_at(s: &[u8]) -> u32 {
    u32::from_le_bytes([s[0], s[1], s[2], s[3]])
}

fn rgb332(v: u32) -> [u8; 3] {
    [expand((v >> 5) & 7, 3), expand((v >> 2) & 7, 3), expand(v & 3, 2)]
}

/// Swaps the low and high 10-bit fields of a 2:10:10:10 word (an involution).
fn swap_rb10(v: u32) -> u32 {
    (v & 0xc00f_fc00) | (v & 0x3ff) << 20 | (v >> 20) & 0x3ff
}

/// Source bytes per unit, destination bytes per unit, and units per texel for a per-texel conversion. A unit is a
/// texel, or one channel for the 16-bit conversions.
fn unit_layout(c: Conversion, gpu: GpuFormat) -> (usize, usize, u32) {
    use Conversion as C;
    match c {
        C::R8G8B8ToRgba8 => (3, 4, 1),
        C::R5G6B5ToRgba8
        | C::X1R5G5B5ToRgba8
        | C::A1R5G5B5ToRgba8
        | C::A4R4G4B4ToRgba8
        | C::X4R4G4B4ToRgba8
        | C::A8R3G3B2ToRgba8
        | C::A8P8ToRgba8 => (2, 4, 1),
        C::R3G3B2ToRgba8 | C::P8ToRgba8 => (1, 4, 1),
        C::A4L4ToRg8 => (1, 2, 1),
        C::L6V5U5ToRgba8Snorm => (2, 4, 1),
        C::X8L8V8U8ToRgba8Snorm | C::A2R10G10B10ToRgb10a2 => (4, 4, 1),
        C::A2W10V10U10ToRgba16Float => (4, 8, 1),
        C::Unorm16ToHalf | C::Snorm16ToHalf => (2, 2, gpu.channels()),
        C::None | C::NoUpload | C::Decode(_) => unreachable!("not a per-texel conversion"),
    }
}

/// Converts one unit from Direct3D's layout `s` to the GPU layout `d`.
fn convert_unit(c: Conversion, s: &[u8], d: &mut [u8], palette: &[[u8; 4]; 256]) {
    use Conversion as C;
    let mut put = |v: &[u8]| d[..v.len()].copy_from_slice(v);
    match c {
        C::R8G8B8ToRgba8 => put(&[s[2], s[1], s[0], 255]),
        C::R5G6B5ToRgba8 => {
            let v = u16_at(s);
            put(&[expand(v >> 11, 5), expand((v >> 5) & 63, 6), expand(v & 31, 5), 255])
        }
        C::X1R5G5B5ToRgba8 | C::A1R5G5B5ToRgba8 => {
            let v = u16_at(s);
            let a = if c == C::X1R5G5B5ToRgba8 { 255 } else { expand(v >> 15, 1) };
            put(&[expand((v >> 10) & 31, 5), expand((v >> 5) & 31, 5), expand(v & 31, 5), a])
        }
        C::A4R4G4B4ToRgba8 | C::X4R4G4B4ToRgba8 => {
            let v = u16_at(s);
            let a = if c == C::X4R4G4B4ToRgba8 { 255 } else { expand(v >> 12, 4) };
            put(&[expand((v >> 8) & 15, 4), expand((v >> 4) & 15, 4), expand(v & 15, 4), a])
        }
        C::R3G3B2ToRgba8 => {
            let [r, g, b] = rgb332(s[0] as u32);
            put(&[r, g, b, 255])
        }
        C::A8R3G3B2ToRgba8 => {
            let [r, g, b] = rgb332(s[0] as u32);
            put(&[r, g, b, s[1]])
        }
        C::A4L4ToRg8 => put(&[expand(s[0] as u32 & 15, 4), expand(s[0] as u32 >> 4, 4)]),
        C::P8ToRgba8 => put(&palette[s[0] as usize]),
        C::A8P8ToRgba8 => {
            let [r, g, b, _] = palette[s[0] as usize];
            put(&[r, g, b, s[1]])
        }
        C::L6V5U5ToRgba8Snorm => {
            let v = u16_at(s);
            let l = ((v >> 10) * 127 + 31) / 63;
            put(&[snorm8(sext(v & 31, 5), 15), snorm8(sext((v >> 5) & 31, 5), 15), l as u8, 127])
        }
        C::X8L8V8U8ToRgba8Snorm => put(&[s[0], s[1], ((s[2] as u32 * 127 + 127) / 255) as u8, 127]),
        C::A2W10V10U10ToRgba16Float => {
            let v = u32_at(s);
            let n = |x: u32| f32_to_f16((sext(x & 0x3ff, 10) as f32 / 511.0).max(-1.0));
            let a = f32_to_f16((v >> 30) as f32 / 3.0);
            for (i, h) in [n(v), n(v >> 10), n(v >> 20), a].into_iter().enumerate() {
                d[2 * i..2 * i + 2].copy_from_slice(&h.to_le_bytes());
            }
        }
        C::A2R10G10B10ToRgb10a2 => put(&swap_rb10(u32_at(s)).to_le_bytes()),
        C::Unorm16ToHalf => put(&f32_to_f16(u16_at(s) as f32 / 65535.0).to_le_bytes()),
        C::Snorm16ToHalf => {
            let v = u16_at(s) as u16 as i16;
            put(&f32_to_f16((v as f32 / 32767.0).max(-1.0)).to_le_bytes())
        }
        C::None | C::NoUpload | C::Decode(_) => unreachable!("not a per-texel conversion"),
    }
}

/// `src` padded with zeros so `rows` rows of `row_bytes` at `pitch` can be read without bounds checks.
fn padded(src: &[u8], pitch: usize, row_bytes: usize, rows: usize) -> Cow<'_, [u8]> {
    let need = if rows == 0 { 0 } else { (rows - 1) * pitch + row_bytes };
    if src.len() >= need {
        Cow::Borrowed(src)
    } else {
        let mut v = src.to_vec();
        v.resize(need, 0);
        Cow::Owned(v)
    }
}

/// Converts a `width` x `height` surface in Direct3D's layout (rows `src_row_pitch` apart) to tightly packed rows
/// of `plan.gpu`, ready for `writeTexture` (which, unlike buffer copies, has no 256-byte row alignment rule).
///
/// BC data is rows of 4x4 blocks either way; sizes that are not multiples of 4 cover the partial edge blocks.
/// `palette` holds the `PALETTEENTRY`s (red, green, blue, flags) for `P8`/`A8P8`; `peFlags` becomes `P8`'s alpha
/// (wined3d does the same when the palette has alpha; a device without `D3DPTEXTURECAPS_ALPHAPALETTE` should pass
/// entries with flags 255). Without a palette every index reads opaque black.
///
/// A `src` shorter than the surface reads as zeros past its end rather than panicking. [`Conversion::NoUpload`]
/// returns an empty vector.
pub fn convert(
    plan: &FormatPlan,
    src: &[u8],
    width: u32,
    height: u32,
    src_row_pitch: u32,
    palette: Option<&[[u8; 4]; 256]>,
) -> Vec<u8> {
    let pitch = src_row_pitch as usize;
    let out_row = plan.gpu.row_bytes(width) as usize;
    match plan.conversion {
        Conversion::None => {
            let rows = plan.gpu.block_rows(height) as usize;
            let src = padded(src, pitch, out_row, rows);
            let mut out = Vec::with_capacity(out_row * rows);
            for r in 0..rows {
                out.extend_from_slice(&src[r * pitch..r * pitch + out_row]);
            }
            out
        }
        Conversion::NoUpload => Vec::new(),
        Conversion::Decode(bc) => {
            let rgba = dxt::decode(bc, src, width, height, src_row_pitch);
            match plan.gpu.channels() {
                4 => rgba,
                n => rgba.as_chunks::<4>().0.iter().flat_map(|p| p[..n as usize].iter().copied()).collect(),
            }
        }
        c => {
            const BLACK: [[u8; 4]; 256] = [[0, 0, 0, 255]; 256];
            let palette = palette.unwrap_or(&BLACK);
            let (sb, db, units) = unit_layout(c, plan.gpu);
            let n = (width * units) as usize;
            let rows = height as usize;
            let src = padded(src, pitch, n * sb, rows);
            let mut out = vec![0u8; out_row * rows];
            for r in 0..rows {
                let s = &src[r * pitch..r * pitch + n * sb];
                let d = &mut out[r * out_row..(r + 1) * out_row];
                for (s, d) in s.chunks_exact(sb).zip(d.chunks_exact_mut(db)) {
                    convert_unit(c, s, d, palette);
                }
            }
            out
        }
    }
}

/// Converts one unit of GPU data `s` back to Direct3D's layout `d`.
fn convert_unit_back(c: Conversion, s: &[u8], d: &mut [u8]) {
    use Conversion as C;
    let q = |i: usize, bits: u32| quantize(s[i], bits);
    let mut put16 = |v: u32| d[..2].copy_from_slice(&(v as u16).to_le_bytes());
    match c {
        C::R5G6B5ToRgba8 => put16(q(0, 5) << 11 | q(1, 6) << 5 | q(2, 5)),
        C::X1R5G5B5ToRgba8 => put16(1 << 15 | q(0, 5) << 10 | q(1, 5) << 5 | q(2, 5)),
        C::A1R5G5B5ToRgba8 => put16(q(3, 1) << 15 | q(0, 5) << 10 | q(1, 5) << 5 | q(2, 5)),
        C::A4R4G4B4ToRgba8 => put16(q(3, 4) << 12 | q(0, 4) << 8 | q(1, 4) << 4 | q(2, 4)),
        C::X4R4G4B4ToRgba8 => put16(0xf000 | q(0, 4) << 8 | q(1, 4) << 4 | q(2, 4)),
        C::A8R3G3B2ToRgba8 => put16((s[3] as u32) << 8 | q(0, 3) << 5 | q(1, 3) << 2 | q(2, 2)),
        C::R3G3B2ToRgba8 => d[0] = (q(0, 3) << 5 | q(1, 3) << 2 | q(2, 2)) as u8,
        C::R8G8B8ToRgba8 => d[..3].copy_from_slice(&[s[2], s[1], s[0]]),
        C::A4L4ToRg8 => d[0] = (q(1, 4) << 4 | q(0, 4)) as u8,
        C::A2R10G10B10ToRgb10a2 => d[..4].copy_from_slice(&swap_rb10(u32_at(s)).to_le_bytes()),
        C::Unorm16ToHalf => {
            let f = f16_to_f32(u16_at(s) as u16);
            // A NaN survives the clamp and then casts to 0.
            put16((f.clamp(0.0, 1.0) * 65535.0).round() as u32)
        }
        C::Snorm16ToHalf => {
            let f = f16_to_f32(u16_at(s) as u16);
            put16((f.clamp(-1.0, 1.0) * 32767.0).round() as i16 as u16 as u32)
        }
        _ => unreachable!("{c:?} has no inverse"),
    }
}

/// Converts rows read back from the GPU (`gpu_row_pitch` apart, typically padded to 256 bytes by
/// `copyTextureToBuffer`) to Direct3D's layout for `format`, with rows `dst_row_pitch` apart (padding zeroed).
///
/// Supports the render-target formats and whatever else round-trips simply: native formats are copied (the X
/// byte of `X8R8G8B8`/`X8B8G8R8` set to 0xff, since the GPU stored whatever alpha the shader wrote), rgba8 is
/// quantized back to the packed 16/8-bit formats by rounding to nearest (exact for texels that came from those
/// formats), float16 back to 16-bit unorm/snorm, and `depth16unorm`/`depth32float` copied for the lockable depth
/// formats. `None` for palettes, decoded BC data, the bump-luminance formats, `NoUpload` depth, a pitch smaller
/// than a row, or `gpu_rows` too short.
pub fn convert_back(
    format: Format,
    plan: &FormatPlan,
    gpu_rows: &[u8],
    width: u32,
    height: u32,
    gpu_row_pitch: u32,
    dst_row_pitch: u32,
) -> Option<Vec<u8>> {
    let d3d_row = format.row_bytes(width)? as usize;
    let gpu_row = plan.gpu.row_bytes(width) as usize;
    let rows = format.block_rows(height) as usize;
    let (gp, dp) = (gpu_row_pitch as usize, dst_row_pitch as usize);
    if dp < d3d_row || gp < gpu_row || (rows > 0 && gpu_rows.len() < (rows - 1) * gp + gpu_row) {
        return None;
    }
    let mut out = vec![0u8; dp * rows];
    match plan.conversion {
        Conversion::None => {
            for r in 0..rows {
                out[r * dp..r * dp + d3d_row].copy_from_slice(&gpu_rows[r * gp..r * gp + d3d_row]);
                if matches!(format, Format::X8R8G8B8 | Format::X8B8G8R8) {
                    for px in out[r * dp..r * dp + d3d_row].as_chunks_mut::<4>().0 {
                        px[3] = 0xff;
                    }
                }
            }
        }
        Conversion::NoUpload
        | Conversion::Decode(_)
        | Conversion::P8ToRgba8
        | Conversion::A8P8ToRgba8
        | Conversion::L6V5U5ToRgba8Snorm
        | Conversion::X8L8V8U8ToRgba8Snorm
        | Conversion::A2W10V10U10ToRgba16Float => return None,
        c => {
            let (db, sb, units) = unit_layout(c, plan.gpu);
            let n = (width * units) as usize;
            for r in 0..rows {
                let s = &gpu_rows[r * gp..r * gp + n * sb];
                let d = &mut out[r * dp..r * dp + n * db];
                for (s, d) in s.chunks_exact(sb).zip(d.chunks_exact_mut(db)) {
                    convert_unit_back(c, s, d);
                }
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests;
