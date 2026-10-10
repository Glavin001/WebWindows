//! Layer 5 — Optimize.
//!
//! * [`lower_flags`]: replaces `Cond`/`Eflags` reads with direct expressions
//!   when the flag kind is known statically (fusing compare-and-branch), and
//!   with a generic helper call otherwise.
//! * [`simplify`]: constant folding, copy propagation and algebraic
//!   identities.
//! * [`dce`]: whole-function liveness and dead-code elimination, which is
//!   what removes flag computations nobody reads.
//! * [`narrow`]: signed narrow loads and 32-bit multiplies where x86
//!   semantics were spelled out with extensions and 64-bit products.
//!
//! [`analyze`] computes the facts code generation needs: which state vregs
//! are dirty (differ from the CPU struct) at each point and which are live.

use std::collections::HashMap;

use crate::abi::flags as fl;
use crate::flags::{self, E};
use crate::abi::cpu;
use crate::ir::*;

// ---- Flag lowering ---------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// Not yet reached by the analysis.
    Top,
    Known(u32),
    Unknown,
}

fn meet(a: Kind, b: Kind) -> Kind {
    match (a, b) {
        (Kind::Top, x) | (x, Kind::Top) => x,
        (Kind::Known(x), Kind::Known(y)) if x == y => Kind::Known(x),
        _ => Kind::Unknown,
    }
}

fn block_kind_out(b: &Block, mut k: Kind) -> Kind {
    for inst in &b.insts {
        if inst.dst == Some(FK) {
            k = match inst.op {
                Op::Const(c) => Kind::Known(c as u32),
                _ => Kind::Unknown,
            };
        }
    }
    if b.term.clobbers_state() {
        Kind::Unknown
    } else {
        k
    }
}

/// Lowers `Cond` and `Eflags` to explicit operations.
pub fn lower_flags(f: &mut Function) {
    let x64 = f.mode == Mode::X64;
    let (h_eflags, h_cond) = if x64 {
        (Helper::Eflags64, Helper::EvalCond64)
    } else {
        (Helper::Eflags, Helper::EvalCond)
    };
    let fix = |e: E, kind: u32| if x64 { flags::for_x64(e, kind) } else { e };
    let n = f.blocks.len();
    let order = f.rpo();
    let mut kin = vec![Kind::Top; n];
    kin[0] = Kind::Unknown;
    let mut changed = true;
    while changed {
        changed = false;
        for &b in &order {
            let out = block_kind_out(&f.blocks[b as usize], kin[b as usize]);
            for s in f.blocks[b as usize].term.successors() {
                let m = meet(kin[s as usize], out);
                if m != kin[s as usize] {
                    kin[s as usize] = m;
                    changed = true;
                }
            }
        }
    }
    for b in 0..n {
        let insts = std::mem::take(&mut f.blocks[b].insts);
        let mut k = kin[b];
        let mut out = Vec::with_capacity(insts.len());
        for inst in insts {
            match (&inst.op, inst.dst) {
                (Op::Cond(cc), Some(dst)) => {
                    let e = match k {
                        Kind::Known(kind) => Some(fix(flags::cond(*cc, kind), kind)),
                        _ => None,
                    };
                    match e {
                        Some(e) => {
                            let v = emit_expr(f, &mut out, &e, inst.eip);
                            out.push(Inst {
                                dst: Some(dst),
                                op: Op::Copy(v),
                                eip: inst.eip,
                            });
                        }
                        None => {
                            let c = f.new_vreg(Ty::I32);
                            out.push(Inst {
                                dst: Some(c),
                                op: Op::Const(*cc as u64),
                                eip: inst.eip,
                            });
                            out.push(Inst {
                                dst: Some(dst),
                                op: Op::CallHelper(h_cond, vec![c, FK, FR, FA, FB, FC]),
                                eip: inst.eip,
                            });
                        }
                    }
                }
                (Op::Eflags, Some(dst)) => match k {
                    Kind::Known(kind) => {
                        let e = fix(flags::eflags(kind), kind);
                        let v = emit_expr(f, &mut out, &e, inst.eip);
                        out.push(Inst {
                            dst: Some(dst),
                            op: Op::Copy(v),
                            eip: inst.eip,
                        });
                    }
                    _ => out.push(Inst {
                        dst: Some(dst),
                        op: Op::CallHelper(h_eflags, vec![FK, FR, FA, FB, FC]),
                        eip: inst.eip,
                    }),
                },
                _ => {
                    if inst.dst == Some(FK) {
                        k = match inst.op {
                            Op::Const(c) => Kind::Known(c as u32),
                            _ => Kind::Unknown,
                        };
                    }
                    out.push(inst);
                }
            }
        }
        f.blocks[b].insts = out;
    }
}

fn emit_expr(f: &mut Function, out: &mut Vec<Inst>, e: &E, eip: u64) -> V {
    let push = |f: &mut Function, op: Op, out: &mut Vec<Inst>| {
        let ty = match &op {
            Op::Bin(b, _, _) => b.result_ty(),
            Op::Un(u, _) => u.result_ty(),
            _ => Ty::I32,
        };
        let v = f.new_vreg(ty);
        out.push(Inst {
            dst: Some(v),
            op,
            eip,
        });
        v
    };
    match e {
        E::Fr => FR,
        E::Fa => FA,
        E::Fb => FB,
        E::Fc => FC,
        E::K(c) => push(f, Op::Const(*c as u64), out),
        E::K64(c) => {
            let v = f.new_vreg(Ty::I64);
            out.push(Inst {
                dst: Some(v),
                op: Op::Const(*c),
                eip,
            });
            v
        }
        E::Bin(op, a, b) => {
            let a = emit_expr(f, out, a, eip);
            let b = emit_expr(f, out, b, eip);
            push(f, Op::Bin(*op, a, b), out)
        }
        E::Un(op, a) => {
            let a = emit_expr(f, out, a, eip);
            push(f, Op::Un(*op, a), out)
        }
    }
}

// ---- Simplification ----------------------------------------------------------

