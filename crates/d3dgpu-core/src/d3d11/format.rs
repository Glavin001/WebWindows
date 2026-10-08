//! DXGI formats in WebGPU terms.

use d3dgpu_dxbc::{BufferFormat, StorageFormat};
use d3dgpu_proto::d3d11::DxgiFormat as F;
use wgpu::TextureFormat as T;

/// The format a texture of DXGI format `f` is created with. Typeless
/// formats take their UNORM/FLOAT member (or the depth format when the
/// texture is a depth-stencil buffer); WebGPU can only reinterpret a
/// texture as its sRGB twin, so other casts between members of a family
/// are not possible.
pub fn texture_format(f: F, depth: bool, features: wgpu::Features) -> Option<T> {
    let bc = features.contains(wgpu::Features::TEXTURE_COMPRESSION_BC);
    let d32s8 = if features.contains(wgpu::Features::DEPTH32FLOAT_STENCIL8) {
        T::Depth32FloatStencil8
    } else {
        T::Depth24PlusStencil8
    };
    Some(match f {
        F::R32Typeless if depth => T::Depth32Float,
        F::R16Typeless if depth => T::Depth16Unorm,
        F::R24G8Typeless | F::D24UnormS8Uint | F::R24UnormX8Typeless | F::X24TypelessG8Uint => T::Depth24PlusStencil8,
        F::R32G8X24Typeless | F::D32FloatS8X24Uint | F::R32FloatX8X24Typeless | F::X32TypelessG8X24Uint => d32s8,
        F::D32Float => T::Depth32Float,
        F::D16Unorm => T::Depth16Unorm,
        F::Bc1Typeless | F::Bc1Unorm | F::Bc1UnormSrgb if bc => typed(f)?,
        F::Bc2Typeless | F::Bc2Unorm | F::Bc2UnormSrgb if bc => typed(f)?,
        F::Bc3Typeless | F::Bc3Unorm | F::Bc3UnormSrgb if bc => typed(f)?,
        F::Bc4Typeless | F::Bc4Unorm | F::Bc4Snorm if bc => typed(f)?,
        F::Bc5Typeless | F::Bc5Unorm | F::Bc5Snorm if bc => typed(f)?,
        F::Bc6hTypeless | F::Bc6hUf16 | F::Bc6hSf16 if bc => typed(f)?,
        F::Bc7Typeless | F::Bc7Unorm | F::Bc7UnormSrgb if bc => typed(f)?,
        _ if f.is_block_compressed() => return None,
        _ => typed(f)?,
    })
}

