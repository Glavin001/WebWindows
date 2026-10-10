use super::*;
use crate::half::f16_to_f32;

const BC: FormatOptions = FormatOptions { bc_supported: true, float32: false };
const NO_BC: FormatOptions = FormatOptions { bc_supported: false, float32: false };

fn pl(f: Format) -> FormatPlan {
    plan(f, &BC).unwrap()
}

/// Converts one row of texels given as packed little-endian values of `bytes` each.
fn up(f: Format, texels: &[u32], bytes: usize) -> Vec<u8> {
    let src: Vec<u8> = texels.iter().flat_map(|t| t.to_le_bytes()[..bytes].to_vec()).collect();
    convert(&pl(f), &src, texels.len() as u32, 1, src.len() as u32, None)
}

#[test]
fn bit_expansion_is_standard() {
    for v in 0..32 {
        assert_eq!(expand(v, 5), ((v << 3) | (v >> 2)) as u8);
    }
    for v in 0..64 {
        assert_eq!(expand(v, 6), ((v << 2) | (v >> 4)) as u8);
    }
    for v in 0..16 {
        assert_eq!(expand(v, 4), (v * 17) as u8);
    }
    for v in 0..8 {
        assert_eq!(expand(v, 3), ((v << 5) | (v << 2) | (v >> 1)) as u8);
    }
    assert_eq!([0, 1, 2, 3].map(|v| expand(v, 2)), [0, 85, 170, 255]);
    assert_eq!([0, 1].map(|v| expand(v, 1)), [0, 255]);
}

#[test]
fn quantize_inverts_expand_exactly() {
    for bits in 1..=8 {
        for v in 0..1u32 << bits {
            assert_eq!(quantize(expand(v, bits), bits), v, "{bits}-bit {v}");
        }
        // And quantize picks the nearest level for every 8-bit input.
        for x in 0..=255u8 {
            let q = quantize(x, bits);
            let err = |l: u32| (x as f32 / 255.0 - l as f32 / ((1 << bits) - 1) as f32).abs();
            for l in 0..1u32 << bits {
                assert!(err(q) <= err(l) + 1e-6, "{bits}-bit {x}: {q} vs {l}");
            }
        }
    }
}

#[test]
fn every_format_has_a_consistent_plan() {
    for &(name, f) in Format::ALL {
        let p = plan(f, &BC);
        if matches!(f, Format::Unknown | Format::Index16 | Format::Index32 | Format::Null) {
            assert!(p.is_none(), "{name}");
            continue;
        }
        let p = p.unwrap_or_else(|| panic!("{name} has no plan"));
        assert_eq!(p.gpu.is_depth(), f.is_depth(), "{name}");
        assert_eq!(p.gpu.has_stencil(), f.has_stencil(), "{name}");
        if p.renderable {
            assert!(p.gpu.is_renderable(), "{name}");
        }
        // Copy-through plans must keep the row size, or rows would be cut or overread.
        if p.conversion == Conversion::None {
            for w in [1, 3, 4, 17] {
                assert_eq!(Some(p.gpu.row_bytes(w)), f.row_bytes(w), "{name} width {w}");
            }
        }
        // Without BC support nothing compressed is chosen.
        let q = plan(f, &NO_BC).unwrap();
        assert!(!q.gpu.is_compressed(), "{name}");
        assert_eq!(q.swizzle, p.swizzle, "{name}: swizzle must not depend on the BC path");
    }
}

#[test]
fn render_target_formats() {
    for f in [
        Format::A8R8G8B8,
        Format::X8R8G8B8,
        Format::R5G6B5,
        Format::A1R5G5B5,
        Format::X1R5G5B5,
        Format::A4R4G4B4,
        Format::X4R4G4B4,
        Format::A8B8G8R8,
        Format::X8B8G8R8,
        Format::A2R10G10B10,
        Format::A2B10G10R10,
        Format::G16R16,
        Format::A16B16G16R16,
        Format::R16F,
        Format::G16R16F,
        Format::A16B16G16R16F,
        Format::R32F,
        Format::G32R32F,
        Format::A32B32G32R32F,
        Format::D16,
        Format::D24S8,
        Format::D24X8,
        Format::D32FLockable,
        Format::Intz,
    ] {
        assert!(pl(f).renderable, "{f:?}");
    }
    for f in [Format::L8, Format::Dxt1, Format::P8, Format::V8U8] {
        assert!(!pl(f).renderable, "{f:?}");
    }
}