pub(crate) fn fold_bin(op: BinOp, a: u64, b: u64) -> Option<u64> {
    use BinOp::*;
    let (x, y) = (a as u32, b as u32);
    let r32 = |v: u32| Some(v as u64);
    match op {
        I32Add => r32(x.wrapping_add(y)),
        I32Sub => r32(x.wrapping_sub(y)),
        I32Mul => r32(x.wrapping_mul(y)),
        I32And => r32(x & y),
        I32Or => r32(x | y),
        I32Xor => r32(x ^ y),
        I32Shl => r32(x.wrapping_shl(y)),
        I32ShrU => r32(x.wrapping_shr(y)),
        I32ShrS => r32((x as i32).wrapping_shr(y) as u32),
        I32Rotl => r32(x.rotate_left(y & 31)),
        I32Rotr => r32(x.rotate_right(y & 31)),
        I32Eq => r32((x == y) as u32),
        I32Ne => r32((x != y) as u32),
        I32LtS => r32(((x as i32) < (y as i32)) as u32),
        I32LtU => r32((x < y) as u32),
        I32GtS => r32(((x as i32) > (y as i32)) as u32),
        I32GtU => r32((x > y) as u32),
        I32LeS => r32(((x as i32) <= (y as i32)) as u32),
        I32LeU => r32((x <= y) as u32),
        I32GeS => r32(((x as i32) >= (y as i32)) as u32),
        I32GeU => r32((x >= y) as u32),
        I32DivU if y != 0 => r32(x / y),
        I32RemU if y != 0 => r32(x % y),
        I64Add => Some(a.wrapping_add(b)),
        I64Sub => Some(a.wrapping_sub(b)),
        I64Mul => Some(a.wrapping_mul(b)),
        I64And => Some(a & b),
        I64Or => Some(a | b),
        I64Xor => Some(a ^ b),
        I64Shl => Some(a.wrapping_shl(b as u32)),
        I64ShrU => Some(a.wrapping_shr(b as u32)),
        I64ShrS => Some((a as i64).wrapping_shr(b as u32) as u64),
        I64Eq => Some((a == b) as u64),
        I64Ne => Some((a != b) as u64),
        I64LtS => Some(((a as i64) < (b as i64)) as u64),
        I64LtU => Some((a < b) as u64),
        I64GtS => Some(((a as i64) > (b as i64)) as u64),
        I64GtU => Some((a > b) as u64),
        I64LeS => Some(((a as i64) <= (b as i64)) as u64),
        I64LeU => Some((a <= b) as u64),
        I64GeS => Some(((a as i64) >= (b as i64)) as u64),
        I64GeU => Some((a >= b) as u64),
        I64Rotl => Some(a.rotate_left(b as u32 & 63)),
        I64Rotr => Some(a.rotate_right(b as u32 & 63)),
        I64DivU if b != 0 => Some(a / b),
        I64RemU if b != 0 => Some(a % b),
        _ => None,
    }
}

pub(crate) fn fold_un(op: UnOp, a: u64) -> Option<u64> {
    use UnOp::*;
    let x = a as u32;
    Some(match op {
        I32Eqz => (x == 0) as u64,
        I32Clz => x.leading_zeros() as u64,
        I32Ctz => x.trailing_zeros() as u64,
        I32Popcnt => x.count_ones() as u64,
        I32Extend8S => (x as u8 as i8 as i32 as u32) as u64,
        I32Extend16S => (x as u16 as i16 as i32 as u32) as u64,
        I64Eqz => (a == 0) as u64,
        I64ExtendI32S => x as i32 as i64 as u64,
        I64ExtendI32U => x as u64,
        I32WrapI64 => (a as u32) as u64,
        I64Clz => a.leading_zeros() as u64,
        I64Ctz => a.trailing_zeros() as u64,
        I64Popcnt => a.count_ones() as u64,
        I64Extend8S => a as u8 as i8 as i64 as u64,
        I64Extend16S => a as u16 as i16 as i64 as u64,
        I64Extend32S => a as u32 as i32 as i64 as u64,
        _ => return None,
    })
}

