//! Vertex attribute formats, and repacking vertex buffers WebGPU cannot address.
//!
//! Most `D3DDECLTYPE`s have a WebGPU vertex format with the same bytes and meaning. The rest are fetched raw and
//! converted in the vertex shader ([`ShaderInput`]), which is cheaper than rewriting every vertex buffer on the CPU:
//! `UBYTE4` and `SHORT2/4` are integers Direct3D reads as floats, which WebGPU only fetches as integers; `UDEC3`
//! and `DEC3N` have no WebGPU format at all.
//!
//! Missing components follow the same rule on both sides: Direct3D expands an attribute with fewer than four
//! components to `(x, 0, 0, 1)`-style defaults, and WebGPU fills a shader input wider than its format with 0 for
//! y and z and 1 for w. So shaders always declare four components.

use d3dgpu_proto::d3d9::DeclType;

/// The WebGPU vertex formats Direct3D 9 attributes are fetched as.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GpuVertexFormat {
    Float32,
    Float32x2,
    Float32x3,
    Float32x4,
    /// `unorm8x4-bgra`: `D3DCOLOR`'s byte order, added to core WebGPU in 2025 (Chrome 133).
    Unorm8x4Bgra,
    Unorm8x4,
    Uint8x4,
    Sint16x2,
    Sint16x4,
    Snorm16x2,
    Snorm16x4,
    Unorm16x2,
    Unorm16x4,
    Float16x2,
    Float16x4,
    Uint32,
}

impl GpuVertexFormat {
    /// Bytes per attribute.
    pub fn size(self) -> u32 {
        use GpuVertexFormat::*;
        match self {
            Float32 | Unorm8x4Bgra | Unorm8x4 | Uint8x4 | Sint16x2 | Snorm16x2 | Unorm16x2 | Float16x2 | Uint32 => 4,
            Float32x2 | Sint16x4 | Snorm16x4 | Unorm16x4 | Float16x4 => 8,
            Float32x3 => 12,
            Float32x4 => 16,
        }
    }

    /// The format's name in the WebGPU specification (`GPUVertexFormat`).
    pub fn webgpu_name(self) -> &'static str {
        use GpuVertexFormat::*;
        match self {
            Float32 => "float32",
            Float32x2 => "float32x2",
            Float32x3 => "float32x3",
            Float32x4 => "float32x4",
            Unorm8x4Bgra => "unorm8x4-bgra",
            Unorm8x4 => "unorm8x4",
            Uint8x4 => "uint8x4",
            Sint16x2 => "sint16x2",
            Sint16x4 => "sint16x4",
            Snorm16x2 => "snorm16x2",
            Snorm16x4 => "snorm16x4",
            Unorm16x2 => "unorm16x2",
            Unorm16x4 => "unorm16x4",
            Float16x2 => "float16x2",
            Float16x4 => "float16x4",
            Uint32 => "uint32",
        }
    }
}

/// How the vertex shader declares an attribute and turns it into the `vec4<f32>` Direct3D's shader sees.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShaderInput {
    /// Declare `vec4<f32>`; use as is.
    Float,
    /// Declare `vec4<f32>`; use `.bgra` (actually `.zyxw`). `D3DCOLOR` fetched as `unorm8x4` on devices without
    /// `unorm8x4-bgra`: the bytes are B, G, R, A, so the fetched x is blue. A separate variant rather than a
    /// flag because it is the only attribute that needs it.
    FloatBgra,
    /// Declare `vec4<u32>`; convert with `vec4<f32>(v)` (`UBYTE4`: 0..255, not normalized).
    Uint,
    /// Declare `vec4<i32>`; convert with `vec4<f32>(v)` (`SHORT2`, `SHORT4`: not normalized).
    Sint,
    /// Declare `u32` (fetched as `uint32`); unpack as [`unpack_udec3`]: three unsigned 10-bit fields, not
    /// normalized, w = 1.
    Udec3,
    /// Declare `u32`; unpack as [`unpack_dec3n`]: three signed 10-bit fields divided by 511, w = 1.
    Dec3n,
}

/// What the device can do, as far as vertex formats are concerned.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VertexOptions {
    /// The device supports the `unorm8x4-bgra` vertex format.
    pub bgra_supported: bool,
}