#[test]
fn storage_and_swizzles() {
    use Component::*;
    let s = |f| {
        let p = pl(f);
        (p.gpu, p.swizzle.0)
    };
    assert_eq!(s(Format::A8R8G8B8), (GpuFormat::Bgra8Unorm, [R, G, B, A]));
    assert_eq!(s(Format::X8R8G8B8), (GpuFormat::Bgra8Unorm, [R, G, B, One]));
    assert_eq!(s(Format::A8B8G8R8), (GpuFormat::Rgba8Unorm, [R, G, B, A]));
    assert_eq!(s(Format::R5G6B5), (GpuFormat::Rgba8Unorm, [R, G, B, One]));
    assert_eq!(s(Format::L8), (GpuFormat::R8Unorm, [R, R, R, One]));
    assert_eq!(s(Format::A8L8), (GpuFormat::Rg8Unorm, [R, R, R, G]));
    assert_eq!(s(Format::A4L4), (GpuFormat::Rg8Unorm, [R, R, R, G]));
    assert_eq!(s(Format::A8), (GpuFormat::R8Unorm, [Zero, Zero, Zero, R]));
    assert_eq!(s(Format::L16), (GpuFormat::R16Float, [R, R, R, One]));
    assert_eq!(s(Format::V8U8), (GpuFormat::Rg8Snorm, [R, G, One, One]));
    assert_eq!(s(Format::Q8W8V8U8), (GpuFormat::Rgba8Snorm, [R, G, B, A]));
    assert_eq!(s(Format::R16F), (GpuFormat::R16Float, [R, One, One, One]));
    assert_eq!(s(Format::R32F), (GpuFormat::R32Float, [R, One, One, One]));
    assert_eq!(s(Format::G16R16F), (GpuFormat::Rg16Float, [R, G, One, One]));
    assert_eq!(s(Format::G16R16), (GpuFormat::Rg16Float, [R, G, One, One]));
    assert_eq!(s(Format::A2B10G10R10), (GpuFormat::Rgb10a2Unorm, [R, G, B, A]));
    assert_eq!(s(Format::Dxt1), (GpuFormat::Bc1RgbaUnorm, [R, G, B, A]));
    assert_eq!(s(Format::Dxt2), (GpuFormat::Bc2RgbaUnorm, [R, G, B, A]));
    assert_eq!(s(Format::Dxt4), (GpuFormat::Bc3RgbaUnorm, [R, G, B, A]));
    assert_eq!(s(Format::Ati1), (GpuFormat::Bc4RUnorm, [R, R, R, R]));
    assert_eq!(s(Format::Ati2), (GpuFormat::Bc5RgUnorm, [G, R, One, One]));
    assert_eq!(s(Format::D24S8).0, GpuFormat::Depth24PlusStencil8);
    assert_eq!(s(Format::D24X8).0, GpuFormat::Depth24Plus);
    assert_eq!(s(Format::D16).0, GpuFormat::Depth16Unorm);
    assert_eq!(s(Format::D32).0, GpuFormat::Depth32Float);
    assert_eq!(plan(Format::Dxt5, &NO_BC).unwrap().gpu, GpuFormat::Rgba8Unorm);
    assert_eq!(plan(Format::Ati1, &NO_BC).unwrap().gpu, GpuFormat::R8Unorm);
    assert_eq!(plan(Format::Ati2, &NO_BC).unwrap().gpu, GpuFormat::Rg8Unorm);
}

#[test]
fn swizzle_helpers() {
    let s = pl(Format::A8L8).swizzle;
    assert_eq!(s.apply([10u8, 20, 30, 40], 0, 255), [10, 10, 10, 20]);
    assert_eq!(Swizzle::ALPHA.apply([0.5f32, 0.25, 0.0, 0.0], 0.0, 1.0), [0.0, 0.0, 0.0, 0.5]);
    assert_eq!(Swizzle::IDENTITY.wgsl("t"), "t");
    assert_eq!(Swizzle::LUMINANCE.wgsl("t"), "vec4<f32>(t.r, t.r, t.r, 1.0)");
    assert_eq!(Swizzle::ALPHA.wgsl("t"), "vec4<f32>(0.0, 0.0, 0.0, t.r)");
    assert!(Swizzle::IDENTITY.is_identity() && !Swizzle::OPAQUE.is_identity());
}