/// Constant folding and copy propagation. Temporaries (vregs above
/// [`NUM_STATE`]) have a single definition, so their constants and copies of
/// other temporaries hold function-wide; copies of state vregs hold until the
/// state vreg is redefined within the block.
pub fn simplify(f: &mut Function) {
    // Function-wide facts about temporaries.
    let mut gconst: HashMap<V, u64> = HashMap::new();
    let mut gcopy: HashMap<V, V> = HashMap::new();
    // Temporaries that zero- or sign-extend another temporary to i64
    // (x86-64 code writes 32-bit results this way and reads them back
    // wrapped).
    let mut gext: HashMap<V, V> = HashMap::new();
    for b in &f.blocks {
        for inst in &b.insts {
            if let Some(d) = inst.dst {
                if d < NUM_STATE {
                    continue;
                }
                match inst.op {
                    Op::Const(c) => {
                        gconst.insert(d, c);
                    }
                    Op::Copy(s) if s >= NUM_STATE => {
                        gcopy.insert(d, s);
                    }
                    Op::Un(UnOp::I64ExtendI32U | UnOp::I64ExtendI32S, s) if s >= NUM_STATE => {
                        gext.insert(d, s);
                    }
                    _ => {}
                }
            }
        }
    }
    let resolve_g = |mut v: V, gcopy: &HashMap<V, V>| {
        let mut n = 0;
        while let Some(&s) = gcopy.get(&v) {
            v = s;
            n += 1;
            if n > 64 {
                break;
            }
        }
        v
    };
    for bi in 0..f.blocks.len() {
        // Block-local facts.
        let mut lconst: HashMap<V, u64> = HashMap::new();
        let mut lcopy: HashMap<V, V> = HashMap::new();
        // lcopy's entries by source, to forget the copies of a state vreg
        // when it is written without scanning them all (obfuscated code has
        // blocks of thousands of instructions).
        let mut lcopy_of: HashMap<V, Vec<V>> = HashMap::new();
        // State vregs currently holding an extension of a temporary.
        let mut lext: HashMap<V, V> = HashMap::new();
        let mut insts = std::mem::take(&mut f.blocks[bi].insts);
        let mut no_ops = vec![];
        for (k, inst) in insts.iter_mut().enumerate() {
            for u in inst.op.uses_mut() {
                let mut v = resolve_g(*u, &gcopy);
                if let Some(&s) = lcopy.get(&v) {
                    v = s;
                }
                *u = v;
            }
            let cst = |v: V, lconst: &HashMap<V, u64>| -> Option<u64> {
                gconst.get(&v).or_else(|| lconst.get(&v)).copied()
            };
            // A copy's source, before folding turns it into a constant.
            let copy_src = match inst.op {
                Op::Copy(s) => Some(s),
                _ => None,
            };
            // Fold.
            let ty = inst.dst.map(|d| f.vtypes[d as usize]);
            let new_op = match &inst.op {
                Op::Bin(op, a, b) => {
                    let (ca, cb) = (cst(*a, &lconst), cst(*b, &lconst));
                    match (ca, cb) {
                        (Some(x), Some(y)) => fold_bin(*op, x, y).map(Op::Const),
                        _ => simplify_bin(*op, *a, *b, ca, cb),
                    }
                }
                Op::Un(op, a) => cst(*a, &lconst)
                    .and_then(|x| fold_un(*op, x))
                    .map(Op::Const)
                    .or_else(|| match op {
                        // wrap(extend(x)) is x.
                        UnOp::I32WrapI64 => {
                            gext.get(a).or_else(|| lext.get(a)).map(|&x| Op::Copy(x))
                        }
                        _ => None,
                    }),
                Op::Select { cond, t, f: fv } => match cst(*cond, &lconst) {
                    Some(c) => Some(Op::Copy(if c as u32 != 0 { *t } else { *fv })),
                    None if t == fv => Some(Op::Copy(*t)),
                    None => None,
                },
                Op::Copy(s) => cst(*s, &lconst).map(Op::Const),
                _ => None,
            };
            if let Some(op) = new_op {
                inst.op = op;
            }
            let _ = ty;
            // `v = v` (alignment no-ops such as `lea esi, [esi+0]`) changes
            // nothing; kept, it would make a state vreg look written.
            if matches!(inst.op, Op::Copy(s) if Some(s) == inst.dst) {
                no_ops.push(k);
                continue;
            }
            if let Some(d) = inst.dst {
                // Invalidate facts that depend on the redefined vreg.
                lconst.remove(&d);
                lcopy.remove(&d);
                lext.remove(&d);
                if d < NUM_STATE {
                    if let Op::Un(UnOp::I64ExtendI32U | UnOp::I64ExtendI32S, x) = inst.op {
                        if x >= NUM_STATE {
                            lext.insert(d, x);
                        }
                    }
                }
                if d < NUM_STATE {
                    for c in lcopy_of.remove(&d).unwrap_or_default() {
                        if lcopy.get(&c) == Some(&d) {
                            lcopy.remove(&c);
                        }
                    }
                }
                match inst.op {
                    Op::Const(c) => {
                        lconst.insert(d, c);
                        // Later uses of a state vreg can read the temporary
                        // instead, letting the state write die.
                        if let Some(src) = copy_src {
                            if d < NUM_STATE && src >= NUM_STATE {
                                lcopy.insert(d, src);
                                lcopy_of.entry(src).or_default().push(d);
                            }
                        }
                    }
                    Op::Copy(s) if s != d => {
                        lcopy.insert(d, s);
                        lcopy_of.entry(s).or_default().push(d);
                        if let Some(c) = cst(s, &lconst) {
                            lconst.insert(d, c);
                        }
                    }
                    _ => {}
                }
            }
        }
        // Terminator uses.
        let mut term = std::mem::replace(&mut f.blocks[bi].term, Term::None);
        for u in term.uses_mut() {
            let mut v = resolve_g(*u, &gcopy);
            if let Some(&s) = lcopy.get(&v) {
                v = s;
            }
            *u = v;
        }
        // A branch with one target, or on a constant, becomes a jump.
        if let Term::Branch { t, f: fb, .. } = &term {
            if t == fb {
                term = Term::Jump(*t);
            }
        }
        if let Term::Branch { cond, t, f: fb } = &term {
            let c = gconst.get(cond).or_else(|| lconst.get(cond)).copied();
            if let Some(c) = c {
                term = Term::Jump(if c as u32 != 0 { *t } else { *fb });
            }
        }
        f.blocks[bi].term = term;
        if !no_ops.is_empty() {
            let mut drop = vec![false; insts.len()];
            for k in no_ops {
                drop[k] = true;
            }
            let mut k = 0;
            insts.retain(|_| {
                k += 1;
                !drop[k - 1]
            });
        }
        f.blocks[bi].insts = insts;
    }
    f.remove_unreachable();
}

fn simplify_bin(op: BinOp, a: V, b: V, ca: Option<u64>, cb: Option<u64>) -> Option<Op> {
    use BinOp::*;
    let ca32 = ca.map(|c| c as u32);
    let cb32 = cb.map(|c| c as u32);
    match (op, ca32, cb32) {
        (
            I32Add | I32Sub | I32Or | I32Xor | I32Shl | I32ShrU | I32ShrS | I32Rotl | I32Rotr,
            _,
            Some(0),
        ) => Some(Op::Copy(a)),
        (I32Add | I32Or | I32Xor, Some(0), _) => Some(Op::Copy(b)),
        (I32And, _, Some(u32::MAX)) => Some(Op::Copy(a)),
        (I32And, Some(u32::MAX), _) => Some(Op::Copy(b)),
        (I32And, _, Some(0)) | (I32And, Some(0), _) => Some(Op::Const(0)),
        (I32Mul, _, Some(1)) => Some(Op::Copy(a)),
        (I32Mul, Some(1), _) => Some(Op::Copy(b)),
        (I32Sub | I32Xor, _, _) if a == b => Some(Op::Const(0)),
        (I32And | I32Or, _, _) if a == b => Some(Op::Copy(a)),
        (I32Eq | I32LeU | I32GeU | I32LeS | I32GeS, _, _) if a == b => Some(Op::Const(1)),
        (I32Ne | I32LtU | I32GtU | I32LtS | I32GtS, _, _) if a == b => Some(Op::Const(0)),
        (I32LtU, _, Some(0)) => Some(Op::Const(0)),
        (I32GeU, _, Some(0)) => Some(Op::Const(1)),
        (I64Or | I64Xor | I64Shl | I64ShrU | I64ShrS | I64Add | I64Sub, _, Some(0))
            if cb == Some(0) =>
        {
            Some(Op::Copy(a))
        }
        (I64Add | I64Or | I64Xor, _, _) if ca == Some(0) => Some(Op::Copy(b)),
        (I64And, _, _) if cb == Some(u64::MAX) => Some(Op::Copy(a)),
        (I64And, _, _) if cb == Some(0) || ca == Some(0) => Some(Op::Const(0)),
        (I64Sub | I64Xor, _, _) if a == b => Some(Op::Const(0)),
        _ => None,
    }
}

