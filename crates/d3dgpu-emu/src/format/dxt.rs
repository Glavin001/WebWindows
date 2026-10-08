//! CPU decoders for the block-compressed formats: BC1 (`DXT1`), BC2 (`DXT2`/`DXT3`), BC3 (`DXT4`/`DXT5`),
//! BC4 (`ATI1`) and BC5 (`ATI2`), for devices without WebGPU's `texture-compression-bc`.
//!
//! Interpolated values are computed on the 8-bit expanded endpoints and rounded to nearest: `(2a + b + 1) / 3`
//! for BC1's thirds, `(a + b + 1) / 2` for its 3-colour midpoint, `(k·a + (7-k)·b + 3) / 7` and `/ 5` for the
//! alpha and BC4/BC5 ramps. The D3D10 specification only bounds the error of these, and hardware differs by one
//! here and there (NVIDIA interpolates on the 5/6-bit values, some decoders truncate), so do not expect
//! bit-exactness against any particular GPU.
//!
//! `DXT2` and `DXT4` are the premultiplied-alpha variants of `DXT3` and `DXT5`. The blocks are identical and
//! Direct3D 9 samples them as stored (it does not divide by alpha), so they decode as BC2 and BC3.

/// A block-compressed layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Bc {
    Bc1,
    Bc2,
    Bc3,
    Bc4,
    Bc5,
}

impl Bc {
    /// Bytes in one 4x4 block.
    pub fn block_bytes(self) -> usize {
        match self {
            Bc::Bc1 | Bc::Bc4 => 8,
            Bc::Bc2 | Bc::Bc3 | Bc::Bc5 => 16,
        }
    }
}

/// RGBA8 texels of one 4x4 block, row-major.
pub type Block = [[u8; 4]; 16];

fn expand565(c: u16) -> [u32; 3] {
    let r = (c >> 11) as u32 & 31;
    let g = (c >> 5) as u32 & 63;
    let b = c as u32 & 31;
    [(r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2)]
}

/// The colour half of a BC1/BC2/BC3 block. `allow_3_colour` is BC1's rule: `c0 <= c1` selects three colours plus
/// transparent black. BC2 and BC3 always use four colours.
fn decode_color(block: &[u8], allow_3_colour: bool) -> Block {
    let c0 = u16::from_le_bytes([block[0], block[1]]);
    let c1 = u16::from_le_bytes([block[2], block[3]]);
    let (a, b) = (expand565(c0), expand565(c1));
    let mix = |f: fn(u32, u32) -> u32| [f(a[0], b[0]) as u8, f(a[1], b[1]) as u8, f(a[2], b[2]) as u8, 255];
    let palette = if c0 > c1 || !allow_3_colour {
        [mix(|a, _| a), mix(|_, b| b), mix(|a, b| (2 * a + b + 1) / 3), mix(|a, b| (a + 2 * b + 1) / 3)]
    } else {
        [mix(|a, _| a), mix(|_, b| b), mix(|a, b| (a + b).div_ceil(2)), [0, 0, 0, 0]]
    };
    let bits = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
    core::array::from_fn(|t| palette[(bits >> (2 * t)) as usize & 3])
}

/// A BC3 alpha / BC4 channel block: two endpoints and 3-bit indices.
fn decode_ramp(block: &[u8]) -> [u8; 16] {
    let (a0, a1) = (block[0] as u32, block[1] as u32);
    let mut ramp = [0u32; 8];
    ramp[0] = a0;
    ramp[1] = a1;
    if a0 > a1 {
        for k in 1..7 {
            ramp[k as usize + 1] = ((7 - k) * a0 + k * a1 + 3) / 7;
        }
    } else {
        for k in 1..5 {
            ramp[k as usize + 1] = ((5 - k) * a0 + k * a1 + 2) / 5;
        }
        ramp[6] = 0;
        ramp[7] = 255;
    }
    let mut bits = [0u8; 8];
    bits[..6].copy_from_slice(&block[2..8]);
    let bits = u64::from_le_bytes(bits);
    core::array::from_fn(|t| ramp[(bits >> (3 * t)) as usize & 7] as u8)
}

/// BC1 / `DXT1`: four colours, or three plus transparent black (alpha 0) when `c0 <= c1`.
pub fn decode_bc1(block: &[u8; 8]) -> Block {
    decode_color(block, true)
}

/// BC2 / `DXT2`, `DXT3`: explicit 4-bit alpha, expanded by ×17.
pub fn decode_bc2(block: &[u8; 16]) -> Block {
    let mut out = decode_color(&block[8..], false);
    let alpha = u64::from_le_bytes(block[..8].try_into().unwrap());
    for (t, px) in out.iter_mut().enumerate() {
        px[3] = ((alpha >> (4 * t)) & 15) as u8 * 17;
    }
    out
}