/// The WebGPU format of a typed (or typeless, by its default member) DXGI
/// colour format.
pub fn typed(f: F) -> Option<T> {
    Some(match f {
        F::R32G32B32A32Typeless | F::R32G32B32A32Float => T::Rgba32Float,
        F::R32G32B32A32Uint => T::Rgba32Uint,
        F::R32G32B32A32Sint => T::Rgba32Sint,
        F::R16G16B16A16Typeless | F::R16G16B16A16Float => T::Rgba16Float,
        F::R16G16B16A16Unorm => T::Rgba16Unorm,
        F::R16G16B16A16Uint => T::Rgba16Uint,
        F::R16G16B16A16Snorm => T::Rgba16Snorm,
        F::R16G16B16A16Sint => T::Rgba16Sint,
        F::R32G32Typeless | F::R32G32Float => T::Rg32Float,
        F::R32G32Uint => T::Rg32Uint,
        F::R32G32Sint => T::Rg32Sint,
        F::R10G10B10A2Typeless | F::R10G10B10A2Unorm => T::Rgb10a2Unorm,
        F::R10G10B10A2Uint => T::Rgb10a2Uint,
        F::R11G11B10Float => T::Rg11b10Ufloat,
        F::R8G8B8A8Typeless | F::R8G8B8A8Unorm => T::Rgba8Unorm,
        F::R8G8B8A8UnormSrgb => T::Rgba8UnormSrgb,
        F::R8G8B8A8Uint => T::Rgba8Uint,
        F::R8G8B8A8Snorm => T::Rgba8Snorm,
        F::R8G8B8A8Sint => T::Rgba8Sint,
        F::R16G16Typeless | F::R16G16Float => T::Rg16Float,
        F::R16G16Unorm => T::Rg16Unorm,
        F::R16G16Uint => T::Rg16Uint,
        F::R16G16Snorm => T::Rg16Snorm,
        F::R16G16Sint => T::Rg16Sint,
        F::R32Typeless | F::R32Float => T::R32Float,
        F::R32Uint => T::R32Uint,
        F::R32Sint => T::R32Sint,
        F::R8G8Typeless | F::R8G8Unorm => T::Rg8Unorm,
        F::R8G8Uint => T::Rg8Uint,
        F::R8G8Snorm => T::Rg8Snorm,
        F::R8G8Sint => T::Rg8Sint,
        F::R16Typeless | F::R16Float => T::R16Float,
        F::R16Unorm => T::R16Unorm,
        F::R16Uint => T::R16Uint,
        F::R16Snorm => T::R16Snorm,
        F::R16Sint => T::R16Sint,
        F::R8Typeless | F::R8Unorm => T::R8Unorm,
        F::R8Uint => T::R8Uint,
        F::R8Snorm => T::R8Snorm,
        F::R8Sint => T::R8Sint,
        F::R9G9B9E5SharedExp => T::Rgb9e5Ufloat,
        // X8 formats: the alpha channel holds garbage that must read as 1
        // (presentation knows; sampling them in a shader does not yet).
        F::B8G8R8A8Typeless | F::B8G8R8A8Unorm | F::B8G8R8X8Typeless | F::B8G8R8X8Unorm => T::Bgra8Unorm,
        F::B8G8R8A8UnormSrgb | F::B8G8R8X8UnormSrgb => T::Bgra8UnormSrgb,
        F::Bc1Typeless | F::Bc1Unorm => T::Bc1RgbaUnorm,
        F::Bc1UnormSrgb => T::Bc1RgbaUnormSrgb,
        F::Bc2Typeless | F::Bc2Unorm => T::Bc2RgbaUnorm,
        F::Bc2UnormSrgb => T::Bc2RgbaUnormSrgb,
        F::Bc3Typeless | F::Bc3Unorm => T::Bc3RgbaUnorm,
        F::Bc3UnormSrgb => T::Bc3RgbaUnormSrgb,
        F::Bc4Typeless | F::Bc4Unorm => T::Bc4RUnorm,
        F::Bc4Snorm => T::Bc4RSnorm,
        F::Bc5Typeless | F::Bc5Unorm => T::Bc5RgUnorm,
        F::Bc5Snorm => T::Bc5RgSnorm,
        F::Bc6hTypeless | F::Bc6hUf16 => T::Bc6hRgbUfloat,
        F::Bc6hSf16 => T::Bc6hRgbFloat,
        F::Bc7Typeless | F::Bc7Unorm => T::Bc7RgbaUnorm,
        F::Bc7UnormSrgb => T::Bc7RgbaUnormSrgb,
        _ => return None,
    })
}

/// Whether the format's alpha channel is undefined and must read as one.
pub fn alpha_is_one(f: F) -> bool {
    matches!(f, F::B8G8R8X8Typeless | F::B8G8R8X8Unorm | F::B8G8R8X8UnormSrgb)
}

/// Which aspect of a depth-stencil texture a view of format `f` sees.
pub fn view_aspect(f: F) -> wgpu::TextureAspect {
    match f {
        F::X24TypelessG8Uint | F::X32TypelessG8X24Uint => wgpu::TextureAspect::StencilOnly,
        F::R24UnormX8Typeless
        | F::R32FloatX8X24Typeless
        | F::R32Float
        | F::R16Unorm
        | F::R32Typeless
        | F::R16Typeless => wgpu::TextureAspect::DepthOnly,
        _ => wgpu::TextureAspect::All,
    }
}

/// Vertex attribute format of an input layout element.
pub fn vertex_format(f: F) -> Option<wgpu::VertexFormat> {
    use wgpu::VertexFormat as V;
    Some(match f {
        F::R32G32B32A32Float => V::Float32x4,
        F::R32G32B32A32Uint => V::Uint32x4,
        F::R32G32B32A32Sint => V::Sint32x4,
        F::R32G32B32Float => V::Float32x3,
        F::R32G32B32Uint => V::Uint32x3,
        F::R32G32B32Sint => V::Sint32x3,
        F::R16G16B16A16Float => V::Float16x4,
        F::R16G16B16A16Unorm => V::Unorm16x4,
        F::R16G16B16A16Uint => V::Uint16x4,
        F::R16G16B16A16Snorm => V::Snorm16x4,
        F::R16G16B16A16Sint => V::Sint16x4,
        F::R32G32Float => V::Float32x2,
        F::R32G32Uint => V::Uint32x2,
        F::R32G32Sint => V::Sint32x2,
        F::R10G10B10A2Unorm => V::Unorm10_10_10_2,
        F::R8G8B8A8Unorm => V::Unorm8x4,
        F::R8G8B8A8Uint => V::Uint8x4,
        F::R8G8B8A8Snorm => V::Snorm8x4,
        F::R8G8B8A8Sint => V::Sint8x4,
        F::B8G8R8A8Unorm => V::Unorm8x4Bgra,
        F::R16G16Float => V::Float16x2,
        F::R16G16Unorm => V::Unorm16x2,
        F::R16G16Uint => V::Uint16x2,
        F::R16G16Snorm => V::Snorm16x2,
        F::R16G16Sint => V::Sint16x2,
        F::R32Float => V::Float32,
        F::R32Uint => V::Uint32,
        F::R32Sint => V::Sint32,
        F::R8G8Unorm => V::Unorm8x2,
        F::R8G8Uint => V::Uint8x2,
        F::R8G8Snorm => V::Snorm8x2,
        F::R8G8Sint => V::Sint8x2,
        F::R16Float => V::Float16,
        F::R16Unorm => V::Unorm16,
        F::R16Uint => V::Uint16,
        F::R16Snorm => V::Snorm16,
        F::R16Sint => V::Sint16,
        F::R8Unorm => V::Unorm8,
        F::R8Uint => V::Uint8,
        F::R8Snorm => V::Snorm8,
        F::R8Sint => V::Sint8,
        _ => return None,
    })
}