/// The WebGPU format and shader conversion for a `D3DDECLTYPE`, or `None` for `UNUSED` and unknown values.
pub fn vertex_format(ty: DeclType, opts: &VertexOptions) -> Option<(GpuVertexFormat, ShaderInput)> {
    use GpuVertexFormat as V;
    use ShaderInput as S;
    Some(match ty {
        DeclType::Float1 => (V::Float32, S::Float),
        DeclType::Float2 => (V::Float32x2, S::Float),
        DeclType::Float3 => (V::Float32x3, S::Float),
        DeclType::Float4 => (V::Float32x4, S::Float),
        DeclType::D3dColor if opts.bgra_supported => (V::Unorm8x4Bgra, S::Float),
        DeclType::D3dColor => (V::Unorm8x4, S::FloatBgra),
        DeclType::UByte4 => (V::Uint8x4, S::Uint),
        DeclType::Short2 => (V::Sint16x2, S::Sint),
        DeclType::Short4 => (V::Sint16x4, S::Sint),
        DeclType::UByte4N => (V::Unorm8x4, S::Float),
        // Direct3D divides by 32767 like WebGPU; only -32768 differs (-1.00003 vs -1.0).
        DeclType::Short2N => (V::Snorm16x2, S::Float),
        DeclType::Short4N => (V::Snorm16x4, S::Float),
        DeclType::UShort2N => (V::Unorm16x2, S::Float),
        DeclType::UShort4N => (V::Unorm16x4, S::Float),
        DeclType::UDec3 => (V::Uint32, S::Udec3),
        DeclType::Dec3N => (V::Uint32, S::Dec3n),
        DeclType::Float16x2 => (V::Float16x2, S::Float),
        DeclType::Float16x4 => (V::Float16x4, S::Float),
        _ => return None,
    })
}

/// `D3DDECLTYPE_UDEC3` as the shader must compute it: `(x, y, z, 1)` from bits 0-9, 10-19, 20-29, as unnormalized
/// floats. The top two bits are ignored. The CPU reference for the shader code.
pub fn unpack_udec3(v: u32) -> [f32; 4] {
    [(v & 0x3ff) as f32, ((v >> 10) & 0x3ff) as f32, ((v >> 20) & 0x3ff) as f32, 1.0]
}

/// `D3DDECLTYPE_DEC3N` as the shader must compute it: `(x/511, y/511, z/511, 1)` from signed 10-bit fields, with
/// -512 clamped to -1. In WGSL, `vec3<f32>(bitcast<vec3<i32>>(vec3(v) << vec3(22u, 12u, 2u)) >> vec3(22u))`
/// divided by 511.0 and clamped.
pub fn unpack_dec3n(v: u32) -> [f32; 4] {
    let f = |shift: u32| ((((v << (22 - shift)) as i32) >> 22) as f32 / 511.0).max(-1.0);
    [f(0), f(10), f(20), 1.0]
}

/// Whether a vertex buffer layout breaks WebGPU's alignment rules: `arrayStride` and every attribute offset must
/// be multiples of 4 (all our formats are at least 4 bytes), and an attribute must end within the stride. Direct3D
/// 9 has none of these rules. `elements` are `(offset, size)` pairs in bytes; a stride of 0 (every vertex reads the
/// same data) is valid on both sides.
pub fn needs_realign(stride: u32, elements: &[(u32, u32)]) -> bool {
    !stride.is_multiple_of(4)
        || elements.iter().any(|&(offset, size)| !offset.is_multiple_of(4) || (stride != 0 && offset + size > stride))
}

/// A repacked vertex buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Realigned {
    pub data: Vec<u8>,
    pub stride: u32,
    /// The new offset of each input element, in order.
    pub offsets: Vec<u32>,
}

