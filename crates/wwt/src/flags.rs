//! Flag formulas over the lazy flag state.
//!
//! Each arithmetic flag and each condition code is described as a small
//! expression tree over `FR`, `FA`, `FB` and `FC` for a statically known flag
//! kind. The optimizer lowers `Cond`/`Eflags` reads to IR from these trees
//! when the kind is known at translation time (fusing `cmp`+`jcc` into one
//! comparison); the generic run-time helpers are generated from the same
//! trees, so both paths share one definition of x86 flag semantics.
//!
//! For 64-bit operations (x86-64, width code 3) the operands are i64 values:
//! the same formulas are built with i64 value operations, and every flag
//! still comes out as an i32 0 or 1. In x86-64 code the lazy operands of
//! narrower operations are i64 too; [`narrow`] wraps them before the 32-bit
//! formulas apply.

use crate::abi::flags::{self as fl, *};
use crate::ir::{BinOp, Cc, UnOp};

#[derive(Debug, Clone, PartialEq)]
pub enum E {
    Fr,
    Fa,
    Fb,
    Fc,
    K(u32),
    /// An i64 constant (operands of 64-bit formulas).
    K64(u64),
    Bin(BinOp, Box<E>, Box<E>),
    Un(UnOp, Box<E>),
}

use E::*;