/// BC3 / `DXT4`, `DXT5`: interpolated alpha.
pub fn decode_bc3(block: &[u8; 16]) -> Block {
    let mut out = decode_color(&block[8..], false);
    let alpha = decode_ramp(&block[..8]);
    for (px, a) in out.iter_mut().zip(alpha) {
        px[3] = a;
    }
    out
}

/// BC4 / `ATI1`: one channel, returned in red as `(r, 0, 0, 255)`.
pub fn decode_bc4(block: &[u8; 8]) -> Block {
    decode_ramp(block).map(|r| [r, 0, 0, 255])
}

/// BC5 / `ATI2`: two channels, the first block in red and the second in green, `(r, g, 0, 255)`. Direct3D's
/// `ATI2` channel order is applied by the sampling swizzle, not here.
pub fn decode_bc5(block: &[u8; 16]) -> Block {
    let r = decode_ramp(&block[..8]);
    let g = decode_ramp(&block[8..]);
    core::array::from_fn(|t| [r[t], g[t], 0, 255])
}

/// Decodes one block of `bc` from the start of `block` (which must hold [`Bc::block_bytes`]).
pub fn decode_block(bc: Bc, block: &[u8]) -> Block {
    match bc {
        Bc::Bc1 => decode_bc1(block[..8].try_into().unwrap()),
        Bc::Bc2 => decode_bc2(block[..16].try_into().unwrap()),
        Bc::Bc3 => decode_bc3(block[..16].try_into().unwrap()),
        Bc::Bc4 => decode_bc4(block[..8].try_into().unwrap()),
        Bc::Bc5 => decode_bc5(block[..16].try_into().unwrap()),
    }
}

