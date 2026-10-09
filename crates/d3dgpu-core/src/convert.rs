//! Direct3D 9 / emulation-library values to wgpu types.

use d3dgpu_emu::format::{Component, GpuFormat, Swizzle};
use d3dgpu_emu::vertex::GpuVertexFormat;
use d3dgpu_proto::d3d9::*;

pub fn texture_format(f: GpuFormat) -> wgpu::TextureFormat {
    use wgpu::TextureFormat as T;
    match f {
        GpuFormat::R8Unorm => T::R8Unorm,
        GpuFormat::Rg8Unorm => T::Rg8Unorm,
        GpuFormat::Rg8Snorm => T::Rg8Snorm,
        GpuFormat::Rgba8Unorm => T::Rgba8Unorm,
        GpuFormat::Rgba8UnormSrgb => T::Rgba8UnormSrgb,
        GpuFormat::Rgba8Snorm => T::Rgba8Snorm,
        GpuFormat::Bgra8Unorm => T::Bgra8Unorm,
        GpuFormat::Bgra8UnormSrgb => T::Bgra8UnormSrgb,
        GpuFormat::Rgb10a2Unorm => T::Rgb10a2Unorm,
        GpuFormat::R16Float => T::R16Float,
        GpuFormat::Rg16Float => T::Rg16Float,
        GpuFormat::Rgba16Float => T::Rgba16Float,
        GpuFormat::R32Float => T::R32Float,
        GpuFormat::Rg32Float => T::Rg32Float,
        GpuFormat::Rgba32Float => T::Rgba32Float,
        GpuFormat::Depth16Unorm => T::Depth16Unorm,
        GpuFormat::Depth24Plus => T::Depth24Plus,
        GpuFormat::Depth24PlusStencil8 => T::Depth24PlusStencil8,
        GpuFormat::Depth32Float => T::Depth32Float,
        GpuFormat::Bc1RgbaUnorm => T::Bc1RgbaUnorm,
        GpuFormat::Bc1RgbaUnormSrgb => T::Bc1RgbaUnormSrgb,
        GpuFormat::Bc2RgbaUnorm => T::Bc2RgbaUnorm,
        GpuFormat::Bc2RgbaUnormSrgb => T::Bc2RgbaUnormSrgb,
        GpuFormat::Bc3RgbaUnorm => T::Bc3RgbaUnorm,
        GpuFormat::Bc3RgbaUnormSrgb => T::Bc3RgbaUnormSrgb,
        GpuFormat::Bc4RUnorm => T::Bc4RUnorm,
        GpuFormat::Bc5RgUnorm => T::Bc5RgUnorm,
    }
}

/// The translator's swizzle encoding (0..4 = r,g,b,a; 4 = 0; 5 = 1).
pub fn swizzle(s: Swizzle) -> [u8; 4] {
    s.0.map(|c| match c {
        Component::R => 0,
        Component::G => 1,
        Component::B => 2,
        Component::A => 3,
        Component::Zero => 4,
        Component::One => 5,
    })
}

pub fn vertex_format(f: GpuVertexFormat) -> wgpu::VertexFormat {
    use wgpu::VertexFormat as V;
    match f {
        GpuVertexFormat::Float32 => V::Float32,
        GpuVertexFormat::Float32x2 => V::Float32x2,
        GpuVertexFormat::Float32x3 => V::Float32x3,
        GpuVertexFormat::Float32x4 => V::Float32x4,
        GpuVertexFormat::Unorm8x4Bgra => V::Unorm8x4Bgra,
        GpuVertexFormat::Unorm8x4 => V::Unorm8x4,
        GpuVertexFormat::Uint8x4 => V::Uint8x4,
        GpuVertexFormat::Sint16x2 => V::Sint16x2,
        GpuVertexFormat::Sint16x4 => V::Sint16x4,
        GpuVertexFormat::Snorm16x2 => V::Snorm16x2,
        GpuVertexFormat::Snorm16x4 => V::Snorm16x4,
        GpuVertexFormat::Unorm16x2 => V::Unorm16x2,
        GpuVertexFormat::Unorm16x4 => V::Unorm16x4,
        GpuVertexFormat::Float16x2 => V::Float16x2,
        GpuVertexFormat::Float16x4 => V::Float16x4,
        GpuVertexFormat::Uint32 => V::Uint32,
    }
}

pub fn compare(f: u32) -> wgpu::CompareFunction {
    use wgpu::CompareFunction as C;
    match CmpFunc(f) {
        CmpFunc::Never => C::Never,
        CmpFunc::Less => C::Less,
        CmpFunc::Equal => C::Equal,
        CmpFunc::LessEqual => C::LessEqual,
        CmpFunc::Greater => C::Greater,
        CmpFunc::NotEqual => C::NotEqual,
        CmpFunc::GreaterEqual => C::GreaterEqual,
        _ => C::Always,
    }
}

