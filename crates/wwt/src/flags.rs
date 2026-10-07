//! Flag formulas over the lazy flag state.
//!
//! Each arithmetic flag and each condition code is described as a small
//! expression tree over `FR`, `FA`, `FB` and `FC` for a statically known flag
//! kind. The optimizer lowers `Cond`/`Eflags` reads to IR from these trees
//! when the kind is known at translation time (fusing `cmp`+`jcc` into one
//! comparison); the generic run-time helpers are generated from the same
//! trees, so both paths share one definition of x86 flag semantics.

use crate::abi::flags::{self as fl, *};
use crate::ir::{BinOp, Cc, UnOp};

#[derive(Debug, Clone, PartialEq)]
pub enum E {
    Fr,
    Fa,
    Fb,
    Fc,
    K(u32),
    Bin(BinOp, Box<E>, Box<E>),
    Un(UnOp, Box<E>),
}

use E::*;

fn bin(op: BinOp, a: E, b: E) -> E {
    // Constant folding keeps the generated code small.
    if let (K(x), K(y)) = (&a, &b) {
        let (x, y) = (*x, *y);
        let v = match op {
            BinOp::I32Add => Some(x.wrapping_add(y)),
            BinOp::I32Sub => Some(x.wrapping_sub(y)),
            BinOp::I32And => Some(x & y),
            BinOp::I32Or => Some(x | y),
            BinOp::I32Xor => Some(x ^ y),
            BinOp::I32Shl => Some(x.wrapping_shl(y)),
            BinOp::I32ShrU => Some(x.wrapping_shr(y)),
            BinOp::I32Eq => Some((x == y) as u32),
            BinOp::I32Ne => Some((x != y) as u32),
            _ => None,
        };
        if let Some(v) = v {
            return K(v);
        }
    }
    match (op, &a, &b) {
        (BinOp::I32And, K(0), _) | (BinOp::I32And, _, K(0)) => K(0),
        (BinOp::I32Or, K(0), _) | (BinOp::I32Xor, K(0), _) => b,
        (BinOp::I32Or, _, K(0))
        | (BinOp::I32Xor, _, K(0))
        | (BinOp::I32Shl, _, K(0))
        | (BinOp::I32ShrU, _, K(0)) => a,
        _ => Bin(op, Box::new(a), Box::new(b)),
    }
}

fn and(a: E, b: E) -> E {
    bin(BinOp::I32And, a, b)
}
fn or(a: E, b: E) -> E {
    bin(BinOp::I32Or, a, b)
}
fn xor(a: E, b: E) -> E {
    bin(BinOp::I32Xor, a, b)
}
fn shl(a: E, n: u32) -> E {
    bin(BinOp::I32Shl, a, K(n))
}
fn shr(a: E, n: u32) -> E {
    bin(BinOp::I32ShrU, a, K(n))
}
fn not1(a: E) -> E {
    match a {
        K(v) => K((v == 0) as u32),
        a => Un(UnOp::I32Eqz, Box::new(a)),
    }
}
fn bit(a: E, n: u32) -> E {
    and(shr(a, n), K(1))
}
fn sign(a: E, w: u32) -> E {
    bit(a, w - 1)
}
fn sext(a: E, w: u32) -> E {
    match w {
        8 => Un(UnOp::I32Extend8S, Box::new(a)),
        16 => Un(UnOp::I32Extend16S, Box::new(a)),
        _ => a,
    }
}
fn sign_min(w: u32) -> u32 {
    1u32 << (w - 1)
}