/// Repacks `count` vertices, `stride` bytes apart in `src`, so each element starts on a 4-byte boundary: elements
/// are laid out in order, each rounded up to 4 bytes, and the new stride is their total. Bytes the source does not
/// have (past its end) read as zero. A source stride of 0 yields one vertex with stride 0.
///
/// `src` starts at the first vertex to copy: for an indexed draw, slice it at the lowest index (and remember to
/// rebase the indices) rather than copying unused vertices.
pub fn realign(src: &[u8], count: u32, stride: u32, elements: &[(u32, u32)]) -> Realigned {
    let mut offsets = Vec::with_capacity(elements.len());
    let mut new_stride = 0;
    for &(_, size) in elements {
        offsets.push(new_stride);
        new_stride += size.div_ceil(4) * 4;
    }
    let count = (if stride == 0 { count.min(1) } else { count }) as usize;
    let mut data = vec![0u8; count * new_stride as usize];
    for v in 0..count {
        let base = v * stride as usize;
        let dst = &mut data[v * new_stride as usize..];
        for (&(offset, size), &to) in elements.iter().zip(&offsets) {
            let from = base + offset as usize;
            let n = (size as usize).min(src.len().saturating_sub(from));
            dst[to as usize..to as usize + n].copy_from_slice(&src[from..from + n]);
        }
    }
    Realigned { data, stride: if stride == 0 { 0 } else { new_stride }, offsets }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BGRA: VertexOptions = VertexOptions { bgra_supported: true };
    const NO_BGRA: VertexOptions = VertexOptions { bgra_supported: false };

    #[test]
    fn every_decl_type_maps_with_matching_size() {
        for &(name, ty) in DeclType::ALL {
            match vertex_format(ty, &BGRA) {
                None => assert_eq!(ty, DeclType::Unused, "{name}"),
                Some((f, _)) => assert_eq!(f.size(), ty.size(), "{name}"),
            }
        }
        assert_eq!(vertex_format(DeclType(200), &BGRA), None);
    }

    #[test]
    fn colors_and_integers() {
        assert_eq!(vertex_format(DeclType::D3dColor, &BGRA), Some((GpuVertexFormat::Unorm8x4Bgra, ShaderInput::Float)));
        assert_eq!(
            vertex_format(DeclType::D3dColor, &NO_BGRA),
            Some((GpuVertexFormat::Unorm8x4, ShaderInput::FloatBgra))
        );
        assert_eq!(vertex_format(DeclType::UByte4N, &NO_BGRA), Some((GpuVertexFormat::Unorm8x4, ShaderInput::Float)));
        assert_eq!(vertex_format(DeclType::UByte4, &BGRA), Some((GpuVertexFormat::Uint8x4, ShaderInput::Uint)));
        assert_eq!(vertex_format(DeclType::Short2, &BGRA), Some((GpuVertexFormat::Sint16x2, ShaderInput::Sint)));
        assert_eq!(vertex_format(DeclType::UDec3, &BGRA), Some((GpuVertexFormat::Uint32, ShaderInput::Udec3)));
        assert_eq!(vertex_format(DeclType::Dec3N, &BGRA), Some((GpuVertexFormat::Uint32, ShaderInput::Dec3n)));
        assert_eq!(GpuVertexFormat::Unorm8x4Bgra.webgpu_name(), "unorm8x4-bgra");
    }

    #[test]
    fn dec3_unpacking() {
        let v = 1023 | 512 << 10 | 1 << 20 | 3 << 30;
        assert_eq!(unpack_udec3(v), [1023.0, 512.0, 1.0, 1.0]);
        // 511 -> 1.0, -512 -> -1.0 (clamped), -1 -> -1/511.
        let v = 511 | 0x200 << 10 | 0x3ff << 20;
        assert_eq!(unpack_dec3n(v), [1.0, -1.0, -1.0 / 511.0, 1.0]);
        assert_eq!(unpack_dec3n(0), [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn alignment_checks() {
        assert!(!needs_realign(32, &[(0, 12), (12, 4), (16, 8), (24, 8)]));
        assert!(!needs_realign(0, &[(0, 16)]));
        assert!(needs_realign(30, &[(0, 12)]));
        assert!(needs_realign(32, &[(0, 12), (14, 4)]));
        assert!(needs_realign(16, &[(8, 12)])); // ends past the stride
    }

    #[test]
    fn realign_packs_elements() {
        // Stride 10: a 4-byte element at 0, a 4-byte element at 6 (misaligned).
        let src: Vec<u8> = (0..30).collect();
        let r = realign(&src, 3, 10, &[(0, 4), (6, 4)]);
        assert_eq!(r.stride, 8);
        assert_eq!(r.offsets, [0, 4]);
        assert_eq!(r.data, [0, 1, 2, 3, 6, 7, 8, 9, 10, 11, 12, 13, 16, 17, 18, 19, 20, 21, 22, 23, 26, 27, 28, 29]);
        assert!(!needs_realign(r.stride, &[(0, 4), (4, 4)]));
    }

    #[test]
    fn realign_edge_cases() {
        // Elements past the end of the source read zeros.
        let r = realign(&[1, 2, 3, 4, 5, 6], 2, 4, &[(1, 4)]);
        assert_eq!(r.data, [2, 3, 4, 5, 6, 0, 0, 0]);
        // Stride 0: one vertex.
        let r = realign(&[9, 9, 1, 2, 3, 4], 100, 0, &[(2, 4)]);
        assert_eq!((r.data, r.stride), (vec![1, 2, 3, 4], 0));
        // An odd size rounds up.
        let r = realign(&[1, 2, 3, 4, 5, 6, 7], 1, 7, &[(0, 2), (2, 5)]);
        assert_eq!((r.stride, r.offsets.clone()), (12, vec![0, 4]));
        assert_eq!(r.data, [1, 2, 0, 0, 3, 4, 5, 6, 7, 0, 0, 0]);
        assert!(realign(&[], 0, 4, &[(0, 4)]).data.is_empty());
    }
}