#[test]
fn plan_helpers() {
    let p = pl(Format::Dxt1);
    assert_eq!(p.gpu_extent(6, 6), (8, 8));
    assert_eq!(p.gpu_extent(2, 1), (4, 4));
    assert_eq!(pl(Format::A8R8G8B8).gpu_extent(6, 6), (6, 6));
    assert!(pl(Format::X8R8G8B8).alpha_is_one());
    assert!(pl(Format::R5G6B5).alpha_is_one());
    assert!(!pl(Format::A8R8G8B8).alpha_is_one());
    assert!(pl(Format::G16R16).needs_output_saturate());
    assert!(!pl(Format::G16R16F).needs_output_saturate());
    assert!(pl(Format::P8).uses_palette() && pl(Format::A8P8).uses_palette());
    assert!(!pl(Format::L8).uses_palette());
}

#[test]
fn gpu_format_properties() {
    use GpuFormat::*;
    let all = [
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
    ];
    let mut names: Vec<_> = all.iter().map(|f| f.webgpu_name()).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), all.len());
    for f in all {
        if let Some(s) = f.srgb() {
            assert!(s.is_srgb());
            assert_eq!(s.linear(), f.linear());
            assert_eq!(s.bytes_per_block(), f.bytes_per_block());
        } else {
            assert!(!f.is_srgb());
        }
        assert!(f.webgpu_name().contains("srgb") == f.is_srgb());
    }
    assert_eq!(Bc1RgbaUnorm.row_bytes(5), 16);
    assert_eq!(Bc3RgbaUnorm.block_rows(5), 2);
    assert_eq!(Rgba16Float.row_bytes(3), 24);
    assert!(!R32Float.is_filterable_float() && R16Float.is_filterable_float());
    assert!(!Depth16Unorm.is_filterable_float());
    assert!(!Rgba8Snorm.is_renderable() && Rgb10a2Unorm.is_renderable());
}

#[test]
fn packed_16bit_to_rgba8() {
    assert_eq!(
        up(Format::R5G6B5, &[0xf800, 0x07e0, 0x001f, 0x8410], 2),
        [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 132, 130, 132, 255]
    );
    assert_eq!(up(Format::A1R5G5B5, &[0x8000 | 0x7c00, 0x03e0], 2), [255, 0, 0, 255, 0, 255, 0, 0]);
    assert_eq!(up(Format::X1R5G5B5, &[0x001f], 2), [0, 0, 255, 255]);
    assert_eq!(up(Format::A4R4G4B4, &[0x1234], 2), [0x22, 0x33, 0x44, 0x11]);
    assert_eq!(up(Format::X4R4G4B4, &[0x0f00], 2), [255, 0, 0, 255]);
    assert_eq!(up(Format::A8R3G3B2, &[0x80e3], 2), [255, 0, 255, 0x80]);
}

#[test]
fn other_conversions() {
    assert_eq!(up(Format::R8G8B8, &[0x112233, 0xaabbcc], 3), [0x11, 0x22, 0x33, 255, 0xaa, 0xbb, 0xcc, 255]);
    assert_eq!(up(Format::R3G3B2, &[7 << 5 | 3, 1 << 5 | 2 << 2 | 1], 1), [255, 0, 255, 255, 36, 73, 85, 255]);
    assert_eq!(up(Format::A4L4, &[0xf3], 1), [0x33, 0xff]);
    // A2R10G10B10 with R = 0x3ff, G = 0, B = 1, A = 2 becomes rgb10a2 with R in the low bits.
    let v = 2 << 30 | 0x3ff << 20 | 1;
    assert_eq!(up(Format::A2R10G10B10, &[v], 4), (2u32 << 30 | 1 << 20 | 0x3ff).to_le_bytes());
    // Native formats are copied.
    assert_eq!(up(Format::A8R8G8B8, &[0x11223344], 4), [0x44, 0x33, 0x22, 0x11]);
}