/// Element format of a typed buffer view.
pub fn buffer_format(f: F) -> Option<BufferFormat> {
    use BufferFormat as B;
    Some(match f {
        F::R32Uint | F::R32Typeless => B::R32Uint,
        F::R32Sint => B::R32Sint,
        F::R32Float => B::R32Float,
        F::R32G32Uint => B::Rg32Uint,
        F::R32G32Sint => B::Rg32Sint,
        F::R32G32Float => B::Rg32Float,
        F::R32G32B32Uint => B::Rgb32Uint,
        F::R32G32B32Sint => B::Rgb32Sint,
        F::R32G32B32Float => B::Rgb32Float,
        F::R32G32B32A32Uint => B::Rgba32Uint,
        F::R32G32B32A32Sint => B::Rgba32Sint,
        F::R32G32B32A32Float => B::Rgba32Float,
        F::R8G8B8A8Unorm => B::Rgba8Unorm,
        F::R8G8B8A8Uint => B::Rgba8Uint,
        F::R16G16B16A16Float => B::Rgba16Float,
        F::R16Float => B::R16Float,
        F::R16G16Float => B::Rg16Float,
        _ => return None,
    })
}

/// Storage texture format of a texture UAV.
pub fn storage_format(f: T) -> Option<StorageFormat> {
    use StorageFormat as S;
    Some(match f {
        T::Rgba8Unorm => S::Rgba8Unorm,
        T::Rgba8Snorm => S::Rgba8Snorm,
        T::Rgba8Uint => S::Rgba8Uint,
        T::Rgba8Sint => S::Rgba8Sint,
        T::Rgba16Uint => S::Rgba16Uint,
        T::Rgba16Sint => S::Rgba16Sint,
        T::Rgba16Float => S::Rgba16Float,
        T::R32Uint => S::R32Uint,
        T::R32Sint => S::R32Sint,
        T::R32Float => S::R32Float,
        T::Rg32Uint => S::Rg32Uint,
        T::Rg32Sint => S::Rg32Sint,
        T::Rg32Float => S::Rg32Float,
        T::Rgba32Uint => S::Rgba32Uint,
        T::Rgba32Sint => S::Rgba32Sint,
        T::Rgba32Float => S::Rgba32Float,
        T::Bgra8Unorm => S::Bgra8Unorm,
        _ => return None,
    })
}

pub fn storage_texture_format(f: StorageFormat) -> T {
    use StorageFormat as S;
    match f {
        S::Rgba8Unorm => T::Rgba8Unorm,
        S::Rgba8Snorm => T::Rgba8Snorm,
        S::Rgba8Uint => T::Rgba8Uint,
        S::Rgba8Sint => T::Rgba8Sint,
        S::Rgba16Uint => T::Rgba16Uint,
        S::Rgba16Sint => T::Rgba16Sint,
        S::Rgba16Float => T::Rgba16Float,
        S::R32Uint => T::R32Uint,
        S::R32Sint => T::R32Sint,
        S::R32Float => T::R32Float,
        S::Rg32Uint => T::Rg32Uint,
        S::Rg32Sint => T::Rg32Sint,
        S::Rg32Float => T::Rg32Float,
        S::Rgba32Uint => T::Rgba32Uint,
        S::Rgba32Sint => T::Rgba32Sint,
        S::Rgba32Float => T::Rgba32Float,
        S::Bgra8Unorm => T::Bgra8Unorm,
    }
}

/// Bytes per element of a typed view format (`None` for block formats).
pub fn element_bytes(f: F) -> Option<u32> {
    if f.is_block_compressed() {
        return None;
    }
    f.block_bytes()
}