/// Carry flag for a statically known kind.
pub fn cf(kind: u32) -> E {
    let w = width_of(kind);
    match op_of(kind) {
        ADD => bin(BinOp::I32LtU, Fr, Fa),
        ADC => or(
            bin(BinOp::I32LtU, Fr, Fa),
            and(Fc, bin(BinOp::I32Eq, Fr, Fa)),
        ),
        SUB => bin(BinOp::I32LtU, Fa, Fb),
        SBB => or(
            bin(BinOp::I32LtU, Fa, Fb),
            and(Fc, bin(BinOp::I32Eq, Fa, Fb)),
        ),
        LOGIC => K(0),
        INC | DEC => Fc,
        NEG => bin(BinOp::I32Ne, Fa, K(0)),
        SHL => and(
            bin(
                BinOp::I32ShrU,
                bin(BinOp::I32Shl, Fa, bin(BinOp::I32Sub, Fb, K(1))),
                K(w - 1),
            ),
            K(1),
        ),
        SHR => and(
            bin(BinOp::I32ShrU, Fa, bin(BinOp::I32Sub, Fb, K(1))),
            K(1),
        ),
        SAR => and(
            bin(BinOp::I32ShrS, sext(Fa, w), bin(BinOp::I32Sub, Fb, K(1))),
            K(1),
        ),
        MUL => Fb,
        _ => and(Fr, K(1)),
    }
}

pub fn zf(kind: u32) -> E {
    match op_of(kind) {
        EXPLICIT => bit(Fr, 6),
        _ => not1(Fr),
    }
}

pub fn sf(kind: u32) -> E {
    match op_of(kind) {
        EXPLICIT => bit(Fr, 7),
        _ => sign(Fr, width_of(kind)),
    }
}

pub fn pf(kind: u32) -> E {
    match op_of(kind) {
        EXPLICIT => bit(Fr, 2),
        _ => not1(and(
            Un(UnOp::I32Popcnt, Box::new(and(Fr, K(0xff)))),
            K(1),
        )),
    }
}

pub fn af(kind: u32) -> E {
    match op_of(kind) {
        ADD | ADC | SUB | SBB => bit(xor(xor(Fa, Fb), Fr), 4),
        INC => not1(and(Fr, K(0xf))),
        DEC => bin(BinOp::I32Eq, and(Fr, K(0xf)), K(0xf)),
        NEG => bit(xor(Fa, Fr), 4),
        EXPLICIT => bit(Fr, 4),
        _ => K(0),
    }
}

pub fn of(kind: u32) -> E {
    let w = width_of(kind);
    match op_of(kind) {
        ADD | ADC => sign(and(xor(Fa, Fr), xor(Fb, Fr)), w),
        SUB | SBB => sign(and(xor(Fa, Fb), xor(Fa, Fr)), w),
        LOGIC | SAR => K(0),
        INC | NEG => bin(BinOp::I32Eq, Fr, K(sign_min(w))),
        DEC => bin(BinOp::I32Eq, Fr, K(sign_min(w) - 1)),
        SHL => xor(sign(Fr, w), cf(kind)),
        SHR => sign(Fa, w),
        MUL => Fb,
        _ => bit(Fr, 11),
    }
}

/// The arithmetic flags as an eflags value.
pub fn eflags(kind: u32) -> E {
    if op_of(kind) == EXPLICIT {
        return and(Fr, K(fl::ARITH));
    }
    let mut e = cf(kind);
    e = or(e, shl(pf(kind), 2));
    e = or(e, shl(af(kind), 4));
    e = or(e, shl(zf(kind), 6));
    e = or(e, shl(sf(kind), 7));
    or(e, shl(of(kind), 11))
}

/// A condition code for a statically known kind, using direct comparisons
/// where the kind allows (e.g. `cmp a, b; jl` becomes `a < b`).
pub fn cond(cc: Cc, kind: u32) -> E {
    let w = width_of(kind);
    let op = op_of(kind);
    if cc.is_negated() {
        // Prefer a direct negated comparison where one exists.
        if let Some(e) = direct(cc, op, w) {
            return e;
        }
        return not1(cond(cc.negate(), kind));
    }
    if let Some(e) = direct(cc, op, w) {
        return e;
    }
    match cc {
        Cc::O => of(kind),
        Cc::B => cf(kind),
        Cc::E => zf(kind),
        Cc::BE => or(cf(kind), zf(kind)),
        Cc::S => sf(kind),
        Cc::P => pf(kind),
        Cc::L => xor(sf(kind), of(kind)),
        Cc::LE => or(zf(kind), xor(sf(kind), of(kind))),
        _ => unreachable!(),
    }
}