#[test]
fn palettes() {
    let mut pal = [[0u8; 4]; 256];
    pal[1] = [10, 20, 30, 40];
    pal[200] = [1, 2, 3, 4];
    let p = convert(&pl(Format::P8), &[1, 200, 0], 3, 1, 3, Some(&pal));
    assert_eq!(p, [10, 20, 30, 40, 1, 2, 3, 4, 0, 0, 0, 0]);
    let p = convert(&pl(Format::A8P8), &[1, 0x99], 1, 1, 2, Some(&pal));
    assert_eq!(p, [10, 20, 30, 0x99]);
    // No palette: opaque black.
    assert_eq!(convert(&pl(Format::P8), &[7], 1, 1, 1, None), [0, 0, 0, 255]);
}

#[test]
fn signed_formats() {
    // L6V5U5: U = -16 (clamps to -1), V = 15 (+1), L = 63 (1.0).
    let v = 63 << 10 | 15 << 5 | 0b10000;
    assert_eq!(up(Format::L6V5U5, &[v], 2), [(-127i8) as u8, 127, 127, 127]);
    // U = 1, V = -1, L = 0.
    let v = 0b11111 << 5 | 1;
    assert_eq!(up(Format::L6V5U5, &[v], 2), [8, (-8i8) as u8, 0, 127]);
    // U = -15 is -15/16, not -1.0 (test_signed_formats).
    assert_eq!(up(Format::L6V5U5, &[0b10001], 2), [(-119i8) as u8, 0, 0, 127]);
    // X8L8V8U8: U, V copied; L 255 -> 127, 128 -> 64.
    assert_eq!(up(Format::X8L8V8U8, &[0x00ff_8001, 0x0080_0000], 4), [1, 0x80, 127, 127, 0, 0, 64, 127]);
    // A2W10V10U10: U = 511 (1.0), V = -512 (-1.0), W = 0, A = 3 (1.0).
    let v = 3 << 30 | 0x200 << 10 | 511;
    let out = up(Format::A2W10V10U10, &[v], 4);
    let h: Vec<f32> = out.chunks(2).map(|c| f16_to_f32(u16::from_le_bytes([c[0], c[1]]))).collect();
    assert_eq!(h, [1.0, -1.0, 0.0, 1.0]);
    // V16U16 to float16.
    let out = up(Format::V16U16, &[0x8000_7fff], 4);
    let h: Vec<f32> = out.chunks(2).map(|c| f16_to_f32(u16::from_le_bytes([c[0], c[1]]))).collect();
    assert_eq!(h, [1.0, -1.0]);
}

#[test]
fn unorm16_to_half() {
    let src: Vec<u8> = [0u16, 65535, 32768, 1000].iter().flat_map(|v| v.to_le_bytes()).collect();
    let p = pl(Format::A16B16G16R16);
    let out = convert(&p, &src, 1, 1, 8, None);
    let h: Vec<f32> = out.chunks(2).map(|c| f16_to_f32(u16::from_le_bytes([c[0], c[1]]))).collect();
    assert_eq!(h[0], 0.0);
    assert_eq!(h[1], 1.0);
    assert!((h[2] - 0.5).abs() < 1e-3);
    assert!((h[3] - 1000.0 / 65535.0).abs() < 1e-5);
    // L16 is one channel per texel.
    assert_eq!(convert(&pl(Format::L16), &src, 4, 1, 8, None).len(), 8);
}

#[test]
fn pitch_is_repacked_and_short_sources_pad() {
    // 2x2 L8 with a 4-byte source pitch.
    let src = [1, 2, 0xee, 0xee, 3, 4, 0xee, 0xee];
    assert_eq!(convert(&pl(Format::L8), &src, 2, 2, 4, None), [1, 2, 3, 4]);
    // Converted formats too.
    let src = [0x1f, 0, 0xee, 0xee, 0, 0xf8];
    assert_eq!(convert(&pl(Format::R5G6B5), &src, 1, 2, 4, None), [0, 0, 255, 255, 255, 0, 0, 255]);
    // A source missing its last row reads zeros.
    assert_eq!(convert(&pl(Format::L8), &[1, 2], 2, 2, 4, None), [1, 2, 0, 0]);
    assert!(convert(&pl(Format::D24S8), &[0; 16], 2, 2, 8, None).is_empty());
}

