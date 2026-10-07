//! IEEE 754 binary16 conversion.
//!
//! Core WebGPU has no 16-bit normalized texture formats, so `L16`, `G16R16`, `V16U16` and friends are stored as
//! float16 and converted here on upload and readback. Kept local rather than pulling in a crate: two functions.

/// `x` rounded to the nearest float16 (ties to even). Overflow becomes infinity, NaN stays NaN.
pub fn f32_to_f16(x: f32) -> u16 {
    let bits = x.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32;
    let man = bits & 0x7f_ffff;
    if exp == 0xff {
        // Keep NaNs quiet and non-zero whatever payload bits survive.
        return sign | 0x7c00 | if man != 0 { 0x200 | (man >> 13) as u16 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        // Subnormal (or zero) result: shift the full significand down into the 10-bit field.
        if e < -10 {
            return sign;
        }
        let m = man | 0x80_0000;
        let shift = (14 - e) as u32;
        let half_m = m >> shift;
        let rem = m & ((1 << shift) - 1);
        let halfway = 1 << (shift - 1);
        let up = rem > halfway || (rem == halfway && half_m & 1 != 0);
        return sign | (half_m + up as u32) as u16;
    }
    let half = (e as u32) << 10 | (man >> 13);
    let rem = man & 0x1fff;
    let up = rem > 0x1000 || (rem == 0x1000 && half & 1 != 0);
    // A carry out of the mantissa correctly bumps the exponent (and reaches infinity at the top).
    sign | (half + up as u32) as u16
}

/// The exact `f32` value of float16 `h`.
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h as u32) & 0x8000) << 16;
    let exp = ((h >> 10) & 0x1f) as u32;
    let man = (h & 0x3ff) as u32;
    match exp {
        0 => {
            let v = man as f32 * (1.0 / 16_777_216.0); // 2^-24
            if sign != 0 {
                -v
            } else {
                v
            }
        }
        31 => f32::from_bits(sign | 0x7f80_0000 | man << 13),
        _ => f32::from_bits(sign | (exp + 112) << 23 | man << 13),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_half_round_trips() {
        for h in 0..=u16::MAX {
            let f = f16_to_f32(h);
            if f.is_nan() {
                assert!(f16_to_f32(f32_to_f16(f)).is_nan());
            } else {
                assert_eq!(f32_to_f16(f), h, "{h:#06x} -> {f}");
            }
        }
    }

    #[test]
    fn known_values() {
        assert_eq!(f32_to_f16(0.0), 0);
        assert_eq!(f32_to_f16(-0.0), 0x8000);
        assert_eq!(f32_to_f16(1.0), 0x3c00);
        assert_eq!(f32_to_f16(-2.0), 0xc000);
        assert_eq!(f32_to_f16(65504.0), 0x7bff);
        assert_eq!(f32_to_f16(65520.0), 0x7c00); // rounds up past the largest finite half
        assert_eq!(f32_to_f16(1e10), 0x7c00);
        assert_eq!(f32_to_f16(f32::INFINITY), 0x7c00);
        assert_eq!(f32_to_f16(5.960_464_5e-8), 1); // smallest subnormal
        assert_eq!(f32_to_f16(2.0e-8), 0); // below half of it
        assert_eq!(f32_to_f16(1.0 / 3.0), 0x3555);
    }

    #[test]
    fn ties_go_to_even() {
        // 1 + 2^-11 lies exactly between 1.0 (even) and the next half: rounds down.
        assert_eq!(f32_to_f16(1.0 + 1.0 / 2048.0), 0x3c00);
        // 1 + 3 * 2^-11 lies between 0x3c01 (odd) and 0x3c02: rounds up.
        assert_eq!(f32_to_f16(1.0 + 3.0 / 2048.0), 0x3c02);
    }
}