fn direct(cc: Cc, op: u32, w: u32) -> Option<E> {
    let s = |e: E| sext(e, w);
    Some(match (op, cc) {
        (SUB, Cc::E) => bin(BinOp::I32Eq, Fa, Fb),
        (SUB, Cc::NE) => bin(BinOp::I32Ne, Fa, Fb),
        (SUB, Cc::B) => bin(BinOp::I32LtU, Fa, Fb),
        (SUB, Cc::AE) => bin(BinOp::I32GeU, Fa, Fb),
        (SUB, Cc::BE) => bin(BinOp::I32LeU, Fa, Fb),
        (SUB, Cc::A) => bin(BinOp::I32GtU, Fa, Fb),
        (SUB, Cc::L) => bin(BinOp::I32LtS, s(Fa), s(Fb)),
        (SUB, Cc::GE) => bin(BinOp::I32GeS, s(Fa), s(Fb)),
        (SUB, Cc::LE) => bin(BinOp::I32LeS, s(Fa), s(Fb)),
        (SUB, Cc::G) => bin(BinOp::I32GtS, s(Fa), s(Fb)),
        (LOGIC, Cc::E) | (LOGIC, Cc::BE) => not1(Fr),
        (LOGIC, Cc::NE) | (LOGIC, Cc::A) => bin(BinOp::I32Ne, Fr, K(0)),
        (LOGIC, Cc::B) | (LOGIC, Cc::O) => K(0),
        (LOGIC, Cc::AE) | (LOGIC, Cc::NO) => K(1),
        (LOGIC, Cc::L) | (LOGIC, Cc::S) => sign(Fr, w),
        (LOGIC, Cc::GE) | (LOGIC, Cc::NS) => not1(sign(Fr, w)),
        (LOGIC, Cc::LE) => bin(BinOp::I32LeS, s(Fr), K(0)),
        (LOGIC, Cc::G) => bin(BinOp::I32GtS, s(Fr), K(0)),
        (_, Cc::NE) if op != EXPLICIT => bin(BinOp::I32Ne, Fr, K(0)),
        _ => return None,
    })
}

/// Condition from an eflags value (used by the generic helper).
pub fn cond_from_eflags(cc: Cc, e: E) -> E {
    let c = || bit(e.clone(), 0);
    let p = || bit(e.clone(), 2);
    let z = || bit(e.clone(), 6);
    let s = || bit(e.clone(), 7);
    let o = || bit(e.clone(), 11);
    let base = match Cc::from_u8(cc as u8 & !1) {
        Cc::O => o(),
        Cc::B => c(),
        Cc::E => z(),
        Cc::BE => or(c(), z()),
        Cc::S => s(),
        Cc::P => p(),
        Cc::L => xor(s(), o()),
        Cc::LE => or(z(), xor(s(), o())),
        _ => unreachable!(),
    };
    if cc.is_negated() {
        not1(base)
    } else {
        base
    }
}

/// Which lazy-state operands an expression reads, as a mask
/// (FR=1, FA=2, FB=4, FC=8).
pub fn operands(e: &E) -> u32 {
    match e {
        Fr => 1,
        Fa => 2,
        Fb => 4,
        Fc => 8,
        K(_) => 0,
        Bin(_, a, b) => operands(a) | operands(b),
        Un(_, a) => operands(a),
    }
}

pub fn all_kinds() -> impl Iterator<Item = u32> {
    (0..NUM_OPS).flat_map(|op| [8, 16, 32].into_iter().map(move |w| kind(op, w)))
}