#[test]
fn block_compressed_upload() {
    // 6x5 DXT1: 2x2 blocks; source pitch 20, output tight 16 bytes per block row.
    let mut src = vec![0u8; 40];
    for (i, b) in src.iter_mut().enumerate() {
        *b = if i % 20 < 16 { i as u8 } else { 0xee };
    }
    let out = convert(&pl(Format::Dxt1), &src, 6, 5, 20, None);
    let want: Vec<u8> = (0..16).chain(20..36).collect();
    assert_eq!(out, want);
    // Decoded on the CPU when BC is unavailable.
    let red = [0x00, 0xf8, 0, 0, 0, 0, 0, 0];
    let p = plan(Format::Dxt1, &NO_BC).unwrap();
    let out = convert(&p, &red, 2, 2, 8, None);
    assert_eq!(out, [255, 0, 0, 255].repeat(4));
    // ATI1 decodes to one channel, ATI2 to two (first block in red; the swizzle swaps them for sampling).
    let ati1 = [77, 0, 0, 0, 0, 0, 0, 0];
    assert_eq!(convert(&plan(Format::Ati1, &NO_BC).unwrap(), &ati1, 3, 1, 8, None), [77, 77, 77]);
    let ati2 = [10, 0, 0, 0, 0, 0, 0, 0, 20, 0, 0, 0, 0, 0, 0, 0];
    let p = plan(Format::Ati2, &NO_BC).unwrap();
    let out = convert(&p, &ati2, 1, 1, 16, None);
    assert_eq!(out, [10, 20]);
    assert_eq!(p.swizzle.apply([out[0], out[1], 0, 0], 0, 255), [20, 10, 255, 255]);
}

/// Every value of a 16-bit format survives upload and readback unchanged.
fn round_trip_16(f: Format, mask: u16) {
    let p = pl(f);
    let src: Vec<u8> = (0..=u16::MAX).flat_map(|v| v.to_le_bytes()).collect();
    let gpu = convert(&p, &src, 256, 256, 512, None);
    let back = convert_back(f, &p, &gpu, 256, 256, 256 * 4, 512).unwrap();
    for (i, c) in back.chunks(2).enumerate() {
        let want = i as u16 | !mask;
        assert_eq!(u16::from_le_bytes([c[0], c[1]]), want, "{f:?} {i:#06x}");
    }
}

#[test]
fn readback_round_trips_exactly() {
    round_trip_16(Format::R5G6B5, 0xffff);
    round_trip_16(Format::A1R5G5B5, 0xffff);
    round_trip_16(Format::X1R5G5B5, 0x7fff); // X bit reads back as 1
    round_trip_16(Format::A4R4G4B4, 0xffff);
    round_trip_16(Format::X4R4G4B4, 0x0fff);
    round_trip_16(Format::A8R3G3B2, 0xffff);

    let src: Vec<u8> = (0..=255).collect();
    for f in [Format::R3G3B2, Format::A4L4] {
        let p = pl(f);
        let gpu = convert(&p, &src, 256, 1, 256, None);
        assert_eq!(convert_back(f, &p, &gpu, 256, 1, 1024, 256).unwrap(), src, "{f:?}");
    }

    let words: Vec<u32> = (0..1000u32).map(|i| i.wrapping_mul(2_654_435_761)).collect();
    let src: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    for f in [Format::A2R10G10B10, Format::A8R8G8B8, Format::A2B10G10R10, Format::R32F] {
        let p = pl(f);
        let gpu = convert(&p, &src, 1000, 1, 4000, None);
        assert_eq!(convert_back(f, &p, &gpu, 1000, 1, 4000, 4000).unwrap(), src, "{f:?}");
    }
    let p = pl(Format::R8G8B8);
    let src: Vec<u8> = (0..30).collect();
    let gpu = convert(&p, &src, 10, 1, 30, None);
    assert_eq!(convert_back(Format::R8G8B8, &p, &gpu, 10, 1, 40, 30).unwrap(), src);
}