// ---- Analysis -----------------------------------------------------------------

/// State vregs as a bit mask.
pub type StateMask = u64;

pub fn state_bit(v: V) -> StateMask {
    if v < NUM_STATE {
        1u64 << v
    } else {
        0
    }
}

/// State written back at fault points: everything except the lazy flag
/// state, whose precision at faults we do not guarantee.
pub const FAULT_SYNC: StateMask =
    !(1u64 << FK | 1u64 << FR | 1u64 << FA | 1u64 << FB | 1u64 << FC | 1u64 << CPU);
pub const ALL_STATE: StateMask = !(1u64 << CPU);

/// The import slot `v` was loaded from, when block `b` loads it from a
/// constant address (`call [slot]`, `jmp [slot]`).
pub fn slot_of(f: &Function, b: BlockId, v: V) -> Option<u64> {
    let insts = &f.blocks[b as usize].insts;
    let load = insts.iter().rev().find(|i| i.dst == Some(v))?;
    let Op::Load { addr, mem } = &load.op else {
        return None;
    };
    let base = insts.iter().rev().find(|i| i.dst == Some(*addr))?;
    match base.op {
        Op::Const(c) if mem.size == 4 => Some((c as u32).wrapping_add(mem.offset) as u64),
        // x86-64 import slots hold 8 bytes.
        Op::Const(c) if mem.size == 8 => Some(c.wrapping_add(mem.offset as u64)),
        _ => None,
    }
}

/// State written back at the terminator of block `b`, when it writes back
/// state (see `CallAbi`).
pub fn term_sync(f: &Function, b: BlockId) -> StateMask {
    let flags_dead = match &f.blocks[b as usize].term {
        Term::Ret(_) => f.abi.flags_dead_at_ret,
        Term::Call {
            target: CallTarget::Direct(t),
            ..
        } => !f.abi.direct_callee_reads_flags(*t),
        Term::Call {
            target: CallTarget::Indirect(v),
            ..
        } => !f.abi.indirect_callee_reads_flags(slot_of(f, b, *v)),
        // A jump to another function's code: it continues there, and
        // returns to this function's caller. (An exit to the function's own
        // entry is a re-entry at a loop header, see `osr`, where the flags
        // may be live.)
        Term::Exit(t) => *t != f.entry && !f.abi.direct_callee_reads_flags(*t),
        // A tail jump through an import slot (an import stub): the import
        // neither reads the flags (see `indirect_callee_reads_flags`) nor
        // keeps them for its caller (system DLLs follow the C calling
        // convention). Other indirect jumps may stay within the function.
        Term::JmpInd(v) => {
            slot_of(f, b, *v).is_some_and(|s| !f.abi.indirect_callee_reads_flags(Some(s)))
        }
        _ => false,
    };
    if flags_dead {
        FAULT_SYNC
    } else {
        ALL_STATE
    }
}

/// State a call leaves unchanged, which keeps its value across the call
/// instead of being reloaded (see `CallAbi::callee_saved`).
pub fn call_preserved(f: &Function) -> StateMask {
    if f.abi.callee_saved {
        1u64 << EBX | 1u64 << ESI | 1u64 << EDI | 1u64 << EBP
    } else {
        0
    }
}

/// A dense bit set.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BitSet(Vec<u64>);

impl BitSet {
    pub fn new(n: usize) -> BitSet {
        BitSet(vec![0; n.div_ceil(64)])
    }
    pub fn insert(&mut self, i: usize) {
        self.0[i / 64] |= 1 << (i % 64);
    }
    pub fn remove(&mut self, i: usize) {
        self.0[i / 64] &= !(1 << (i % 64));
    }
    pub fn contains(&self, i: usize) -> bool {
        self.0[i / 64] >> (i % 64) & 1 != 0
    }
    pub fn union(&mut self, o: &BitSet) -> bool {
        let mut changed = false;
        for (a, b) in self.0.iter_mut().zip(&o.0) {
            let n = *a | *b;
            changed |= n != *a;
            *a = n;
        }
        changed
    }
}

/// Facts for code generation.
pub struct Analysis {
    /// Dirty state vregs on entry to each block.
    pub dirty_in: Vec<StateMask>,
    /// Dirty state vregs at the end of each block (before the terminator).
    pub dirty_end: Vec<StateMask>,
    /// Vregs that are live across block boundaries, densely numbered.
    pub dense: Vec<u32>,
    pub global: Vec<V>,
    pub live_in: Vec<BitSet>,
    /// Blocks at whose end all dirty state is live: see `preempt_points`.
    pub preempt: Vec<bool>,
}

impl Analysis {
    pub fn is_live_in(&self, b: BlockId, v: V) -> bool {
        let d = self.dense[v as usize];
        d != u32::MAX && self.live_in[b as usize].contains(d as usize)
    }
    /// State vregs live on entry to a block.
    pub fn state_live_in(&self, b: BlockId) -> StateMask {
        let mut m = 0;
        for v in 0..NUM_STATE {
            if self.is_live_in(b, v) {
                m |= 1u64 << v;
            }
        }
        m
    }
}