fn bin(op: BinOp, a: E, b: E) -> E {
    // Constant folding keeps the generated code small.
    if let (K64(x), K64(y)) = (&a, &b) {
        if let Some(v) = crate::opt::fold_bin(op, *x, *y) {
            return if op.result_ty() == crate::ir::Ty::I64 {
                K64(v)
            } else {
                K(v as u32)
            };
        }
    }
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
fn sext(a: E, w: u32) -> E {
    match w {
        8 => Un(UnOp::I32Extend8S, Box::new(a)),
        16 => Un(UnOp::I32Extend16S, Box::new(a)),
        _ => a,
    }
}

/// The i64 counterpart of an i32 operation.
pub fn op64(op: BinOp) -> BinOp {
    use BinOp::*;
    match op {
        I32Add => I64Add,
        I32Sub => I64Sub,
        I32Mul => I64Mul,
        I32And => I64And,
        I32Or => I64Or,
        I32Xor => I64Xor,
        I32Shl => I64Shl,
        I32ShrS => I64ShrS,
        I32ShrU => I64ShrU,
        I32Rotl => I64Rotl,
        I32Rotr => I64Rotr,
        I32Eq => I64Eq,
        I32Ne => I64Ne,
        I32LtS => I64LtS,
        I32LtU => I64LtU,
        I32GtS => I64GtS,
        I32GtU => I64GtU,
        I32LeS => I64LeS,
        I32LeU => I64LeU,
        I32GeS => I64GeS,
        I32GeU => I64GeU,
        I32DivS => I64DivS,
        I32DivU => I64DivU,
        I32RemS => I64RemS,
        I32RemU => I64RemU,
        o => o,
    }
}

/// Formula context: `wide` when the lazy operands are 64-bit values.
#[derive(Clone, Copy)]
struct C {
    wide: bool,
}

impl C {
    fn of(kind: u32) -> C {
        C {
            wide: width_of(kind) == 64,
        }
    }
    /// An operation on operand values.
    fn v(self, op: BinOp, a: E, b: E) -> E {
        bin(if self.wide { op64(op) } else { op }, a, b)
    }
    /// A constant in the operand domain.
    fn k(self, c: u64) -> E {
        if self.wide {
            K64(c)
        } else {
            K(c as u32)
        }
    }
    /// The low 32 bits of an operand-domain value.
    fn lo(self, a: E) -> E {
        if self.wide {
            Un(UnOp::I32WrapI64, Box::new(a))
        } else {
            a
        }
    }
    /// Bit `n` of an operand-domain value, as an i32 0/1.
    fn bit(self, a: E, n: u32) -> E {
        if self.wide {
            and(self.lo(self.v(BinOp::I32ShrU, a, K64(n as u64))), K(1))
        } else {
            bit(a, n)
        }
    }
    fn sign(self, a: E, w: u32) -> E {
        self.bit(a, w - 1)
    }
    /// 1 when an operand-domain value is zero.
    fn is_zero(self, a: E) -> E {
        if self.wide {
            Un(UnOp::I64Eqz, Box::new(a))
        } else {
            not1(a)
        }
    }
    fn sext(self, a: E, w: u32) -> E {
        sext(a, w)
    }
    fn sign_min(self, w: u32) -> u64 {
        1u64 << (w - 1)
    }
}

/// Carry flag for a statically known kind.
pub fn cf(kind: u32) -> E {
    let w = width_of(kind);
    let c = C::of(kind);
    match op_of(kind) {
        ADD => c.v(BinOp::I32LtU, Fr, Fa),
        ADC => or(
            c.v(BinOp::I32LtU, Fr, Fa),
            and(c.lo(Fc), c.v(BinOp::I32Eq, Fr, Fa)),
        ),
        SUB => c.v(BinOp::I32LtU, Fa, Fb),
        SBB => or(
            c.v(BinOp::I32LtU, Fa, Fb),
            and(c.lo(Fc), c.v(BinOp::I32Eq, Fa, Fb)),
        ),
        LOGIC => K(0),
        INC | DEC => c.lo(Fc),
        NEG => c.v(BinOp::I32Ne, Fa, c.k(0)),
        SHL => c.bit(
            c.v(BinOp::I32Shl, Fa, c.v(BinOp::I32Sub, Fb, c.k(1))),
            w - 1,
        ),
        SHR => c.bit(c.v(BinOp::I32ShrU, Fa, c.v(BinOp::I32Sub, Fb, c.k(1))), 0),
        SAR => c.bit(
            c.v(
                BinOp::I32ShrS,
                c.sext(Fa, w),
                c.v(BinOp::I32Sub, Fb, c.k(1)),
            ),
            0,
        ),
        MUL => c.lo(Fb),
        _ => and(c.lo(Fr), K(1)),
    }
}

pub fn zf(kind: u32) -> E {
    let c = C::of(kind);
    match op_of(kind) {
        EXPLICIT => bit(Fr, 6),
        _ => c.is_zero(Fr),
    }
}

pub fn sf(kind: u32) -> E {
    let c = C::of(kind);
    match op_of(kind) {
        EXPLICIT => bit(Fr, 7),
        _ => c.sign(Fr, width_of(kind)),
    }
}

pub fn pf(kind: u32) -> E {
    let c = C::of(kind);
    match op_of(kind) {
        EXPLICIT => bit(Fr, 2),
        _ => not1(and(
            Un(UnOp::I32Popcnt, Box::new(and(c.lo(Fr), K(0xff)))),
            K(1),
        )),
    }
}

pub fn af(kind: u32) -> E {
    let c = C::of(kind);
    match op_of(kind) {
        ADD | ADC | SUB | SBB => bit(xor(xor(c.lo(Fa), c.lo(Fb)), c.lo(Fr)), 4),
        INC => not1(and(c.lo(Fr), K(0xf))),
        DEC => bin(BinOp::I32Eq, and(c.lo(Fr), K(0xf)), K(0xf)),
        NEG => bit(xor(c.lo(Fa), c.lo(Fr)), 4),
        EXPLICIT => bit(Fr, 4),
        _ => K(0),
    }
}

pub fn of(kind: u32) -> E {
    let w = width_of(kind);
    let c = C::of(kind);
    match op_of(kind) {
        ADD | ADC => c.sign(
            c.v(
                BinOp::I32And,
                c.v(BinOp::I32Xor, Fa, Fr),
                c.v(BinOp::I32Xor, Fb, Fr),
            ),
            w,
        ),
        SUB | SBB => c.sign(
            c.v(
                BinOp::I32And,
                c.v(BinOp::I32Xor, Fa, Fb),
                c.v(BinOp::I32Xor, Fa, Fr),
            ),
            w,
        ),
        LOGIC | SAR => K(0),
        INC | NEG => c.v(BinOp::I32Eq, Fr, c.k(c.sign_min(w))),
        DEC => c.v(BinOp::I32Eq, Fr, c.k(c.sign_min(w) - 1)),
        SHL => xor(c.sign(Fr, w), cf(kind)),
        SHR => c.sign(Fa, w),
        MUL => c.lo(Fb),
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
    let c = C { wide: w == 64 };
    let s = |e: E| sext(e, w);
    Some(match (op, cc) {
        (SUB, Cc::E) => c.v(BinOp::I32Eq, Fa, Fb),
        (SUB, Cc::NE) => c.v(BinOp::I32Ne, Fa, Fb),
        (SUB, Cc::B) => c.v(BinOp::I32LtU, Fa, Fb),
        (SUB, Cc::AE) => c.v(BinOp::I32GeU, Fa, Fb),
        (SUB, Cc::BE) => c.v(BinOp::I32LeU, Fa, Fb),
        (SUB, Cc::A) => c.v(BinOp::I32GtU, Fa, Fb),
        (SUB, Cc::L) => c.v(BinOp::I32LtS, s(Fa), s(Fb)),
        (SUB, Cc::GE) => c.v(BinOp::I32GeS, s(Fa), s(Fb)),
        (SUB, Cc::LE) => c.v(BinOp::I32LeS, s(Fa), s(Fb)),
        (SUB, Cc::G) => c.v(BinOp::I32GtS, s(Fa), s(Fb)),
        (LOGIC, Cc::E) | (LOGIC, Cc::BE) => c.is_zero(Fr),
        (LOGIC, Cc::NE) | (LOGIC, Cc::A) => c.v(BinOp::I32Ne, Fr, c.k(0)),
        (LOGIC, Cc::B) | (LOGIC, Cc::O) => K(0),
        (LOGIC, Cc::AE) | (LOGIC, Cc::NO) => K(1),
        (LOGIC, Cc::L) | (LOGIC, Cc::S) => c.sign(Fr, w),
        (LOGIC, Cc::GE) | (LOGIC, Cc::NS) => not1(c.sign(Fr, w)),
        (LOGIC, Cc::LE) => c.v(BinOp::I32LeS, s(Fr), c.k(0)),
        (LOGIC, Cc::G) => c.v(BinOp::I32GtS, s(Fr), c.k(0)),
        (_, Cc::NE) if op != EXPLICIT => c.v(BinOp::I32Ne, Fr, c.k(0)),
        _ => return None,
    })
}

/// Rewrites a formula for a narrow (8/16/32-bit) kind so it reads i64 lazy
/// operands, as x86-64 code keeps them: each operand is wrapped to i32.
pub fn narrow(e: E) -> E {
    match e {
        Fr | Fa | Fb | Fc => Un(UnOp::I32WrapI64, Box::new(e)),
        K(_) | K64(_) => e,
        Bin(op, a, b) => Bin(op, Box::new(narrow(*a)), Box::new(narrow(*b))),
        Un(op, a) => Un(op, Box::new(narrow(*a))),
    }
}

/// A formula for a kind in x86-64 code (i64 lazy operands).
pub fn for_x64(e: E, kind: u32) -> E {
    if width_of(kind) == 64 && op_of(kind) != EXPLICIT {
        e
    } else {
        narrow(e)
    }
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
        K(_) | K64(_) => 0,
        Bin(_, a, b) => operands(a) | operands(b),
        Un(_, a) => operands(a),
    }
}

pub fn all_kinds() -> impl Iterator<Item = u32> {
    (0..NUM_OPS).flat_map(|op| [8, 16, 32].into_iter().map(move |w| kind(op, w)))
}

/// Every kind in x86-64 code, in `op * 4 + width_code` order.
pub fn all_kinds64() -> impl Iterator<Item = u32> {
    (0..NUM_OPS).flat_map(|op| [8, 16, 32, 64].into_iter().map(move |w| kind(op, w)))
}

/// Evaluates an expression for concrete operand values.
pub fn eval(e: &E, fr: u64, fa: u64, fb: u64, fc: u64) -> u64 {
    match e {
        Fr => fr,
        Fa => fa,
        Fb => fb,
        Fc => fc,
        K(c) => *c as u64,
        K64(c) => *c,
        Bin(op, a, b) => {
            let x = eval(a, fr, fa, fb, fc);
            let y = eval(b, fr, fa, fb, fc);
            crate::opt::fold_bin(*op, x, y).expect("foldable flag op")
        }
        Un(op, a) => {
            let x = eval(a, fr, fa, fb, fc);
            crate::opt::fold_un(*op, x).expect("foldable flag op")
        }
    }
}

/// The arithmetic eflags for a lazy flag state, as the run-time helper
/// computes them.
pub fn eflags_of_state(fk: u32, fr: u32, fa: u32, fb: u32, fc: u32) -> u32 {
    if op_of(fk) >= NUM_OPS || (fk >> 8 & 3) > 2 {
        return fr & fl::ARITH;
    }
    eval(&eflags(fk), fr as u64, fa as u64, fb as u64, fc as u64) as u32
}

/// [`eflags_of_state`] for x86-64 code, whose lazy operands are 64-bit.
pub fn eflags_of_state64(fk: u32, fr: u64, fa: u64, fb: u64, fc: u64) -> u32 {
    if op_of(fk) >= NUM_OPS {
        return fr as u32 & fl::ARITH;
    }
    eval(&for_x64(eflags(fk), fk), fr, fa, fb, fc) as u32
}
