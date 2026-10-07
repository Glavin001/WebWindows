//! 80-bit extended precision conversions (x87 `tbyte` loads and stores).
//!
//! x87 registers are kept as f64 by default; these conversions are exact
//! from f64 to 80-bit and round to nearest-even from 80-bit to f64.

/// Converts a little-endian 80-bit extended value to f64.
pub fn f80_to_f64(b: &[u8]) -> f64 {
    let mant = u64::from_le_bytes(b[0..8].try_into().unwrap());
    let se = u16::from_le_bytes([b[8], b[9]]);
    let sign = (se >> 15) as u64;
    let exp = (se & 0x7fff) as i32;
    let sbit = sign << 63;
    if exp == 0x7fff {
        // Infinity (fraction zero) or NaN.
        let frac = mant << 1;
        if frac == 0 {
            return f64::from_bits(sbit | 0x7ff0_0000_0000_0000);
        }
        // Keep the top fraction bits; force quiet.
        return f64::from_bits(sbit | 0x7ff8_0000_0000_0000 | (frac >> 12));
    }
    if mant == 0 {
        return f64::from_bits(sbit);
    }
    // Normalize (handles denormals and unnormals).
    let lz = mant.leading_zeros() as i32;
    let m = mant << lz;
    let e = exp - lz - 16383 + if exp == 0 { 1 } else { 0 }; // unbiased exponent of bit 63
    let mut e64 = e + 1023;
    // Round the 64-bit significand to 53 bits (or fewer for subnormals).
    let mut shift = 11;
    if e64 <= 0 {
        shift += 1 - e64;
        e64 = 0;
    }
    if shift >= 64 + 2 {
        return f64::from_bits(sbit);
    }
    let (mut q, rem, half) = if shift >= 64 {
        (0u64, m as u128, 1u128 << (shift - 1))
    } else {
        (m >> shift, (m & ((1u64 << shift) - 1)) as u128, 1u128 << (shift - 1))
    };
    if rem > half || (rem == half && q & 1 == 1) {
        q += 1;
    }
    if e64 == 0 {
        // Subnormal (q may have rounded up into the normal range).
        return f64::from_bits(sbit | q);
    }
    if q >> 53 != 0 {
        q >>= 1;
        e64 += 1;
    }
    if e64 >= 0x7ff {
        return f64::from_bits(sbit | 0x7ff0_0000_0000_0000);
    }
    f64::from_bits(sbit | (e64 as u64) << 52 | (q & ((1u64 << 52) - 1)))
}

/// Converts f64 to a little-endian 80-bit extended value (exact).
pub fn f64_to_f80(v: f64) -> [u8; 10] {
    let bits = v.to_bits();
    let sign = (bits >> 63) as u16;
    let exp = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & ((1u64 << 52) - 1);
    let (e80, mant): (u16, u64) = if exp == 0x7ff {
        (0x7fff, (1u64 << 63) | (frac << 11))
    } else if exp == 0 {
        if frac == 0 {
            (0, 0)
        } else {
            // Subnormal f64: normalize.
            let lz = frac.leading_zeros() as i32 - 11;
            let m = frac << (lz + 11);
            ((1 - 1023 - lz + 16383) as u16, m)
        }
    } else {
        ((exp - 1023 + 16383) as u16, (1u64 << 63) | (frac << 11))
    };
    let mut out = [0u8; 10];
    out[0..8].copy_from_slice(&mant.to_le_bytes());
    out[8..10].copy_from_slice(&(e80 | sign << 15).to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for v in [0.0, -0.0, 1.0, -2.5, 1e300, 1e-300, 5e-324, f64::MAX, f64::MIN_POSITIVE, f64::INFINITY, f64::NEG_INFINITY, 3.141592653589793] {
            let b = f64_to_f80(v);
            assert_eq!(f80_to_f64(&b).to_bits(), v.to_bits(), "{v}");
        }
        assert!(f80_to_f64(&f64_to_f80(f64::NAN)).is_nan());
    }

    #[test]
    fn rounding() {
        // 1 + 2^-53 (exactly halfway) rounds to even = 1.0
        let mut b = [0u8; 10];
        let m: u64 = (1 << 63) | (1 << 10);
        b[0..8].copy_from_slice(&m.to_le_bytes());
        b[8..10].copy_from_slice(&16383u16.to_le_bytes());
        assert_eq!(f80_to_f64(&b), 1.0);
        // Slightly above halfway rounds up.
        let m: u64 = (1 << 63) | (1 << 10) | 1;
        b[0..8].copy_from_slice(&m.to_le_bytes());
        assert_eq!(f80_to_f64(&b), 1.0 + f64::EPSILON);
    }
}