/// Dirty state vregs before each instruction of a block, plus at the end.
pub fn dirty_trace(b: &Block, dirty_in: StateMask) -> Vec<StateMask> {
    let mut d = dirty_in;
    let mut out = Vec::with_capacity(b.insts.len() + 1);
    for inst in &b.insts {
        out.push(d);
        if let Some(v) = inst.dst {
            d |= state_bit(v) & ALL_STATE;
        }
    }
    out.push(d);
    out
}

/// Blocks whose edges may get a preemption check in code generation
/// (`codegen::preempt_check`): a thread that has used up its time slice
/// writes its state back there and later resumes at the edge's target, so
/// all dirty state must still be in its vregs at the block's end. These are
/// blocks with a retreating edge in reverse postorder (loop back edges),
/// and in an irreducible function also blocks with an edge into a block
/// with several predecessors, since making it reducible turns edges into
/// loop entries into back edges.
pub fn preempt_points(f: &Function) -> Vec<bool> {
    let order = f.rpo();
    let mut rpo = vec![usize::MAX; f.blocks.len()];
    for (i, &b) in order.iter().enumerate() {
        rpo[b as usize] = i;
    }
    let irreducible = !crate::reducible::is_reducible(f);
    let preds = f.predecessors();
    (0..f.blocks.len())
        .map(|b| {
            f.blocks[b]
                .term
                .successors()
                .iter()
                .any(|&s| rpo[s as usize] <= rpo[b] || irreducible && preds[s as usize].len() > 1)
        })
        .collect()
}

pub fn analyze(f: &Function) -> Analysis {
    let n = f.blocks.len();
    let order = f.rpo();
    let preempt = preempt_points(f);
    // Forward: dirty state.
    let mut dirty_in = vec![0 as StateMask; n];
    let mut dirty_end = vec![0 as StateMask; n];
    let mut changed = true;
    while changed {
        changed = false;
        for &b in &order {
            let blk = &f.blocks[b as usize];
            let end = *dirty_trace(blk, dirty_in[b as usize]).last().unwrap();
            dirty_end[b as usize] = end;
            let out = if blk.term.clobbers_state() { 0 } else { end };
            for s in blk.term.successors() {
                let m = dirty_in[s as usize] | out;
                if m != dirty_in[s as usize] {
                    dirty_in[s as usize] = m;
                    changed = true;
                }
            }
        }
    }
    // Global vregs: state vregs plus anything upward-exposed in some block.
    let mut dense = vec![u32::MAX; f.vtypes.len()];
    let mut global: Vec<V> = (0..NUM_STATE).collect();
    for v in 0..NUM_STATE {
        dense[v as usize] = v;
    }
    let mut ue_list: Vec<Vec<V>> = Vec::with_capacity(n);
    for blk in &f.blocks {
        let mut defined: Vec<V> = vec![];
        let mut ue = vec![];
        let mut def_set = std::collections::HashSet::new();
        for inst in &blk.insts {
            for u in inst.op.uses() {
                if !def_set.contains(&u) {
                    ue.push(u);
                }
            }
            if let Some(d) = inst.dst {
                def_set.insert(d);
                defined.push(d);
            }
        }
        for u in blk.term.uses() {
            if !def_set.contains(&u) {
                ue.push(u);
            }
        }
        for &u in &ue {
            if dense[u as usize] == u32::MAX {
                dense[u as usize] = global.len() as u32;
                global.push(u);
            }
        }
        ue_list.push(ue);
    }
    let ng = global.len();
    let mut live_in = vec![BitSet::new(ng); n];
    let mut changed = true;
    let post: Vec<BlockId> = order.iter().rev().copied().collect();
    while changed {
        changed = false;
        for &b in &post {
            let live = block_live_in(f, b, &dirty_in, &dirty_end, &dense, &live_in, ng, &preempt);
            if live != live_in[b as usize] {
                live_in[b as usize] = live;
                changed = true;
            }
        }
    }
    Analysis {
        dirty_in,
        dirty_end,
        dense,
        global,
        live_in,
        preempt,
    }
}

fn add_mask(set: &mut BitSet, mask: StateMask) {
    for v in 0..NUM_STATE {
        if mask >> v & 1 != 0 {
            set.insert(v as usize);
        }
    }
}

/// Live set at the end of a block's instructions (after the terminator's
/// effects are accounted for).
fn term_live(
    f: &Function,
    b: BlockId,
    dirty_end: StateMask,
    dense: &[u32],
    live_in: &[BitSet],
    ng: usize,
    preempt: &[bool],
) -> BitSet {
    let blk = &f.blocks[b as usize];
    let mut live = BitSet::new(ng);
    match &blk.term {
        Term::Call { cont, .. } => {
            // State live after the call is reloaded (except what the call
            // preserves); temporaries survive.
            let mut l = live_in[*cont as usize].clone();
            let keep = call_preserved(f);
            for v in 0..NUM_STATE {
                if keep >> v & 1 == 0 {
                    l.remove(v as usize);
                }
            }
            live.union(&l);
            add_mask(&mut live, dirty_end & term_sync(f, b));
        }
        t => {
            for s in t.successors() {
                live.union(&live_in[s as usize]);
            }
            if t.syncs() {
                add_mask(&mut live, dirty_end & term_sync(f, b));
            } else if preempt[b as usize] {
                add_mask(&mut live, dirty_end);
            }
        }
    }
    for u in blk.term.uses() {
        let d = dense[u as usize];
        if d != u32::MAX {
            live.insert(d as usize);
        }
    }
    live.insert(CPU as usize);
    live
}

