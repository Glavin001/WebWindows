//! Comparison of x87/SSE state (FXSAVE images).

use crate::case::unhex;

/// Differences between two FXSAVE images. x87 registers are compared after
/// conversion to f64, which is the precision the translator keeps. x86-64
/// images (`x64`) also hold xmm8-15.
pub fn compare_fx(want: &str, got: &str, form: &str, simd: bool, x64: bool) -> Vec<String> {
    let nxmm = if x64 { 16 } else { 8 };
    if simd {
        return compare_simd(&unhex(want), &unhex(got), form, nxmm);
    }
    // Transcendental results may differ in the last bits from the CPU's.
    let approx = [
        "Fsin", "Fcos", "Fsincos", "Fptan", "Fpatan", "F2xm1", "Fyl2x", "Fyl2xp1",
    ]
    .iter()
    .any(|m| form.starts_with(m) && (form.len() == m.len() || form[m.len()..].starts_with('_')));
    // C1 is only meaningful for fxam (sign) and fprem (quotient bit).
    let c1 = if form.starts_with("Fxam") || form.starts_with("Fprem") {
        0x0200
    } else {
        0
    };
    let (w, g) = (unhex(want), unhex(got));
    let mut d = vec![];
    let u16at = |b: &[u8], o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
    if u16at(&w, 0) != u16at(&g, 0) {
        d.push(format!(
            "fcw: want {:#06x} got {:#06x}",
            u16at(&w, 0),
            u16at(&g, 0)
        ));
    }
    // Status word: top and condition codes C0-C3 (exception flags are not
    // tracked).
    let sw_mask = 0x4500 | 0x3800 | c1;
    if u16at(&w, 2) & sw_mask != u16at(&g, 2) & sw_mask {
        d.push(format!(
            "fsw: want {:#06x} got {:#06x}",
            u16at(&w, 2) & sw_mask,
            u16at(&g, 2) & sw_mask
        ));
    }
    if w[4] != g[4] {
        d.push(format!("ftw: want {:#04x} got {:#04x}", w[4], g[4]));
    }
    for i in 0..8 {
        if w[4] >> (((u16at(&w, 2) >> 11) as usize + i) & 7) & 1 == 0 {
            continue; // empty register
        }
        let a = wwt::fpu::f80_to_f64(&w[32 + i * 16..42 + i * 16]);
        let b = wwt::fpu::f80_to_f64(&g[32 + i * 16..42 + i * 16]);
        let close = approx && ((a - b).abs() <= a.abs().max(b.abs()) * 1e-14 || a == b);
        if a.to_bits() != b.to_bits() && !(a.is_nan() && b.is_nan()) && !close {
            d.push(format!("st{i}: want {a:e} got {b:e}"));
        }
    }
    let mx = |b: &[u8]| u32::from_le_bytes(b[24..28].try_into().unwrap()) & 0xffc0;
    if mx(&w) != mx(&g) {
        d.push(format!("mxcsr: want {:#x} got {:#x}", mx(&w), mx(&g)));
    }
    for i in 0..nxmm {
        let (a, b) = (
            &w[160 + i * 16..176 + i * 16],
            &g[160 + i * 16..176 + i * 16],
        );
        if a != b {
            d.push(format!(
                "xmm{i}: want {} got {}",
                crate::case::hex(a),
                crate::case::hex(b)
            ));
        }
    }
    d
}

/// MMX/SSE state: MMX registers (x87 mantissas), tag word, the first
/// `nxmm` XMM registers, MXCSR.
fn compare_simd(w: &[u8], g: &[u8], form: &str, nxmm: usize) -> Vec<String> {
    // rcp/rsqrt are 12-bit approximations on x86 (and differ between
    // vendors); the translator computes them exactly.
    let approx_lanes = if form.starts_with("Rcpps") || form.starts_with("Rsqrtps") {
        4
    } else if form.starts_with("Rcpss") || form.starts_with("Rsqrtss") {
        1
    } else {
        0
    };
    let mut d = vec![];
    if w[4] != g[4] {
        d.push(format!("ftw: want {:#04x} got {:#04x}", w[4], g[4]));
    }
    for i in 0..8 {
        let (a, b) = (&w[32 + i * 16..40 + i * 16], &g[32 + i * 16..40 + i * 16]);
        if a != b {
            d.push(format!(
                "mm{i}: want {} got {}",
                crate::case::hex(a),
                crate::case::hex(b)
            ));
        }
    }
    let mx = |b: &[u8]| u32::from_le_bytes(b[24..28].try_into().unwrap()) & 0xffc0;
    if mx(w) != mx(g) {
        d.push(format!("mxcsr: want {:#x} got {:#x}", mx(w), mx(g)));
    }
    for i in 0..nxmm {
        let (a, b) = (
            &w[160 + i * 16..176 + i * 16],
            &g[160 + i * 16..176 + i * 16],
        );
        if a == b {
            continue;
        }
        let close = approx_lanes > 0
            && (0..4).all(|k| {
                let x = &a[k * 4..k * 4 + 4];
                let y = &b[k * 4..k * 4 + 4];
                if k >= approx_lanes {
                    return x == y;
                }
                let fx = f32::from_le_bytes(x.try_into().unwrap());
                let fy = f32::from_le_bytes(y.try_into().unwrap());
                x == y
                    || (fx.is_nan() && fy.is_nan())
                    || (fx - fy).abs() <= fx.abs().max(fy.abs()) * 3.7e-4
                    || (fx.abs() < 1.2e-38 && fy.abs() < 1.2e-38)
            });
        if !close {
            d.push(format!(
                "xmm{i}: want {} got {}",
                crate::case::hex(a),
                crate::case::hex(b)
            ));
        }
    }
    d
}