/// Decodes a `width` x `height` surface to tightly packed RGBA8. `src_row_pitch` is the distance between rows of
/// blocks. Sizes that are not multiples of 4 are fine: the texels of edge blocks past the surface are dropped.
/// Blocks missing from a short `src` decode as zeros (black for colour, transparent where alpha is explicit).
pub fn decode(bc: Bc, src: &[u8], width: u32, height: u32, src_row_pitch: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let bb = bc.block_bytes();
    let mut out = vec![0u8; w * h * 4];
    let zero = [0u8; 16];
    for by in 0..h.div_ceil(4) {
        for bx in 0..w.div_ceil(4) {
            let at = by * src_row_pitch as usize + bx * bb;
            let block = src.get(at..at + bb).unwrap_or(&zero[..bb]);
            let texels = decode_block(bc, block);
            for y in 0..4.min(h - by * 4) {
                for x in 0..4.min(w - bx * 4) {
                    let o = ((by * 4 + y) * w + bx * 4 + x) * 4;
                    out[o..o + 4].copy_from_slice(&texels[y * 4 + x]);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bc1(c0: u16, c1: u16, indices: u32) -> [u8; 8] {
        let mut b = [0u8; 8];
        b[..2].copy_from_slice(&c0.to_le_bytes());
        b[2..4].copy_from_slice(&c1.to_le_bytes());
        b[4..].copy_from_slice(&indices.to_le_bytes());
        b
    }

    fn close(a: [u8; 4], b: [u8; 4]) -> bool {
        a.iter().zip(b).all(|(&x, y)| x.abs_diff(y) <= 1)
    }

    /// Every texel uses index `i`.
    fn all(i: u32) -> u32 {
        (0..16).fold(0, |acc, t| acc | i << (2 * t))
    }

    #[test]
    fn bc1_four_colour() {
        let (red, blue) = (0xf800, 0x001f);
        assert!(decode_bc1(&bc1(red, blue, all(0))).iter().all(|&p| p == [255, 0, 0, 255]));
        assert!(decode_bc1(&bc1(red, blue, all(1))).iter().all(|&p| p == [0, 0, 255, 255]));
        // Two thirds red, one third blue: 170 and 85 (exactly representable, so every rounding agrees).
        assert!(decode_bc1(&bc1(red, blue, all(2))).iter().all(|&p| close(p, [170, 0, 85, 255])));
        assert!(decode_bc1(&bc1(red, blue, all(3))).iter().all(|&p| close(p, [85, 0, 170, 255])));
        // A non-exact third: green 6-bit 1 -> 4, so (2*4 + 0) / 3 = 2.67 rounds to 3 (truncating decoders give 2).
        let p = decode_bc1(&bc1(0x0020, 0x0000, all(2)))[0];
        assert!(close(p, [0, 3, 0, 255]));
    }

    #[test]
    fn bc1_three_colour_and_transparent() {
        let (blue, red) = (0x001f, 0xf800); // c0 <= c1 selects 3-colour mode
        let b = decode_bc1(&bc1(blue, red, all(2)));
        assert!(close(b[0], [128, 0, 128, 255]));
        let t = decode_bc1(&bc1(blue, red, all(3)));
        assert_eq!(t[0], [0, 0, 0, 0]);
        // Equal endpoints are 3-colour mode too.
        assert_eq!(decode_bc1(&bc1(red, red, all(3)))[5], [0, 0, 0, 0]);
    }

    #[test]
    fn bc1_index_layout() {
        // Texel (x, y) uses bits 2*(4y + x): put index 1 at (1, 0) and (3, 2).
        let idx = 1 << 2 | 1 << (2 * 11);
        let b = decode_bc1(&bc1(0xffff, 0x0000, idx));
        for (t, p) in b.iter().enumerate() {
            let want = if t == 1 || t == 11 { [0, 0, 0, 255] } else { [255, 255, 255, 255] };
            assert_eq!(*p, want, "texel {t}");
        }
    }

    #[test]
    fn bc2_explicit_alpha_and_four_colour() {
        let mut b = [0u8; 16];
        // Alpha nibbles 0..15 in texel order.
        let alpha: u64 = (0..16).fold(0, |acc, t| acc | (t as u64) << (4 * t));
        b[..8].copy_from_slice(&alpha.to_le_bytes());
        // c0 < c1 would be 3-colour in BC1; BC2 must still interpolate.
        b[8..].copy_from_slice(&bc1(0x001f, 0xf800, all(3)));
        let out = decode_bc2(&b);
        for (t, p) in out.iter().enumerate() {
            assert_eq!(p[3], t as u8 * 17);
            assert!(close([p[0], p[1], p[2], 0], [170, 0, 85, 0]));
        }
    }

    fn ramp_block(a0: u8, a1: u8, idx: [u8; 16]) -> [u8; 8] {
        let bits = idx.iter().enumerate().fold(0u64, |acc, (t, &i)| acc | (i as u64) << (3 * t));
        let mut b = [0u8; 8];
        b[0] = a0;
        b[1] = a1;
        b[2..].copy_from_slice(&bits.to_le_bytes()[..6]);
        b
    }

    #[test]
    fn bc3_alpha_ramps() {
        let idx = core::array::from_fn(|t| (t % 8) as u8);
        let mut b = [0u8; 16];
        b[..8].copy_from_slice(&ramp_block(255, 0, idx));
        b[8..].copy_from_slice(&bc1(0xffff, 0xffff, 0));
        let a: Vec<u8> = decode_bc3(&b).iter().map(|p| p[3]).collect();
        // 8-value ramp from 255 to 0 in sevenths.
        let want = [255, 0, 219, 182, 146, 109, 73, 36];
        for t in 0..16 {
            assert!(a[t].abs_diff(want[t % 8]) <= 1, "{t}: {} vs {}", a[t], want[t % 8]);
        }
        // 6-value ramp plus explicit 0 and 255.
        b[..8].copy_from_slice(&ramp_block(0, 255, idx));
        let a: Vec<u8> = decode_bc3(&b).iter().map(|p| p[3]).collect();
        let want = [0, 255, 51, 102, 153, 204, 0, 255];
        for t in 0..16 {
            assert_eq!(a[t], want[t % 8]);
        }
        // The colour is white throughout.
        assert!(decode_bc3(&b).iter().all(|p| p[..3] == [255, 255, 255]));
    }

    #[test]
    fn bc4_bc5_channels() {
        let idx = [0; 16];
        let r = decode_bc4(&ramp_block(200, 10, idx));
        assert!(r.iter().all(|&p| p == [200, 0, 0, 255]));
        let mut b = [0u8; 16];
        b[..8].copy_from_slice(&ramp_block(200, 10, idx));
        b[8..].copy_from_slice(&ramp_block(30, 40, [1; 16]));
        assert!(decode_bc5(&b).iter().all(|&p| p == [200, 40, 0, 255]));
    }

    #[test]
    fn decode_handles_partial_blocks_and_pitch() {
        // 6x5 surface: 2x2 blocks; pitch padded to 24 bytes per block row.
        let red = bc1(0xf800, 0, 0);
        let green = bc1(0x07e0, 0, 0);
        let mut src = vec![0xeeu8; 48];
        src[0..8].copy_from_slice(&red);
        src[8..16].copy_from_slice(&green);
        src[24..32].copy_from_slice(&green);
        src[32..40].copy_from_slice(&red);
        let out = decode(Bc::Bc1, &src, 6, 5, 24);
        assert_eq!(out.len(), 6 * 5 * 4);
        let px = |x: usize, y: usize| <[u8; 4]>::try_from(&out[(y * 6 + x) * 4..][..4]).unwrap();
        assert_eq!(px(0, 0), [255, 0, 0, 255]);
        assert_eq!(px(3, 3), [255, 0, 0, 255]);
        assert_eq!(px(4, 0), [0, 255, 0, 255]);
        assert_eq!(px(5, 3), [0, 255, 0, 255]);
        assert_eq!(px(0, 4), [0, 255, 0, 255]);
        assert_eq!(px(5, 4), [255, 0, 0, 255]);
        // A short source decodes the missing blocks as zero blocks (c0 = c1 = 0: black).
        let out = decode(Bc::Bc1, &src[..8], 8, 4, 16);
        assert_eq!(&out[16..20], &[0, 0, 0, 255]);
    }
}