pub fn stencil_op(op: u32) -> wgpu::StencilOperation {
    use wgpu::StencilOperation as S;
    match StencilOp(op) {
        StencilOp::Zero => S::Zero,
        StencilOp::Replace => S::Replace,
        StencilOp::IncrSat => S::IncrementClamp,
        StencilOp::DecrSat => S::DecrementClamp,
        StencilOp::Invert => S::Invert,
        StencilOp::Incr => S::IncrementWrap,
        StencilOp::Decr => S::DecrementWrap,
        _ => S::Keep,
    }
}

/// A `D3DBLEND` factor. `alpha_is_one`: the target has no alpha channel,
/// so destination alpha reads as 1 (as Direct3D defines it).
pub fn blend_factor(b: u32, alpha_is_one: bool) -> wgpu::BlendFactor {
    use wgpu::BlendFactor as F;
    match Blend(b) {
        Blend::Zero => F::Zero,
        Blend::One => F::One,
        Blend::SrcColor => F::Src,
        Blend::InvSrcColor => F::OneMinusSrc,
        Blend::SrcAlpha => F::SrcAlpha,
        Blend::InvSrcAlpha => F::OneMinusSrcAlpha,
        Blend::DestAlpha if alpha_is_one => F::One,
        Blend::InvDestAlpha if alpha_is_one => F::Zero,
        Blend::DestAlpha => F::DstAlpha,
        Blend::InvDestAlpha => F::OneMinusDstAlpha,
        Blend::DestColor => F::Dst,
        Blend::InvDestColor => F::OneMinusDst,
        Blend::SrcAlphaSat => F::SrcAlphaSaturated,
        Blend::BlendFactor => F::Constant,
        Blend::InvBlendFactor => F::OneMinusConstant,
        _ => F::One,
    }
}

pub fn blend_op(op: u32) -> wgpu::BlendOperation {
    use wgpu::BlendOperation as O;
    match BlendOp(op) {
        BlendOp::Subtract => O::Subtract,
        BlendOp::RevSubtract => O::ReverseSubtract,
        BlendOp::Min => O::Min,
        BlendOp::Max => O::Max,
        _ => O::Add,
    }
}

/// A blend component from `D3DRS_SRCBLEND`, `DESTBLEND` and `BLENDOP`,
/// resolving `BOTHSRCALPHA`/`BOTHINVSRCALPHA` and WebGPU's rule that
/// min/max use factor one.
pub fn blend_component(src: u32, dst: u32, op: u32, alpha_is_one: bool) -> wgpu::BlendComponent {
    let (src, dst) = match Blend(src) {
        Blend::BothSrcAlpha => (Blend::SrcAlpha.0, Blend::InvSrcAlpha.0),
        Blend::BothInvSrcAlpha => (Blend::InvSrcAlpha.0, Blend::SrcAlpha.0),
        _ => (src, dst),
    };
    let operation = blend_op(op);
    if matches!(operation, wgpu::BlendOperation::Min | wgpu::BlendOperation::Max) {
        return wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation,
        };
    }
    wgpu::BlendComponent {
        src_factor: blend_factor(src, alpha_is_one),
        dst_factor: blend_factor(dst, alpha_is_one),
        operation,
    }
}

pub fn address_mode(a: u32) -> wgpu::AddressMode {
    match TextureAddress(a) {
        TextureAddress::Mirror => wgpu::AddressMode::MirrorRepeat,
        // Border colour and mirror-once need shader emulation; clamp is the
        // closest WebGPU has.
        TextureAddress::Clamp | TextureAddress::Border | TextureAddress::MirrorOnce => wgpu::AddressMode::ClampToEdge,
        _ => wgpu::AddressMode::Repeat,
    }
}

pub fn filter(f: u32) -> wgpu::FilterMode {
    match TextureFilter(f) {
        TextureFilter::Linear
        | TextureFilter::Anisotropic
        | TextureFilter::PyramidalQuad
        | TextureFilter::GaussianQuad => wgpu::FilterMode::Linear,
        _ => wgpu::FilterMode::Nearest,
    }
}

/// `D3DCOLOR` (0xAARRGGBB) to a clear colour.
pub fn color(c: u32) -> wgpu::Color {
    let ch = |s: u32| ((c >> s) & 0xff) as f64 / 255.0;
    wgpu::Color { r: ch(16), g: ch(8), b: ch(0), a: ch(24) }
}

/// Whether render targets of format `f` blend (32-bit float ones need the
/// `float32-blendable` feature).
pub fn is_blendable(f: wgpu::TextureFormat, features: wgpu::Features) -> bool {
    features.contains(wgpu::Features::FLOAT32_BLENDABLE)
        || !matches!(f, wgpu::TextureFormat::R32Float | wgpu::TextureFormat::Rg32Float | wgpu::TextureFormat::Rgba32Float)
}