#[test]
fn readback_of_unorm16_through_half_is_close() {
    let p = pl(Format::G16R16);
    let src: Vec<u8> = (0..=u16::MAX).step_by(7).flat_map(|v| v.to_le_bytes()).collect();
    let n = src.len() as u32 / 4;
    let gpu = convert(&p, &src, n, 1, n * 4, None);
    let back = convert_back(Format::G16R16, &p, &gpu, n, 1, n * 4, n * 4).unwrap();
    for (a, b) in src.chunks(2).zip(back.chunks(2)) {
        let (a, b) = (u16::from_le_bytes([a[0], a[1]]), u16::from_le_bytes([b[0], b[1]]));
        // Half a float16 ulp at 1.0 is 2^-12 of full scale: 16 steps of 65535.
        assert!(a.abs_diff(b) <= 16, "{a} -> {b}");
    }
    let p = pl(Format::V16U16);
    let src: Vec<u8> = [0x7fffu16, 0x8001, 0, 0x8000].iter().flat_map(|v| v.to_le_bytes()).collect();
    let gpu = convert(&p, &src, 2, 1, 8, None);
    let back = convert_back(Format::V16U16, &p, &gpu, 2, 1, 8, 8).unwrap();
    // -32768 and -32767 are both -1.0.
    assert_eq!(back, [0xff, 0x7f, 0x01, 0x80, 0, 0, 0x01, 0x80]);
}

#[test]
fn readback_pitches_and_x_channels() {
    let p = pl(Format::X8R8G8B8);
    // Two rows of one texel, GPU pitch 256, destination pitch 8.
    let mut gpu = vec![0u8; 260];
    gpu[..4].copy_from_slice(&[1, 2, 3, 0]);
    gpu[256..260].copy_from_slice(&[4, 5, 6, 7]);
    let back = convert_back(Format::X8R8G8B8, &p, &gpu, 1, 2, 256, 8).unwrap();
    assert_eq!(back, [1, 2, 3, 0xff, 0, 0, 0, 0, 4, 5, 6, 0xff, 0, 0, 0, 0]);
    // A8R8G8B8 keeps alpha.
    let back = convert_back(Format::A8R8G8B8, &pl(Format::A8R8G8B8), &gpu, 1, 2, 256, 4).unwrap();
    assert_eq!(back, [1, 2, 3, 0, 4, 5, 6, 7]);
    // Too short, pitch too small.
    assert!(convert_back(Format::X8R8G8B8, &p, &gpu[..259], 1, 2, 256, 4).is_none());
    assert!(convert_back(Format::X8R8G8B8, &p, &gpu, 1, 2, 256, 3).is_none());
    assert!(convert_back(Format::X8R8G8B8, &p, &gpu, 1, 2, 2, 4).is_none());
}

#[test]
fn readback_unsupported_and_depth() {
    let gpu = [0u8; 64];
    assert!(convert_back(Format::P8, &pl(Format::P8), &gpu, 1, 1, 4, 1).is_none());
    assert!(convert_back(Format::D24S8, &pl(Format::D24S8), &gpu, 1, 1, 4, 4).is_none());
    assert!(convert_back(Format::L6V5U5, &pl(Format::L6V5U5), &gpu, 1, 1, 4, 2).is_none());
    let dxt = plan(Format::Dxt1, &NO_BC).unwrap();
    assert!(convert_back(Format::Dxt1, &dxt, &gpu, 4, 4, 16, 8).is_none());
    // Native BC data copies back as blocks.
    let blocks: Vec<u8> = (0..16).collect();
    assert_eq!(convert_back(Format::Dxt1, &pl(Format::Dxt1), &blocks, 8, 4, 16, 16).unwrap(), blocks);
    // Lockable depth copies.
    let d: Vec<u8> = (0..8).collect();
    assert_eq!(convert_back(Format::D16Lockable, &pl(Format::D16Lockable), &d, 2, 2, 4, 4).unwrap(), d);
    assert_eq!(convert_back(Format::D32FLockable, &pl(Format::D32FLockable), &d, 1, 2, 4, 4).unwrap(), d);
}