#[allow(clippy::too_many_arguments)]
fn block_live_in(
    f: &Function,
    b: BlockId,
    dirty_in: &[StateMask],
    dirty_end: &[StateMask],
    dense: &[u32],
    live_in: &[BitSet],
    ng: usize,
    preempt: &[bool],
) -> BitSet {
    let blk = &f.blocks[b as usize];
    let mut live = term_live(f, b, dirty_end[b as usize], dense, live_in, ng, preempt);
    let trace = dirty_trace(blk, dirty_in[b as usize]);
    for (k, inst) in blk.insts.iter().enumerate().rev() {
        // Strong liveness: a pure instruction whose result is dead does not
        // make its operands live (a shift by a variable count reads the old
        // flags, which matters only when its own flags are read).
        let mut dead = false;
        if let Some(d) = inst.dst {
            let dd = dense[d as usize];
            if dd != u32::MAX {
                dead = !live.contains(dd as usize)
                    && !inst.op.has_side_effects()
                    && !inst.op.may_fault();
                live.remove(dd as usize);
            }
        }
        if dead {
            continue;
        }
        for u in inst.op.uses() {
            let du = dense[u as usize];
            if du != u32::MAX {
                live.insert(du as usize);
            }
        }
        if inst.op.may_fault() {
            add_mask(&mut live, trace[k] & FAULT_SYNC);
        }
    }
    live
}

/// Removes instructions whose results are never used. Repeats until stable.
pub fn dce(f: &mut Function) -> usize {
    let mut removed = 0;
    loop {
        let a = analyze(f);
        let ng = a.global.len();
        let mut round = 0;
        for b in 0..f.blocks.len() {
            let blk = &f.blocks[b];
            let mut live_g = term_live(
                f,
                b as BlockId,
                a.dirty_end[b],
                &a.dense,
                &a.live_in,
                ng,
                &a.preempt,
            );
            // Local liveness for temporaries that never leave the block.
            let mut live_local: std::collections::HashSet<V> = std::collections::HashSet::new();
            for u in blk.term.uses() {
                if a.dense[u as usize] == u32::MAX {
                    live_local.insert(u);
                }
            }
            let trace = dirty_trace(blk, a.dirty_in[b]);
            let mut keep = vec![true; blk.insts.len()];
            for (k, inst) in blk.insts.iter().enumerate().rev() {
                let is_live = |v: V, lg: &BitSet, ll: &std::collections::HashSet<V>| {
                    let d = a.dense[v as usize];
                    if d != u32::MAX {
                        lg.contains(d as usize)
                    } else {
                        ll.contains(&v)
                    }
                };
                let needed = inst.op.has_side_effects()
                    || inst.dst.is_none()
                    || is_live(inst.dst.unwrap(), &live_g, &live_local);
                if !needed {
                    keep[k] = false;
                    round += 1;
                    continue;
                }
                if let Some(d) = inst.dst {
                    let dd = a.dense[d as usize];
                    if dd != u32::MAX {
                        live_g.remove(dd as usize);
                    } else {
                        live_local.remove(&d);
                    }
                }
                for u in inst.op.uses() {
                    let du = a.dense[u as usize];
                    if du != u32::MAX {
                        live_g.insert(du as usize);
                    } else {
                        live_local.insert(u);
                    }
                }
                if inst.op.may_fault() {
                    add_mask(&mut live_g, trace[k] & FAULT_SYNC);
                }
            }
            if round > 0 {
                let mut k = 0;
                f.blocks[b].insts.retain(|_| {
                    let r = keep[k];
                    k += 1;
                    r
                });
            }
        }
        removed += round;
        if round == 0 {
            break;
        }
    }
    removed
}

/// Folds `addr + const` into the memory operand's offset when the constant
/// is small, saving an add per access.
pub fn fold_address_offsets(f: &mut Function) {
    let mut defs: HashMap<V, (V, u32)> = HashMap::new();
    let mut consts: HashMap<V, u64> = HashMap::new();
    for b in &f.blocks {
        for inst in &b.insts {
            if let (Some(d), Op::Const(c)) = (inst.dst, &inst.op) {
                if d >= NUM_STATE {
                    consts.insert(d, *c);
                }
            }
        }
    }
    for b in &f.blocks {
        for inst in &b.insts {
            if let (Some(d), Op::Bin(op @ (BinOp::I32Add | BinOp::I64Add), x, y)) =
                (inst.dst, &inst.op)
            {
                if d < NUM_STATE {
                    continue;
                }
                if let Some(&c) = consts.get(y) {
                    let small = match op {
                        BinOp::I32Add => (c as u32) < 0x1000,
                        _ => c < 0x1000,
                    };
                    if small && *x >= NUM_STATE {
                        defs.insert(d, (*x, c as u32));
                    }
                }
            }
        }
    }
    // Only fold when the base is a temporary (single definition), so it is
    // still valid at the access.
    for b in &mut f.blocks {
        for inst in &mut b.insts {
            let (addr, mem) = match &mut inst.op {
                Op::Load { addr, mem } | Op::Store { addr, mem, .. } => (addr, mem),
                _ => continue,
            };
            if mem.atomic || mem.space == Space::Native {
                continue;
            }
            if let Some(&(base, off)) = defs.get(addr) {
                *addr = base;
                mem.offset = mem.offset.wrapping_add(off);
            }
        }
    }
}

