//! Layer 5 — Optimize.
//!
//! * [`lower_flags`]: replaces `Cond`/`Eflags` reads with direct expressions
//!   when the flag kind is known statically (fusing compare-and-branch), and
//!   with a generic helper call otherwise.
//! * [`simplify`]: constant folding, copy propagation and algebraic
//!   identities.
//! * [`dce`]: whole-function liveness and dead-code elimination, which is
//!   what removes flag computations nobody reads.
//!
//! [`analyze`] computes the facts code generation needs: which state vregs
//! are dirty (differ from the CPU struct) at each point and which are live.

use std::collections::HashMap;

use crate::abi::flags as fl;
use crate::flags::{self, E};
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
                        Kind::Known(kind) => Some(flags::cond(*cc, kind)),
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
                                op: Op::CallHelper(Helper::EvalCond, vec![c, FK, FR, FA, FB, FC]),
                                eip: inst.eip,
                            });
                        }
                    }
                }
                (Op::Eflags, Some(dst)) => match k {
                    Kind::Known(kind) => {
                        let v = emit_expr(f, &mut out, &flags::eflags(kind), inst.eip);
                        out.push(Inst {
                            dst: Some(dst),
                            op: Op::Copy(v),
                            eip: inst.eip,
                        });
                    }
                    _ => out.push(Inst {
                        dst: Some(dst),
                        op: Op::CallHelper(Helper::Eflags, vec![FK, FR, FA, FB, FC]),
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

fn emit_expr(f: &mut Function, out: &mut Vec<Inst>, e: &E, eip: u32) -> V {
    let push = |f: &mut Function, op: Op, out: &mut Vec<Inst>| {
        let v = f.new_vreg(Ty::I32);
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
        let mut insts = std::mem::take(&mut f.blocks[bi].insts);
        for inst in insts.iter_mut() {
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
                    .map(Op::Const),
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
            if let Some(d) = inst.dst {
                // Invalidate facts that depend on the redefined vreg.
                lconst.remove(&d);
                lcopy.remove(&d);
                if d < NUM_STATE {
                    lcopy.retain(|_, s| *s != d);
                }
                match inst.op {
                    Op::Const(c) => {
                        lconst.insert(d, c);
                        // Later uses of a state vreg can read the temporary
                        // instead, letting the state write die.
                        if let Some(src) = copy_src {
                            if d < NUM_STATE && src >= NUM_STATE {
                                lcopy.insert(d, src);
                            }
                        }
                    }
                    Op::Copy(s) if s != d => {
                        lcopy.insert(d, s);
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
        // A branch on a constant becomes a jump.
        if let Term::Branch { cond, t, f: fb } = &term {
            let c = gconst.get(cond).or_else(|| lconst.get(cond)).copied();
            if let Some(c) = c {
                term = Term::Jump(if c as u32 != 0 { *t } else { *fb });
            }
        }
        f.blocks[bi].term = term;
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
        (I64Or | I64Xor | I64Shl | I64ShrU | I64Add, _, Some(0)) if cb == Some(0) => {
            Some(Op::Copy(a))
        }
        _ => None,
    }
}

// ---- Analysis -----------------------------------------------------------------

/// State vregs as a bit mask.
pub type StateMask = u32;

pub fn state_bit(v: V) -> StateMask {
    if v < NUM_STATE {
        1 << v
    } else {
        0
    }
}

/// State written back at fault points: everything except the lazy flag
/// state, whose precision at faults we do not guarantee.
pub const FAULT_SYNC: StateMask = !(1 << FK | 1 << FR | 1 << FA | 1 << FB | 1 << FC | 1 << CPU);
pub const ALL_STATE: StateMask = !(1 << CPU);

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
                m |= 1 << v;
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

pub fn analyze(f: &Function) -> Analysis {
    let n = f.blocks.len();
    let order = f.rpo();
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
            let live = block_live_in(f, b, &dirty_in, &dirty_end, &dense, &live_in, ng);
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
) -> BitSet {
    let blk = &f.blocks[b as usize];
    let mut live = BitSet::new(ng);
    match &blk.term {
        Term::Call { cont, .. } => {
            // State live after the call is reloaded; temporaries survive.
            let mut l = live_in[*cont as usize].clone();
            for v in 0..NUM_STATE {
                l.remove(v as usize);
            }
            live.union(&l);
            add_mask(&mut live, dirty_end);
        }
        t => {
            for s in t.successors() {
                live.union(&live_in[s as usize]);
            }
            if t.syncs() {
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

fn block_live_in(
    f: &Function,
    b: BlockId,
    dirty_in: &[StateMask],
    dirty_end: &[StateMask],
    dense: &[u32],
    live_in: &[BitSet],
    ng: usize,
) -> BitSet {
    let blk = &f.blocks[b as usize];
    let mut live = term_live(f, b, dirty_end[b as usize], dense, live_in, ng);
    let trace = dirty_trace(blk, dirty_in[b as usize]);
    for (k, inst) in blk.insts.iter().enumerate().rev() {
        if let Some(d) = inst.dst {
            let dd = dense[d as usize];
            if dd != u32::MAX {
                live.remove(dd as usize);
            }
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
            let mut live_g = term_live(f, b as BlockId, a.dirty_end[b], &a.dense, &a.live_in, ng);
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
            if let (Some(d), Op::Bin(BinOp::I32Add, x, y)) = (inst.dst, &inst.op) {
                if d < NUM_STATE {
                    continue;
                }
                if let Some(&c) = consts.get(y) {
                    if (c as u32) < 0x1000 && *x >= NUM_STATE {
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

/// Runs the standard pipeline.
pub fn optimize(f: &mut Function, level: u32) {
    lower_flags(f);
    if level == 0 {
        dce(f);
        return;
    }
    simplify(f);
    dce(f);
    simplify(f);
    fold_address_offsets(f);
    dce(f);
}

pub fn flag_kind_count() -> u32 {
    fl::NUM_OPS
}
