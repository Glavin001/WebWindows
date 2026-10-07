//! Comparison of x87/SSE state (FXSAVE images).

use crate::case::unhex;

/// Differences between two FXSAVE images. x87 registers are compared after
/// conversion to f64, which is the precision the translator keeps.
pub fn compare_fx(want: &str, got: &str) -> Vec<String> {
    let (w, g) = (unhex(want), unhex(got));
    let mut d = vec![];
    let u16at = |b: &[u8], o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
    if u16at(&w, 0) != u16at(&g, 0) {
        d.push(format!("fcw: want {:#06x} got {:#06x}", u16at(&w, 0), u16at(&g, 0)));
    }
    // Status word: top and condition codes C0-C3 (exception flags are not
    // tracked).
    let sw_mask = 0x4700 | 0x3800;
    if u16at(&w, 2) & sw_mask != u16at(&g, 2) & sw_mask {
        d.push(format!("fsw: want {:#06x} got {:#06x}", u16at(&w, 2) & sw_mask, u16at(&g, 2) & sw_mask));
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
        if a.to_bits() != b.to_bits() && !(a.is_nan() && b.is_nan()) {
            d.push(format!("st{i}: want {a:e} got {b:e}"));
        }
    }
    let mx = |b: &[u8]| u32::from_le_bytes(b[24..28].try_into().unwrap()) & 0xffc0;
    if mx(&w) != mx(&g) {
        d.push(format!("mxcsr: want {:#x} got {:#x}", mx(&w), mx(&g)));
    }
    for i in 0..8 {
        let (a, b) = (&w[160 + i * 16..176 + i * 16], &g[160 + i * 16..176 + i * 16]);
        if a != b {
            d.push(format!("xmm{i}: want {} got {}", crate::case::hex(a), crate::case::hex(b)));
        }
    }
    d
}