/// Peepholes that do the same work with narrower WebAssembly:
///
/// * an 8- or 16-bit load whose only use is a sign extension (`movsx`)
///   becomes a signed load;
/// * the low half of a 64-bit product of two extended 32-bit values
///   (`imul`, which computes the full product for its flags) becomes a
///   32-bit multiply; the 64-bit one dies if the flags are unused.
///
/// Relies on temporaries having a single definition, like [`simplify`].
pub fn narrow(f: &mut Function) {
    let n = f.vtypes.len();
    let mut uses = vec![0u32; n];
    let mut def: Vec<Option<(usize, usize)>> = vec![None; n];
    for (bi, b) in f.blocks.iter().enumerate() {
        for (k, inst) in b.insts.iter().enumerate() {
            for u in inst.op.uses() {
                uses[u as usize] += 1;
            }
            if let Some(d) = inst.dst {
                if d >= NUM_STATE {
                    def[d as usize] = Some((bi, k));
                }
            }
        }
        for u in b.term.uses() {
            uses[u as usize] += 1;
        }
    }
    let op_of = |f: &Function, v: V| -> Option<Op> {
        def.get(v as usize)
            .copied()
            .flatten()
            .map(|(b, k)| f.blocks[b].insts[k].op.clone())
    };
    let mut signed_loads = vec![];
    for bi in 0..f.blocks.len() {
        for k in 0..f.blocks[bi].insts.len() {
            let new = match f.blocks[bi].insts[k].op {
                Op::Un(ext @ (UnOp::I32Extend8S | UnOp::I32Extend16S), t)
                    if t >= NUM_STATE
                        && uses[t as usize] == 1
                        && f.vtypes[t as usize] == Ty::I32 =>
                {
                    let size = if ext == UnOp::I32Extend8S { 1 } else { 2 };
                    match op_of(f, t) {
                        Some(Op::Load { mem, .. })
                            if mem.size == size && !mem.signed && !mem.atomic =>
                        {
                            signed_loads.push(def[t as usize].unwrap());
                            Some(Op::Copy(t))
                        }
                        _ => None,
                    }
                }
                Op::Un(UnOp::I32WrapI64, x) => match op_of(f, x) {
                    Some(Op::Bin(BinOp::I64Mul, a, b)) => {
                        let ext = |v: V| match op_of(f, v) {
                            Some(Op::Un(UnOp::I64ExtendI32S | UnOp::I64ExtendI32U, s)) => Some(s),
                            _ => None,
                        };
                        match (ext(a), ext(b)) {
                            (Some(a), Some(b)) => Some(Op::Bin(BinOp::I32Mul, a, b)),
                            _ => None,
                        }
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some(op) = new {
                f.blocks[bi].insts[k].op = op;
            }
        }
    }
    for (b, k) in signed_loads {
        if let Op::Load { mem, .. } = &mut f.blocks[b].insts[k].op {
            mem.signed = true;
        }
    }
}

/// Runs the standard pipeline.
pub fn optimize(f: &mut Function, level: u32) {
    lower_flags(f);
    if level == 0 {
        dce(f);
        return;
    }
    simplify(f);
    forward_x87(f);
    dce(f);
    simplify(f);
    propagate_state_copies(f);
    fold_address_offsets(f);
    narrow(f);
    dce(f);
}

// ---- x87 registers ------------------------------------------------------

/// Where a value stands relative to the x87 stack top at the start of the
/// block, `B` (0..=7).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TopRel {
    /// `(B + k) & 7`
    Top(i32),
    /// `B + k`, not yet masked
    Raw(i32),
    /// `((B + k) & 7) << 3`, the offset of st(k) in the register file
    Off(i32),
    /// The CPU struct pointer plus `Off(k)`: st(k)'s address
    Slot(i32),
}

/// x87 registers live in the CPU struct, addressed through the stack top
/// (`lift::fpu`): each instruction loads its operands from there and stores
/// its result back, and pushes and pops rewrite the tag word. Within a
/// block the stack top is the block's starting top plus a known amount, so
/// accesses to the same register are known for what they are: a load of a
/// register the block already loaded or stored becomes a copy of that
/// value, and a store that a later store to the same register replaces,
/// with nothing in between that reads it or can fault (so a fault still
/// sees every register as the instructions before it left them), is
/// removed. The tag word, at a fixed offset, likewise.
pub fn forward_x87(f: &mut Function) {
    let mut consts = vec![None; f.vtypes.len()];
    for b in &f.blocks {
        for inst in &b.insts {
            if let (Some(d), Op::Const(c)) = (inst.dst, &inst.op) {
                if d >= NUM_STATE {
                    consts[d as usize] = Some(*c as i64 as i32);
                }
            }
        }
    }
    for b in 0..f.blocks.len() {
        forward_x87_block(f, b, &consts);
    }
}

fn forward_x87_block(f: &mut Function, b: usize, consts: &[Option<i32>]) {
    use std::collections::HashMap;
    const ST_END: u32 = cpu::FPU_ST + 64;
    // Register file slots 0..8 (st(k) for k mod 8), and the tag word as 8.
    const TAG: usize = 8;
    let mut rel: HashMap<V, TopRel> = HashMap::new();
    let mut top = Some(TopRel::Top(0));
    // The value each slot holds (as of the last load or store of it) and the
    // last store to it that nothing has read or depended on since.
    let mut known: [Option<V>; 9] = [None; 9];
    let mut pending: [Option<usize>; 9] = [None; 9];
    let mut dead = vec![false; f.blocks[b].insts.len()];
    let c = |v: V| consts.get(v as usize).copied().flatten();
    for i in 0..f.blocks[b].insts.len() {
        let inst = &f.blocks[b].insts[i];
        let get = |v: V, rel: &HashMap<V, TopRel>, top: Option<TopRel>| if v == FPU_TOP { top } else { rel.get(&v).copied() };
        // The slot a native access touches: Some(Some(s)) a known slot,
        // Some(None) somewhere in the x87 area, None elsewhere.
        let slot_of = |addr: V, mem: &Mem, rel: &HashMap<V, TopRel>, top| -> Option<Option<usize>> {
            if mem.space != Space::Native {
                return None;
            }
            if let Some(TopRel::Slot(k)) = get(addr, rel, top) {
                return Some((mem.offset == cpu::FPU_ST && mem.size == 8).then(|| k.rem_euclid(8) as usize));
            }
            if addr != CPU {
                return Some(None);
            }
            let (lo, hi) = (mem.offset, mem.offset + mem.size as u32);
            if (lo, hi) == (cpu::FPU_TAG, cpu::FPU_TAG + 1) {
                return Some(Some(TAG));
            }
            let overlaps = |a: u32, b: u32| lo < b && a < hi;
            (overlaps(cpu::FPU_TAG, cpu::FPU_TAG + 1) || overlaps(cpu::FPU_ST, ST_END)).then_some(None)
        };
        let mut replace = None;
        match &inst.op {
            Op::Copy(a) => {
                if let (Some(d), Some(r)) = (inst.dst, get(*a, &rel, top)) {
                    rel.insert(d, r);
                }
            }
            Op::Bin(op, a, bv) => {
                let (ra, rb) = (get(*a, &rel, top), get(*bv, &rel, top));
                let r = match (op, ra, rb, c(*bv)) {
                    (BinOp::I32Add, Some(TopRel::Top(k) | TopRel::Raw(k)), _, Some(n)) => Some(TopRel::Raw(k + n)),
                    (BinOp::I32Sub, Some(TopRel::Top(k) | TopRel::Raw(k)), _, Some(n)) => Some(TopRel::Raw(k - n)),
                    (BinOp::I32And, Some(TopRel::Top(k) | TopRel::Raw(k)), _, Some(7)) => Some(TopRel::Top(k)),
                    (BinOp::I32Shl, Some(TopRel::Top(k)), _, Some(3)) => Some(TopRel::Off(k)),
                    (BinOp::I32Add | BinOp::I64Add, _, Some(TopRel::Off(k)), _) if *a == CPU => Some(TopRel::Slot(k)),
                    (BinOp::I32Add | BinOp::I64Add, Some(TopRel::Off(k)), _, _) if *bv == CPU => Some(TopRel::Slot(k)),
                    _ => None,
                };
                if let (Some(d), Some(r)) = (inst.dst, r) {
                    rel.insert(d, r);
                }
            }
            Op::Un(UnOp::I64ExtendI32U, a) => {
                if let (Some(d), Some(r @ TopRel::Off(_))) = (inst.dst, get(*a, &rel, top)) {
                    rel.insert(d, r);
                }
            }
            Op::Load { addr, mem } => match slot_of(*addr, mem, &rel, top) {
                Some(Some(s)) => {
                    match known[s] {
                        Some(v) => replace = Some(Op::Copy(v)),
                        None => known[s] = inst.dst,
                    }
                    pending[s] = None;
                }
                Some(None) => {
                    known = [None; 9];
                    pending = [None; 9];
                }
                None => {}
            },
            Op::Store { addr, val, mem } => match slot_of(*addr, mem, &rel, top) {
                Some(Some(s)) => {
                    if let Some(p) = pending[s] {
                        dead[p] = true;
                    }
                    pending[s] = Some(i);
                    known[s] = Some(*val);
                }
                Some(None) => {
                    known = [None; 9];
                    pending = [None; 9];
                }
                None => {}
            },
            // Helpers may read or write the CPU struct.
            Op::CallHelper(..) => {
                known = [None; 9];
                pending = [None; 9];
            }
            _ => {}
        }
        if inst.op.may_fault() || matches!(&inst.op, Op::Bin(op, ..) if op.can_trap()) {
            pending = [None; 9];
        }
        if inst.dst == Some(FPU_TOP) {
            top = match &inst.op {
                Op::Copy(a) => get(*a, &rel, top).filter(|r| matches!(r, TopRel::Top(_))),
                _ => None,
            };
        }
        if let Some(op) = replace {
            f.blocks[b].insts[i].op = op;
        }
    }
    if dead.iter().any(|&d| d) {
        let mut i = 0;
        f.blocks[b].insts.retain(|_| {
            i += 1;
            !dead[i - 1]
        });
    }
}

/// Across blocks, a use of a state vreg that on every path holds a copy of
/// a single-definition temporary (`esi = v777` in a dominating block, not
/// redefined since) reads the temporary instead. Code generation knows
/// more about temporaries (address aliases and checks, see
/// `codegen::check_covered`); the state vreg stays as it was for write-back.
pub fn propagate_state_copies(f: &mut Function) {
    let n = f.blocks.len();
    let mut defs = vec![0u32; f.vtypes.len()];
    for b in &f.blocks {
        for inst in &b.insts {
            if let Some(d) = inst.dst {
                defs[d as usize] += 1;
            }
        }
    }
    let single = |v: V| v >= NUM_STATE && defs[v as usize] == 1;
    type Copies = Option<Vec<(V, V)>>; // None: not yet reached
    let step = |b: &Block, mut m: Vec<(V, V)>| -> Vec<(V, V)> {
        for inst in &b.insts {
            if let Some(d) = inst.dst {
                if d < NUM_STATE {
                    m.retain(|&(s, _)| s != d);
                    if let Op::Copy(t) = inst.op {
                        if single(t) {
                            m.push((d, t));
                        }
                    }
                }
            }
        }
        m
    };
    let order = f.rpo();
    let preds = f.predecessors();
    let mut inn: Vec<Copies> = vec![None; n];
    inn[0] = Some(vec![]);
    let mut changed = true;
    while changed {
        changed = false;
        for &b in &order {
            let m = if b == 0 {
                Some(vec![])
            } else {
                let mut acc: Copies = None;
                for &p in &preds[b as usize] {
                    let Some(pin) = &inn[p as usize] else {
                        continue;
                    };
                    // After a call the state is reloaded.
                    let out = if f.blocks[p as usize].term.clobbers_state() {
                        vec![]
                    } else {
                        step(&f.blocks[p as usize], pin.clone())
                    };
                    acc = Some(match acc {
                        None => out,
                        Some(a) => a.into_iter().filter(|x| out.contains(x)).collect(),
                    });
                }
                acc
            };
            if m.is_some() && m != inn[b as usize] {
                inn[b as usize] = m;
                changed = true;
            }
        }
    }
    for b in 0..n {
        let Some(mut m) = inn[b].clone() else {
            continue;
        };
        let blk = &mut f.blocks[b];
        for inst in &mut blk.insts {
            for u in inst.op.uses_mut() {
                if let Some(&(_, t)) = m.iter().find(|&&(s, _)| s == *u) {
                    *u = t;
                }
            }
            if let Some(d) = inst.dst {
                if d < NUM_STATE {
                    m.retain(|&(s, _)| s != d);
                    if let Op::Copy(t) = inst.op {
                        if single(t) {
                            m.push((d, t));
                        }
                    }
                }
            }
        }
        for u in blk.term.uses_mut() {
            if let Some(&(_, t)) = m.iter().find(|&&(s, _)| s == *u) {
                *u = t;
            }
        }
    }
}

pub fn flag_kind_count() -> u32 {
    fl::NUM_OPS
}
