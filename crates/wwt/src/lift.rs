//! Layer 4 — Lift x86 instructions into the IR.
//!
//! Every instruction becomes explicit operations on registers, flags and
//! memory. This is where x86 semantics live. Values narrower than 32 bits
//! are kept zero-extended in i32 vregs.

use std::collections::{BTreeMap, HashMap, VecDeque};

use iced_x86::{ConditionCode, Instruction, Mnemonic, OpKind, Register};

use crate::abi::{fault, flags as fl};
use crate::discover::{CodeSource, Discovery};
use crate::ir::*;

mod fpu;
mod simd;

#[derive(Debug, Clone)]
pub struct LiftConfig {
    /// Emit null-region/guest-limit checks on accesses the translator cannot
    /// prove valid.
    pub mem_checks: bool,
    /// Emit code-page checks on stores that might modify translated code.
    pub smc_checks: bool,
    /// Make every guest memory access atomic (strict memory ordering).
    pub strict_ordering: bool,
    /// Upper bound on x86 basic blocks per function before splitting.
    pub max_blocks: usize,
}

impl Default for LiftConfig {
    fn default() -> Self {
        LiftConfig {
            mem_checks: true,
            smc_checks: true,
            strict_ordering: false,
            max_blocks: 4000,
        }
    }
}

/// The result of lifting one function.
pub struct Lifted {
    pub func: Function,
    /// Addresses the function exits to that are not known function entries
    /// (split points). The translator turns them into entries.
    pub extra_entries: Vec<u32>,
    /// Instructions that could not be lifted: (address, text).
    pub unsupported: Vec<(u32, String)>,
}

/// Where an operand lives.
#[derive(Clone, Copy)]
enum Loc {
    Reg(Register),
    Mem { addr: V, mem: Mem },
    Imm(u32),
}

struct Lifter<'a> {
    f: Function,
    cur: BlockId,
    cfg: &'a LiftConfig,
    src: &'a dyn CodeSource,
    disc: &'a Discovery,
    /// IR block for each x86 block start in the function.
    blocks: &'a HashMap<u32, BlockId>,
    entry: u32,
    eip: u32,
    next: u32,
    /// For each GPR, the address vreg it was last loaded from in the current
    /// x86 block (used to lower jump tables reached through a register).
    loaded_from: [Option<V>; 8],
    unsupported: Vec<(u32, String)>,
    extra_entries: Vec<u32>,
}

fn width_mask(w: u32) -> u32 {
    if w >= 32 {
        u32::MAX
    } else {
        (1 << w) - 1
    }
}

fn gpr_index(r: Register) -> u32 {
    r.full_register32().number() as u32
}

fn is_high8(r: Register) -> bool {
    matches!(r, Register::AH | Register::CH | Register::DH | Register::BH)
}

fn cc_of(c: ConditionCode) -> Cc {
    match c {
        ConditionCode::o => Cc::O,
        ConditionCode::no => Cc::NO,
        ConditionCode::b => Cc::B,
        ConditionCode::ae => Cc::AE,
        ConditionCode::e => Cc::E,
        ConditionCode::ne => Cc::NE,
        ConditionCode::be => Cc::BE,
        ConditionCode::a => Cc::A,
        ConditionCode::s => Cc::S,
        ConditionCode::ns => Cc::NS,
        ConditionCode::p => Cc::P,
        ConditionCode::np => Cc::NP,
        ConditionCode::l => Cc::L,
        ConditionCode::ge => Cc::GE,
        ConditionCode::le => Cc::LE,
        ConditionCode::g => Cc::G,
        ConditionCode::None => unreachable!("no condition code"),
    }
}

/// Lifts the function starting at `entry`.
pub fn lift_function(
    src: &dyn CodeSource,
    disc: &Discovery,
    entry: u32,
    cfg: &LiftConfig,
) -> Lifted {
    // Collect the function's x86 blocks: everything reachable from the entry
    // through jumps, branches and call continuations, without entering
    // other functions.
    let mut order: Vec<u32> = vec![];
    let mut seen: BTreeMap<u32, ()> = BTreeMap::new();
    let mut work = VecDeque::from([entry]);
    let mut extra = vec![];
    while let Some(b) = work.pop_front() {
        if seen.contains_key(&b) {
            continue;
        }
        if b != entry && disc.functions.contains(&b) {
            continue;
        }
        if order.len() >= cfg.max_blocks {
            extra.push(b);
            continue;
        }
        seen.insert(b, ());
        order.push(b);
        for s in x86_successors(src, disc, b) {
            work.push_back(s);
        }
    }
    let mut f = Function::new(entry);
    let mut blocks = HashMap::new();
    for &b in &order {
        let id = f.new_block(b);
        blocks.insert(b, id);
    }
    let mut l = Lifter {
        f,
        cur: 0,
        cfg,
        src,
        disc,
        blocks: &blocks,
        entry,
        eip: entry,
        next: entry,
        loaded_from: [None; 8],
        unsupported: vec![],
        extra_entries: vec![],
    };
    for &b in &order {
        l.lift_block(b);
    }
    let mut extra_entries = l.extra_entries;
    extra_entries.extend(extra.into_iter().filter(|e| !disc.functions.contains(e)));
    extra_entries.sort_unstable();
    extra_entries.dedup();
    Lifted {
        func: l.f,
        extra_entries,
        unsupported: l.unsupported,
    }
}

/// Intra-procedural successors of an x86 block.
fn x86_successors(src: &dyn CodeSource, disc: &Discovery, start: u32) -> Vec<u32> {
    let _ = src;
    let (insts, fall) = disc.block(start);
    let mut out = vec![];
    if let Some(last) = insts.last() {
        use iced_x86::FlowControl as FC;
        match last.flow_control() {
            FC::ConditionalBranch | FC::UnconditionalBranch => {
                if last.op0_kind() == OpKind::NearBranch32 {
                    out.push(last.near_branch32());
                }
            }
            FC::Call => {
                if last.op0_kind() == OpKind::NearBranch32
                    && last.near_branch32() == last.next_ip32()
                {
                    // get-PC idiom: falls through below.
                }
            }
            FC::IndirectBranch => {
                if let Some(jt) = disc.jump_tables.get(&last.ip32()) {
                    out.extend(jt.targets.iter().copied());
                }
            }
            _ => {}
        }
    }
    if let Some(n) = fall {
        out.push(n);
    }
    out
}

impl<'a> Lifter<'a> {
    // ---- Emission helpers ------------------------------------------------

    fn push_inst(&mut self, dst: Option<V>, op: Op) {
        let eip = self.eip;
        self.f.blocks[self.cur as usize]
            .insts
            .push(Inst { dst, op, eip });
    }

    fn emit(&mut self, ty: Ty, op: Op) -> V {
        let v = self.f.new_vreg(ty);
        self.push_inst(Some(v), op);
        v
    }

    fn emit_to(&mut self, dst: V, op: Op) {
        self.push_inst(Some(dst), op);
    }

    fn effect(&mut self, op: Op) {
        self.push_inst(None, op);
    }

    fn c32(&mut self, v: u32) -> V {
        self.emit(Ty::I32, Op::Const(v as u64))
    }

    fn c64(&mut self, v: u64) -> V {
        self.emit(Ty::I64, Op::Const(v))
    }

    fn bin(&mut self, op: BinOp, a: V, b: V) -> V {
        self.emit(op.result_ty(), Op::Bin(op, a, b))
    }

    fn bini(&mut self, op: BinOp, a: V, imm: u32) -> V {
        let b = self.c32(imm);
        self.bin(op, a, b)
    }

    fn un(&mut self, op: UnOp, a: V) -> V {
        self.emit(op.result_ty(), Op::Un(op, a))
    }

    fn select(&mut self, cond: V, t: V, f: V) -> V {
        let ty = self.f.ty(t);
        self.emit(ty, Op::Select { cond, t, f })
    }

    fn copy(&mut self, v: V) -> V {
        let ty = self.f.ty(v);
        self.emit(ty, Op::Copy(v))
    }

    fn mask(&mut self, v: V, w: u32) -> V {
        if w >= 32 {
            v
        } else {
            self.bini(BinOp::I32And, v, width_mask(w))
        }
    }

    fn sext(&mut self, v: V, w: u32) -> V {
        match w {
            8 => self.un(UnOp::I32Extend8S, v),
            16 => self.un(UnOp::I32Extend16S, v),
            _ => v,
        }
    }

    fn sign_bit(&mut self, v: V, w: u32) -> V {
        let s = self.bini(BinOp::I32ShrU, v, w - 1);
        self.bini(BinOp::I32And, s, 1)
    }

    fn is_zero(&mut self, v: V) -> V {
        self.un(UnOp::I32Eqz, v)
    }

    fn not1(&mut self, v: V) -> V {
        self.bini(BinOp::I32Xor, v, 1)
    }

    // ---- Registers -------------------------------------------------------

    fn read_reg(&mut self, r: Register) -> V {
        match r.size() {
            4 => self.copy(gpr_index(r)),
            2 => {
                let full = gpr_index(r);
                self.bini(BinOp::I32And, full, 0xffff)
            }
            1 => {
                let full = gpr_index(r);
                if is_high8(r) {
                    let s = self.bini(BinOp::I32ShrU, full, 8);
                    self.bini(BinOp::I32And, s, 0xff)
                } else {
                    self.bini(BinOp::I32And, full, 0xff)
                }
            }
            _ => panic!("unsupported register {r:?}"),
        }
    }

    fn write_reg(&mut self, r: Register, v: V) {
        let full = gpr_index(r);
        self.loaded_from[full as usize] = None;
        match r.size() {
            4 => self.emit_to(full, Op::Copy(v)),
            2 => {
                let hi = self.bini(BinOp::I32And, full, 0xffff_0000);
                let lo = self.mask(v, 16);
                self.emit_to(full, Op::Bin(BinOp::I32Or, hi, lo));
            }
            1 => {
                let lo = self.mask(v, 8);
                if is_high8(r) {
                    let keep = self.bini(BinOp::I32And, full, 0xffff_00ff);
                    let sh = self.bini(BinOp::I32Shl, lo, 8);
                    self.emit_to(full, Op::Bin(BinOp::I32Or, keep, sh));
                } else {
                    let keep = self.bini(BinOp::I32And, full, 0xffff_ff00);
                    self.emit_to(full, Op::Bin(BinOp::I32Or, keep, lo));
                }
            }
            _ => panic!("unsupported register {r:?}"),
        }
    }

    fn set_gpr(&mut self, idx: V, v: V) {
        self.loaded_from[idx as usize] = None;
        self.emit_to(idx, Op::Copy(v));
    }

    // ---- Memory ----------------------------------------------------------

    /// Effective address of the instruction's memory operand.
    fn ea(&mut self, i: &Instruction) -> (V, Space) {
        let base = i.memory_base();
        let index = i.memory_index();
        let disp = i.memory_displacement32();
        let mut addr: Option<V> = None;
        if base != Register::None {
            addr = Some(self.copy(gpr_index(base)));
        }
        if index != Register::None {
            let mut ix = self.copy(gpr_index(index));
            let scale = i.memory_index_scale();
            if scale > 1 {
                ix = self.bini(BinOp::I32Shl, ix, scale.trailing_zeros());
            }
            addr = Some(match addr {
                Some(a) => self.bin(BinOp::I32Add, a, ix),
                None => ix,
            });
        }
        let mut a = match addr {
            Some(a) if disp != 0 => self.bini(BinOp::I32Add, a, disp),
            Some(a) => a,
            None => self.c32(disp),
        };
        let seg = i.memory_segment();
        let space = match seg {
            Register::FS | Register::GS => {
                let b = if seg == Register::FS { FS_BASE } else { GS_BASE };
                a = self.bin(BinOp::I32Add, a, b);
                Space::Guest
            }
            _ => {
                if base != Register::None
                    && index == Register::None
                    && matches!(base, Register::ESP | Register::EBP)
                {
                    Space::Trusted
                } else if base == Register::None && index == Register::None {
                    self.static_space(disp)
                } else {
                    Space::Guest
                }
            }
        };
        (a, space)
    }

    /// Space for a constant address: trusted when it lies in the image's
    /// data, checked otherwise.
    fn static_space(&self, addr: u32) -> Space {
        if addr >= crate::abi::addr::NULL_LIMIT
            && !self.src.bytes(addr).is_empty()
            && !self.src.is_code(addr)
        {
            Space::Trusted
        } else {
            Space::Guest
        }
    }

    fn mem(&self, size: u32, space: Space) -> Mem {
        Mem {
            size: size as u8,
            signed: false,
            space,
            offset: 0,
            atomic: self.cfg.strict_ordering && space != Space::Native,
        }
    }

    fn load(&mut self, addr: V, size: u32, space: Space) -> V {
        let ty = if size == 8 { Ty::I64 } else { Ty::I32 };
        let mem = self.mem(size, space);
        self.emit(ty, Op::Load { addr, mem })
    }

    fn store(&mut self, addr: V, val: V, size: u32, space: Space) {
        let mem = self.mem(size, space);
        self.effect(Op::Store { addr, val, mem });
    }

    fn load_native(&mut self, ty: Ty, size: u32, offset: u32) -> V {
        self.emit(
            ty,
            Op::Load {
                addr: CPU,
                mem: Mem::native(size as u8, offset),
            },
        )
    }

    fn store_native(&mut self, val: V, size: u32, offset: u32) {
        self.effect(Op::Store {
            addr: CPU,
            val,
            mem: Mem::native(size as u8, offset),
        });
    }

    // ---- Operands --------------------------------------------------------

    fn op_width(&self, i: &Instruction, n: u32) -> u32 {
        match i.op_kind(n) {
            OpKind::Register => i.op_register(n).size() as u32 * 8,
            OpKind::Memory => i.memory_size().size() as u32 * 8,
            OpKind::Immediate8 => 8,
            OpKind::Immediate16 | OpKind::Immediate8to16 => 16,
            _ => 32,
        }
    }

    fn loc(&mut self, i: &Instruction, n: u32) -> Loc {
        match i.op_kind(n) {
            OpKind::Register => Loc::Reg(i.op_register(n)),
            OpKind::Memory => {
                let (addr, space) = self.ea(i);
                let size = i.memory_size().size() as u32;
                Loc::Mem {
                    addr,
                    mem: self.mem(size, space),
                }
            }
            OpKind::Immediate8
            | OpKind::Immediate16
            | OpKind::Immediate32
            | OpKind::Immediate8to16
            | OpKind::Immediate8to32
            | OpKind::Immediate8_2nd => Loc::Imm(i.immediate(n) as u32),
            k => panic!("unsupported operand kind {k:?}"),
        }
    }

    fn read(&mut self, loc: Loc, w: u32) -> V {
        match loc {
            Loc::Reg(r) => self.read_reg(r),
            Loc::Mem { addr, mem } => {
                let mut m = mem;
                m.size = (w / 8) as u8;
                let ty = if w == 64 { Ty::I64 } else { Ty::I32 };
                self.emit(ty, Op::Load { addr, mem: m })
            }
            Loc::Imm(v) => self.c32(v & width_mask(w)),
        }
    }

    fn write(&mut self, loc: Loc, v: V, w: u32) {
        match loc {
            Loc::Reg(r) => self.write_reg(r, v),
            Loc::Mem { addr, mem } => {
                let mut m = mem;
                m.size = (w / 8) as u8;
                self.effect(Op::Store { addr, val: v, mem: m });
            }
            Loc::Imm(_) => panic!("write to immediate"),
        }
    }

    fn read_op(&mut self, i: &Instruction, n: u32) -> V {
        let w = self.op_width(i, n);
        let l = self.loc(i, n);
        self.read(l, w)
    }

    // ---- Flags -----------------------------------------------------------

    fn set_flags(&mut self, op: u32, w: u32, res: V, a: V, b: Option<V>, c: Option<V>) {
        self.emit_to(FK, Op::Const(fl::kind(op, w) as u64));
        self.emit_to(FR, Op::Copy(res));
        self.emit_to(FA, Op::Copy(a));
        if let Some(b) = b {
            self.emit_to(FB, Op::Copy(b));
        }
        if let Some(c) = c {
            self.emit_to(FC, Op::Copy(c));
        }
    }

    fn set_flags_explicit(&mut self, eflags: V) {
        self.emit_to(FK, Op::Const(fl::EXPLICIT as u64));
        self.emit_to(FR, Op::Copy(eflags));
    }

    /// Sets the flag state only when `cond` is non-zero (shifts by a
    /// variable count leave flags alone when the count is zero).
    fn set_flags_if(
        &mut self,
        cond: V,
        op: u32,
        w: u32,
        res: V,
        a: V,
        b: Option<V>,
        c: Option<V>,
    ) {
        let k = self.c32(fl::kind(op, w));
        self.emit_to(FK, Op::Select { cond, t: k, f: FK });
        self.emit_to(FR, Op::Select { cond, t: res, f: FR });
        self.emit_to(FA, Op::Select { cond, t: a, f: FA });
        if let Some(b) = b {
            self.emit_to(FB, Op::Select { cond, t: b, f: FB });
        }
        if let Some(c) = c {
            self.emit_to(FC, Op::Select { cond, t: c, f: FC });
        }
    }

    fn cond(&mut self, cc: Cc) -> V {
        self.emit(Ty::I32, Op::Cond(cc))
    }

    fn eflags(&mut self) -> V {
        self.emit(Ty::I32, Op::Eflags)
    }

    /// Full eflags as `pushfd` sees it.
    fn full_eflags(&mut self) -> V {
        let arith = self.eflags();
        let df = self.bini(BinOp::I32Shl, DF, 10);
        let sys = self.load_native(Ty::I32, 4, crate::abi::cpu::EFLAGS_SYS);
        let a = self.bin(BinOp::I32Or, arith, df);
        let b = self.bin(BinOp::I32Or, a, sys);
        // Bit 1 is always set.
        self.bini(BinOp::I32Or, b, 2)
    }

    fn set_full_eflags(&mut self, e: V) {
        let arith = self.bini(BinOp::I32And, e, fl::ARITH);
        self.set_flags_explicit(arith);
        let d = self.bini(BinOp::I32ShrU, e, 10);
        let d = self.bini(BinOp::I32And, d, 1);
        self.emit_to(DF, Op::Copy(d));
        // Keep the bits user code may change (TF, AC, ID and friends).
        let sys_mask = !(fl::ARITH | fl::DF | 2) & 0x0024_7fff;
        let sys = self.bini(BinOp::I32And, e, sys_mask);
        self.store_native(sys, 4, crate::abi::cpu::EFLAGS_SYS);
    }

    // ---- Control flow ----------------------------------------------------

    fn block_for(&self, addr: u32) -> Option<BlockId> {
        if addr == self.entry {
            return Some(0);
        }
        if self.disc.functions.contains(&addr) {
            return None;
        }
        self.blocks.get(&addr).copied()
    }

    /// A block that continues at `addr`: the function's own block when the
    /// address belongs to it, otherwise a block that leaves the function.
    fn target_block(&mut self, addr: u32) -> BlockId {
        if let Some(b) = self.block_for(addr) {
            return b;
        }
        let b = self.f.new_block(addr);
        self.f.blocks[b as usize].term = Term::Exit(addr);
        b
    }

    fn terminate(&mut self, t: Term) {
        self.f.blocks[self.cur as usize].term = t;
    }

    /// Starts a new IR block (for control flow inside one instruction).
    fn new_block(&mut self) -> BlockId {
        self.f.new_block(self.eip)
    }

    fn switch_to(&mut self, b: BlockId) {
        self.cur = b;
    }

    fn fault_if(&mut self, cond: V, code: u32, info: V) {
        self.effect(Op::FaultIf { cond, code, info });
    }

    fn unsupported(&mut self, i: &Instruction, why: &str) {
        let text = format!("{i}");
        self.unsupported.push((self.eip, format!("{text} ({why})")));
        self.terminate(Term::Fault {
            code: fault::UNSUPPORTED,
            eip: self.eip,
        });
    }

    // ---- Blocks ----------------------------------------------------------

    fn lift_block(&mut self, start: u32) {
        self.cur = self.blocks[&start];
        self.loaded_from = [None; 8];
        let (insts, fall) = self.disc.block(start);
        if insts.is_empty() {
            self.eip = start;
            self.terminate(Term::Fault {
                code: fault::ILLEGAL_INSTRUCTION,
                eip: start,
            });
            return;
        }
        for i in &insts {
            self.eip = i.ip32();
            self.next = i.next_ip32();
            if !self.lift_inst(i) {
                // The instruction ended the block (control flow or fault).
                return;
            }
        }
        match fall {
            Some(n) => {
                let t = self.target_block(n);
                self.terminate(Term::Jump(t));
            }
            None => {
                let last = insts.last().unwrap();
                self.terminate(Term::Fault {
                    code: fault::ILLEGAL_INSTRUCTION,
                    eip: last.next_ip32(),
                });
            }
        }
    }

    /// Lifts one instruction. Returns false when it terminated the block.
    fn lift_inst(&mut self, i: &Instruction) -> bool {
        use Mnemonic as M;
        let m = i.mnemonic();
        if i.is_privileged() && !matches!(m, M::Hlt) {
            self.terminate(Term::Fault {
                code: fault::PRIVILEGED_INSTRUCTION,
                eip: self.eip,
            });
            return false;
        }
        if fpu::is_fpu(i) {
            return self.lift_fpu(i);
        }
        match m {
            M::Nop | M::Pause | M::Lfence | M::Mfence | M::Sfence | M::Prefetchnta
            | M::Prefetcht0 | M::Prefetcht1 | M::Prefetcht2 | M::Prefetchw | M::Endbr32 => {}
            M::Mov => self.lift_mov(i),
            M::Movzx | M::Movsx => {
                let sw = self.op_width(i, 1);
                let dw = self.op_width(i, 0);
                let v = self.read_op(i, 1);
                let v = if m == M::Movsx {
                    let s = self.sext(v, sw);
                    self.mask(s, dw)
                } else {
                    v
                };
                let d = self.loc(i, 0);
                self.write(d, v, dw);
            }
            M::Lea => {
                let (a, _) = self.ea_no_seg(i);
                let w = self.op_width(i, 0);
                let d = self.loc(i, 0);
                self.write(d, a, w);
            }
            M::Xchg => self.lift_xchg(i),
            M::Bswap => {
                let r = i.op0_register();
                let v = self.read_reg(r);
                let b0 = self.bini(BinOp::I32Shl, v, 24);
                let t1 = self.bini(BinOp::I32And, v, 0xff00);
                let b1 = self.bini(BinOp::I32Shl, t1, 8);
                let t2 = self.bini(BinOp::I32ShrU, v, 8);
                let b2 = self.bini(BinOp::I32And, t2, 0xff00);
                let b3 = self.bini(BinOp::I32ShrU, v, 24);
                let o1 = self.bin(BinOp::I32Or, b0, b1);
                let o2 = self.bin(BinOp::I32Or, b2, b3);
                let res = self.bin(BinOp::I32Or, o1, o2);
                self.write_reg(r, res);
            }
            M::Cmove
            | M::Cmovne
            | M::Cmovb
            | M::Cmovae
            | M::Cmovbe
            | M::Cmova
            | M::Cmovl
            | M::Cmovge
            | M::Cmovle
            | M::Cmovg
            | M::Cmovs
            | M::Cmovns
            | M::Cmovo
            | M::Cmovno
            | M::Cmovp
            | M::Cmovnp => {
                let w = self.op_width(i, 0);
                let c = self.cond(cc_of(i.condition_code()));
                let src = self.read_op(i, 1);
                let r = i.op0_register();
                // A 32-bit cmov always writes (zero-extending in 64-bit mode;
                // a no-op here), but the source is read even when false.
                let old = self.read_reg(r);
                let v = self.select(c, src, old);
                let _ = w;
                self.write_reg(r, v);
            }
            M::Sete
            | M::Setne
            | M::Setb
            | M::Setae
            | M::Setbe
            | M::Seta
            | M::Setl
            | M::Setge
            | M::Setle
            | M::Setg
            | M::Sets
            | M::Setns
            | M::Seto
            | M::Setno
            | M::Setp
            | M::Setnp => {
                let c = self.cond(cc_of(i.condition_code()));
                let d = self.loc(i, 0);
                self.write(d, c, 8);
            }
            M::Push => self.lift_push(i),
            M::Pop => self.lift_pop(i),
            M::Pushad | M::Pusha => {
                let sz = if m == M::Pushad { 4 } else { 2 };
                let old_esp = self.copy(ESP);
                for r in 0..8u32 {
                    let v = if r == ESP { old_esp } else { r };
                    let v = if sz == 2 { self.mask(v, 16) } else { self.copy(v) };
                    self.push_val(v, sz);
                }
            }
            M::Popad | M::Popa => {
                let sz = if m == M::Popad { 4 } else { 2 };
                for r in (0..8u32).rev() {
                    let v = self.pop_val(sz);
                    if r == ESP {
                        continue;
                    }
                    if sz == 4 {
                        self.set_gpr(r, v);
                    } else {
                        let reg = [
                            Register::AX,
                            Register::CX,
                            Register::DX,
                            Register::BX,
                            Register::SP,
                            Register::BP,
                            Register::SI,
                            Register::DI,
                        ][r as usize];
                        self.write_reg(reg, v);
                    }
                }
            }
            M::Pushfd | M::Pushf => {
                let e = self.full_eflags();
                // VM and RF read as zero.
                let e = self.bini(BinOp::I32And, e, 0x00fc_ffff);
                let sz = if m == M::Pushfd { 4 } else { 2 };
                let e = if sz == 2 { self.mask(e, 16) } else { e };
                self.push_val(e, sz);
            }
            M::Popfd | M::Popf => {
                let sz = if m == M::Popfd { 4 } else { 2 };
                let v = self.pop_val(sz);
                if sz == 2 {
                    let old = self.full_eflags();
                    let hi = self.bini(BinOp::I32And, old, 0xffff_0000);
                    let merged = self.bin(BinOp::I32Or, hi, v);
                    self.set_full_eflags(merged);
                } else {
                    self.set_full_eflags(v);
                }
            }
            M::Lahf => {
                let e = self.eflags();
                let e = self.bini(BinOp::I32Or, e, 2);
                let e = self.bini(BinOp::I32And, e, 0xd7);
                self.write_reg(Register::AH, e);
            }
            M::Sahf => {
                let ah = self.read_reg(Register::AH);
                let ah = self.bini(BinOp::I32And, ah, 0xd5);
                let of = self.cond(Cc::O);
                let of = self.bini(BinOp::I32Shl, of, 11);
                let e = self.bin(BinOp::I32Or, ah, of);
                self.set_flags_explicit(e);
            }
            M::Cbw => {
                let v = self.read_reg(Register::AL);
                let v = self.sext(v, 8);
                self.write_reg(Register::AX, v);
            }
            M::Cwde => {
                let v = self.read_reg(Register::AX);
                let v = self.sext(v, 16);
                self.write_reg(Register::EAX, v);
            }
            M::Cwd => {
                let v = self.read_reg(Register::AX);
                let v = self.sext(v, 16);
                let s = self.bini(BinOp::I32ShrS, v, 31);
                self.write_reg(Register::DX, s);
            }
            M::Cdq => {
                let s = self.bini(BinOp::I32ShrS, EAX, 31);
                self.set_gpr(EDX, s);
            }
            M::Add | M::Adc | M::Sub | M::Sbb | M::Cmp | M::And | M::Or | M::Xor | M::Test => {
                self.lift_alu(i)
            }
            M::Inc | M::Dec => self.lift_incdec(i),
            M::Neg => {
                let w = self.op_width(i, 0);
                let d = self.loc(i, 0);
                if i.has_lock_prefix() {
                    // Not atomic: no WebAssembly RMW computes a negation.
                }
                let a = self.read(d, w);
                let z = self.c32(0);
                let r = self.bin(BinOp::I32Sub, z, a);
                let r = self.mask(r, w);
                self.write(d, r, w);
                self.set_flags(fl::NEG, w, r, a, None, None);
            }
            M::Not => {
                let w = self.op_width(i, 0);
                let d = self.loc(i, 0);
                let a = self.read(d, w);
                let r = self.bini(BinOp::I32Xor, a, width_mask(w));
                self.write(d, r, w);
            }
            M::Shl | M::Sal | M::Shr | M::Sar => self.lift_shift(i),
            M::Rol | M::Ror => self.lift_rotate(i),
            M::Rcl | M::Rcr => self.lift_rotate_carry(i),
            M::Shld | M::Shrd => self.lift_shift_double(i),
            M::Mul | M::Imul => self.lift_mul(i),
            M::Div | M::Idiv => self.lift_div(i),
            M::Bt | M::Bts | M::Btr | M::Btc => self.lift_bt(i),
            M::Bsf | M::Bsr | M::Tzcnt | M::Lzcnt | M::Popcnt => self.lift_bitscan(i),
            M::Xadd => self.lift_xadd(i),
            M::Cmpxchg => self.lift_cmpxchg(i),
            M::Cmpxchg8b => self.lift_cmpxchg8b(i),
            M::Clc | M::Stc | M::Cmc => {
                let e = self.eflags();
                let e = match m {
                    M::Clc => self.bini(BinOp::I32And, e, !1),
                    M::Stc => self.bini(BinOp::I32Or, e, 1),
                    _ => self.bini(BinOp::I32Xor, e, 1),
                };
                self.set_flags_explicit(e);
            }
            M::Cld => self.emit_to(DF, Op::Const(0)),
            M::Std => self.emit_to(DF, Op::Const(1)),
            M::Cli | M::Sti => {
                self.terminate(Term::Fault {
                    code: fault::PRIVILEGED_INSTRUCTION,
                    eip: self.eip,
                });
                return false;
            }
            M::Leave => {
                self.set_gpr(ESP, EBP);
                let v = self.pop_val(4);
                self.set_gpr(EBP, v);
            }
            M::Enter => {
                let size = i.immediate16() as u32;
                let level = i.immediate8_2nd() as u32 & 31;
                if level != 0 {
                    self.unsupported(i, "enter with nesting level");
                    return false;
                }
                let ebp = self.copy(EBP);
                self.push_val(ebp, 4);
                self.set_gpr(EBP, ESP);
                let s = self.bini(BinOp::I32Sub, ESP, size);
                self.set_gpr(ESP, s);
            }
            M::Movsb | M::Movsw | M::Movsd | M::Stosb | M::Stosw | M::Stosd | M::Lodsb
            | M::Lodsw | M::Lodsd | M::Cmpsb | M::Cmpsw | M::Cmpsd | M::Scasb | M::Scasw
            | M::Scasd
                if i.is_string_instruction() =>
            {
                return self.lift_string(i);
            }
            M::Xlatb => {
                let al = self.read_reg(Register::AL);
                let a = self.bin(BinOp::I32Add, EBX, al);
                let v = self.load(a, 1, Space::Guest);
                self.write_reg(Register::AL, v);
            }
            M::Cpuid => self.lift_cpuid(),
            M::Rdtsc => {
                // A deterministic counter: advances by 1000 per read.
                let t = self.load_native(Ty::I64, 8, crate::abi::cpu::SCRATCH);
                let k = self.c64(1000);
                let t = self.bin(BinOp::I64Add, t, k);
                self.store_native(t, 8, crate::abi::cpu::SCRATCH);
                let lo = self.un(UnOp::I32WrapI64, t);
                let k32 = self.c64(32);
                let hi = self.bin(BinOp::I64ShrU, t, k32);
                let hi = self.un(UnOp::I32WrapI64, hi);
                self.set_gpr(EAX, lo);
                self.set_gpr(EDX, hi);
            }
            M::Jmp => return self.lift_jmp(i),
            M::Je
            | M::Jne
            | M::Jb
            | M::Jae
            | M::Jbe
            | M::Ja
            | M::Jl
            | M::Jge
            | M::Jle
            | M::Jg
            | M::Js
            | M::Jns
            | M::Jo
            | M::Jno
            | M::Jp
            | M::Jnp => {
                let c = self.cond(cc_of(i.condition_code()));
                self.branch(c, i.near_branch32(), self.next);
                return false;
            }
            M::Jecxz | M::Jcxz => {
                let v = if m == M::Jcxz {
                    self.read_reg(Register::CX)
                } else {
                    self.copy(ECX)
                };
                let c = self.is_zero(v);
                self.branch(c, i.near_branch32(), self.next);
                return false;
            }
            M::Loop | M::Loope | M::Loopne => {
                let n = self.bini(BinOp::I32Sub, ECX, 1);
                self.set_gpr(ECX, n);
                let nz = self.bini(BinOp::I32Ne, n, 0);
                let c = match m {
                    M::Loop => nz,
                    M::Loope => {
                        let z = self.cond(Cc::E);
                        self.bin(BinOp::I32And, nz, z)
                    }
                    _ => {
                        let z = self.cond(Cc::NE);
                        self.bin(BinOp::I32And, nz, z)
                    }
                };
                self.branch(c, i.near_branch32(), self.next);
                return false;
            }
            M::Call => return self.lift_call(i),
            M::Ret => {
                let t = self.pop_val(4);
                if i.op_count() == 1 {
                    let n = i.immediate16() as u32;
                    let s = self.bini(BinOp::I32Add, ESP, n);
                    self.set_gpr(ESP, s);
                }
                self.terminate(Term::Ret(t));
                return false;
            }
            M::Int3 => {
                // The exception address of a breakpoint is the int3 itself.
                self.terminate(Term::Fault {
                    code: fault::BREAKPOINT,
                    eip: self.eip,
                });
                return false;
            }
            M::Int | M::Into | M::Int1 => {
                let n = if m == M::Int { i.immediate8() as u32 } else { 4 };
                let v = self.c32(n);
                self.store_native(v, 4, crate::abi::cpu::FAULT_ADDR);
                self.terminate(Term::Fault {
                    code: fault::SOFTWARE_INTERRUPT,
                    eip: self.eip,
                });
                return false;
            }
            M::Ud2 | M::Ud0 | M::Ud1 => {
                self.terminate(Term::Fault {
                    code: fault::ILLEGAL_INSTRUCTION,
                    eip: self.eip,
                });
                return false;
            }
            M::Hlt => {
                self.terminate(Term::Fault {
                    code: fault::PRIVILEGED_INSTRUCTION,
                    eip: self.eip,
                });
                return false;
            }
            _ => {
                if simd::is_simd(i) {
                    return self.lift_simd(i);
                }
                self.unsupported(i, "no lifting rule");
                return false;
            }
        }
        true
    }

    fn ea_no_seg(&mut self, i: &Instruction) -> (V, Space) {
        // lea ignores segment bases.
        let base = i.memory_base();
        let index = i.memory_index();
        let disp = i.memory_displacement32();
        let mut addr: Option<V> = None;
        if base != Register::None {
            addr = Some(self.copy(gpr_index(base)));
        }
        if index != Register::None {
            let mut ix = self.copy(gpr_index(index));
            let scale = i.memory_index_scale();
            if scale > 1 {
                ix = self.bini(BinOp::I32Shl, ix, scale.trailing_zeros());
            }
            addr = Some(match addr {
                Some(a) => self.bin(BinOp::I32Add, a, ix),
                None => ix,
            });
        }
        let a = match addr {
            Some(a) if disp != 0 => self.bini(BinOp::I32Add, a, disp),
            Some(a) => a,
            None => self.c32(disp),
        };
        (a, Space::Guest)
    }

    fn lift_mov(&mut self, i: &Instruction) {
        let k0 = i.op0_kind();
        let k1 = i.op1_kind();
        if k0 == OpKind::Register && i.op0_register().is_segment_register() {
            // Loading a segment register: only flat selectors are supported;
            // record the selector so it reads back.
            let v = self.read_op(i, 1);
            let idx = i.op0_register() as u32 - Register::ES as u32;
            self.store_native(v, 2, crate::abi::cpu::SEG_SEL + idx * 2);
            return;
        }
        if k1 == OpKind::Register && i.op1_register().is_segment_register() {
            let idx = i.op1_register() as u32 - Register::ES as u32;
            let v = self.load_native(Ty::I32, 2, crate::abi::cpu::SEG_SEL + idx * 2);
            let w = self.op_width(i, 0);
            let d = self.loc(i, 0);
            self.write(d, v, w.min(16).max(if k0 == OpKind::Register { w } else { 16 }));
            return;
        }
        let w = self.op_width(i, 0);
        let s = self.loc(i, 1);
        let v = self.read(s, w);
        let d = self.loc(i, 0);
        self.write(d, v, w);
        if let (Loc::Reg(r), Loc::Mem { addr, .. }) = (d, s) {
            if w == 32 {
                self.loaded_from[gpr_index(r) as usize] = Some(addr);
            }
        }
    }

    fn lift_xchg(&mut self, i: &Instruction) {
        let w = self.op_width(i, 0);
        let a = self.loc(i, 0);
        let b = self.loc(i, 1);
        match (a, b) {
            (Loc::Mem { addr, mem }, Loc::Reg(r)) | (Loc::Reg(r), Loc::Mem { addr, mem }) => {
                // xchg with memory is implicitly locked.
                let v = self.read_reg(r);
                let mut m = mem;
                m.size = (w / 8) as u8;
                m.atomic = true;
                let old = self.emit(
                    Ty::I32,
                    Op::AtomicRmw {
                        op: RmwOp::Xchg,
                        addr,
                        val: v,
                        mem: m,
                    },
                );
                self.write_reg(r, old);
            }
            _ => {
                let va = self.read(a, w);
                let vb = self.read(b, w);
                self.write(a, vb, w);
                self.write(b, va, w);
            }
        }
    }

    fn push_val(&mut self, v: V, size: u32) {
        let sp = self.bini(BinOp::I32Sub, ESP, size);
        self.store(sp, v, size, Space::Trusted);
        self.set_gpr(ESP, sp);
    }

    fn pop_val(&mut self, size: u32) -> V {
        let v = self.load(ESP, size, Space::Trusted);
        let sp = self.bini(BinOp::I32Add, ESP, size);
        self.set_gpr(ESP, sp);
        v
    }

    fn lift_push(&mut self, i: &Instruction) {
        let size = (i.stack_pointer_increment().unsigned_abs()) as u32;
        let v = match i.op0_kind() {
            OpKind::Register if i.op0_register().is_segment_register() => {
                let idx = i.op0_register() as u32 - Register::ES as u32;
                self.load_native(Ty::I32, 2, crate::abi::cpu::SEG_SEL + idx * 2)
            }
            OpKind::Register => self.read_reg(i.op0_register()),
            OpKind::Memory => {
                let (a, sp) = self.ea(i);
                self.load(a, size, sp)
            }
            _ => self.c32(i.immediate(0) as u32 & width_mask(size * 8)),
        };
        self.push_val(v, size);
    }

    fn lift_pop(&mut self, i: &Instruction) {
        let size = i.stack_pointer_increment() as u32;
        match i.op0_kind() {
            OpKind::Register if i.op0_register().is_segment_register() => {
                let v = self.pop_val(size);
                let idx = i.op0_register() as u32 - Register::ES as u32;
                self.store_native(v, 2, crate::abi::cpu::SEG_SEL + idx * 2);
            }
            OpKind::Register => {
                let v = self.pop_val(size);
                self.write_reg(i.op0_register(), v);
            }
            _ => {
                // The address is computed after esp is incremented.
                let v = self.load(ESP, size, Space::Trusted);
                let sp = self.bini(BinOp::I32Add, ESP, size);
                self.set_gpr(ESP, sp);
                let (a, s) = self.ea(i);
                self.store(a, v, size, s);
            }
        }
    }

    fn lift_alu(&mut self, i: &Instruction) {
        use Mnemonic as M;
        let m = i.mnemonic();
        let w = self.op_width(i, 0);
        let d = self.loc(i, 0);
        let b = self.read_op_w(i, 1, w);
        let locked = i.has_lock_prefix() && matches!(d, Loc::Mem { .. });
        // `xor r, r` and `sub r, r` are zeroing idioms.
        if matches!(m, M::Xor | M::Sub)
            && i.op0_kind() == OpKind::Register
            && i.op1_kind() == OpKind::Register
            && i.op0_register() == i.op1_register()
        {
            let z = self.c32(0);
            self.write(d, z, w);
            if m == M::Xor {
                self.set_flags(fl::LOGIC, w, z, z, None, None);
            } else {
                self.set_flags(fl::SUB, w, z, z, Some(z), None);
            }
            return;
        }
        if locked {
            if let Loc::Mem { addr, mem } = d {
                let rmw = match m {
                    M::Add => Some(RmwOp::Add),
                    M::Sub => Some(RmwOp::Sub),
                    M::And => Some(RmwOp::And),
                    M::Or => Some(RmwOp::Or),
                    M::Xor => Some(RmwOp::Xor),
                    _ => None,
                };
                if let Some(op) = rmw {
                    let mut mm = mem;
                    mm.size = (w / 8) as u8;
                    mm.atomic = true;
                    let a = self.emit(
                        Ty::I32,
                        Op::AtomicRmw {
                            op,
                            addr,
                            val: b,
                            mem: mm,
                        },
                    );
                    let (bop, kind) = match m {
                        M::Add => (BinOp::I32Add, fl::ADD),
                        M::Sub => (BinOp::I32Sub, fl::SUB),
                        M::And => (BinOp::I32And, fl::LOGIC),
                        M::Or => (BinOp::I32Or, fl::LOGIC),
                        _ => (BinOp::I32Xor, fl::LOGIC),
                    };
                    let r = self.bin(bop, a, b);
                    let r = self.mask(r, w);
                    let bb = if kind == fl::LOGIC { None } else { Some(b) };
                    self.set_flags(kind, w, r, a, bb, None);
                    return;
                }
            }
        }
        let a = self.read(d, w);
        let (r, kind, carry) = match m {
            M::Add => (self.bin(BinOp::I32Add, a, b), fl::ADD, None),
            M::Sub | M::Cmp => (self.bin(BinOp::I32Sub, a, b), fl::SUB, None),
            M::Adc => {
                let c = self.cond(Cc::B);
                let s = self.bin(BinOp::I32Add, a, b);
                (self.bin(BinOp::I32Add, s, c), fl::ADC, Some(c))
            }
            M::Sbb => {
                let c = self.cond(Cc::B);
                let s = self.bin(BinOp::I32Sub, a, b);
                (self.bin(BinOp::I32Sub, s, c), fl::SBB, Some(c))
            }
            M::And | M::Test => (self.bin(BinOp::I32And, a, b), fl::LOGIC, None),
            M::Or => (self.bin(BinOp::I32Or, a, b), fl::LOGIC, None),
            _ => (self.bin(BinOp::I32Xor, a, b), fl::LOGIC, None),
        };
        let r = self.mask(r, w);
        if !matches!(m, M::Cmp | M::Test) {
            self.write(d, r, w);
        }
        let bb = if kind == fl::LOGIC { None } else { Some(b) };
        self.set_flags(kind, w, r, a, bb, carry);
    }

    /// Reads operand `n` at width `w` (immediates are sign-extended to the
    /// destination width by the decoder already).
    fn read_op_w(&mut self, i: &Instruction, n: u32, w: u32) -> V {
        let l = self.loc(i, n);
        self.read(l, w)
    }

    fn lift_incdec(&mut self, i: &Instruction) {
        let inc = i.mnemonic() == Mnemonic::Inc;
        let w = self.op_width(i, 0);
        let d = self.loc(i, 0);
        let cf = self.cond(Cc::B);
        let a = if let (true, Loc::Mem { addr, mem }) = (i.has_lock_prefix(), d) {
            let one = self.c32(1);
            let mut mm = mem;
            mm.size = (w / 8) as u8;
            mm.atomic = true;
            self.emit(
                Ty::I32,
                Op::AtomicRmw {
                    op: if inc { RmwOp::Add } else { RmwOp::Sub },
                    addr,
                    val: one,
                    mem: mm,
                },
            )
        } else {
            self.read(d, w)
        };
        let r = if inc {
            self.bini(BinOp::I32Add, a, 1)
        } else {
            self.bini(BinOp::I32Sub, a, 1)
        };
        let r = self.mask(r, w);
        if !i.has_lock_prefix() || !matches!(d, Loc::Mem { .. }) {
            self.write(d, r, w);
        }
        self.set_flags(
            if inc { fl::INC } else { fl::DEC },
            w,
            r,
            a,
            None,
            Some(cf),
        );
    }

    /// Shift count operand masked to 5 bits.
    fn shift_count(&mut self, i: &Instruction) -> (V, Option<u32>) {
        match i.op_kind(1) {
            OpKind::Immediate8 => {
                let c = i.immediate8() as u32 & 31;
                (self.c32(c), Some(c))
            }
            OpKind::Register => {
                let c = self.read_reg(i.op1_register());
                (self.bini(BinOp::I32And, c, 31), None)
            }
            _ => {
                // D0/D1 forms shift by one; iced reports no second operand.
                (self.c32(1), Some(1))
            }
        }
    }

    fn lift_shift(&mut self, i: &Instruction) {
        use Mnemonic as M;
        let m = i.mnemonic();
        let w = self.op_width(i, 0);
        let d = self.loc(i, 0);
        let (cnt, known) = if i.op_count() == 1 {
            (self.c32(1), Some(1))
        } else {
            self.shift_count(i)
        };
        if known == Some(0) {
            return;
        }
        let a = self.read(d, w);
        let (r, kind) = match m {
            M::Shl | M::Sal => (self.bin(BinOp::I32Shl, a, cnt), fl::SHL),
            M::Shr => (self.bin(BinOp::I32ShrU, a, cnt), fl::SHR),
            _ => {
                let s = self.sext(a, w);
                (self.bin(BinOp::I32ShrS, s, cnt), fl::SAR)
            }
        };
        let r = self.mask(r, w);
        self.write(d, r, w);
        match known {
            Some(_) => self.set_flags(kind, w, r, a, Some(cnt), None),
            None => {
                let nz = self.bini(BinOp::I32Ne, cnt, 0);
                self.set_flags_if(nz, kind, w, r, a, Some(cnt), None);
            }
        }
    }

    fn lift_rotate(&mut self, i: &Instruction) {
        let rol = i.mnemonic() == Mnemonic::Rol;
        let w = self.op_width(i, 0);
        let d = self.loc(i, 0);
        let (cnt, known) = if i.op_count() == 1 {
            (self.c32(1), Some(1))
        } else {
            self.shift_count(i)
        };
        if known == Some(0) {
            return;
        }
        let a = self.read(d, w);
        let r = if w == 32 {
            self.bin(if rol { BinOp::I32Rotl } else { BinOp::I32Rotr }, a, cnt)
        } else {
            // Rotate within w bits: n = cnt mod w.
            let n = self.bini(BinOp::I32And, cnt, w - 1);
            let back = self.c32(w);
            let back = self.bin(BinOp::I32Sub, back, n);
            let (x, y) = if rol {
                (
                    self.bin(BinOp::I32Shl, a, n),
                    self.bin(BinOp::I32ShrU, a, back),
                )
            } else {
                (
                    self.bin(BinOp::I32ShrU, a, n),
                    self.bin(BinOp::I32Shl, a, back),
                )
            };
            let o = self.bin(BinOp::I32Or, x, y);
            self.mask(o, w)
        };
        self.write(d, r, w);
        // CF and OF change; other flags are preserved.
        let (cf, of) = if rol {
            let cf = self.bini(BinOp::I32And, r, 1);
            let msb = self.sign_bit(r, w);
            (cf, self.bin(BinOp::I32Xor, msb, cf))
        } else {
            let cf = self.sign_bit(r, w);
            let s2 = self.bini(BinOp::I32ShrU, r, w - 2);
            let s2 = self.bini(BinOp::I32And, s2, 1);
            (cf, self.bin(BinOp::I32Xor, cf, s2))
        };
        self.update_cf_of(cf, of, cnt, known);
    }

    /// Replaces CF and OF in the flag state, keeping the other flags. When
    /// the count is not known to be non-zero, the update is conditional.
    fn update_cf_of(&mut self, cf: V, of: V, cnt: V, known: Option<u32>) {
        let e = self.eflags();
        let k = self.bini(BinOp::I32And, e, !(fl::CF | fl::OF));
        let o = self.bini(BinOp::I32Shl, of, 11);
        let x = self.bin(BinOp::I32Or, k, cf);
        let n = self.bin(BinOp::I32Or, x, o);
        match known {
            Some(_) => self.set_flags_explicit(n),
            None => {
                let nz = self.bini(BinOp::I32Ne, cnt, 0);
                let k = self.c32(fl::EXPLICIT);
                self.emit_to(FK, Op::Select { cond: nz, t: k, f: FK });
                self.emit_to(FR, Op::Select { cond: nz, t: n, f: FR });
            }
        }
    }

    fn lift_rotate_carry(&mut self, i: &Instruction) {
        let rcl = i.mnemonic() == Mnemonic::Rcl;
        let w = self.op_width(i, 0);
        let d = self.loc(i, 0);
        let (cnt0, known) = if i.op_count() == 1 {
            (self.c32(1), Some(1))
        } else {
            self.shift_count(i)
        };
        if known == Some(0) {
            return;
        }
        // Effective count modulo w+1 for 8/16-bit operands.
        let cnt = if w < 32 {
            self.bini(BinOp::I32RemU, cnt0, w + 1)
        } else {
            cnt0
        };
        let a = self.read(d, w);
        let cf_in = self.cond(Cc::B);
        // Build the (w+1)-bit value CF:a in an i64 and rotate it.
        let a64 = self.un(UnOp::I64ExtendI32U, a);
        let c64 = self.un(UnOp::I64ExtendI32U, cf_in);
        let wk = self.c64(w as u64);
        let c_hi = self.bin(BinOp::I64Shl, c64, wk);
        let x = self.bin(BinOp::I64Or, c_hi, a64);
        let n64 = self.un(UnOp::I64ExtendI32U, cnt);
        let total = self.c64(w as u64 + 1);
        let back = self.bin(BinOp::I64Sub, total, n64);
        let (p, q) = if rcl {
            (
                self.bin(BinOp::I64Shl, x, n64),
                self.bin(BinOp::I64ShrU, x, back),
            )
        } else {
            (
                self.bin(BinOp::I64ShrU, x, n64),
                self.bin(BinOp::I64Shl, x, back),
            )
        };
        // When n == 0, `back` is w+1 and the shifted-in part must vanish.
        let z = self.c64(0);
        let is0 = self.bini(BinOp::I32Eq, cnt, 0);
        let q0 = self.select(is0, z, q);
        let y = self.bin(BinOp::I64Or, p, q0);
        let r = self.un(UnOp::I32WrapI64, y);
        let r = self.mask(r, w);
        let wk2 = self.c64(w as u64);
        let cf = self.bin(BinOp::I64ShrU, y, wk2);
        let cf = self.un(UnOp::I32WrapI64, cf);
        let cf = self.bini(BinOp::I32And, cf, 1);
        // With an effective count of 0 the carry is unchanged.
        let is0 = self.bini(BinOp::I32Eq, cnt, 0);
        let cf = self.select(is0, cf_in, cf);
        self.write(d, r, w);
        let of = if rcl {
            let msb = self.sign_bit(r, w);
            self.bin(BinOp::I32Xor, msb, cf)
        } else {
            let msb = self.sign_bit(r, w);
            let s2 = self.bini(BinOp::I32ShrU, r, w - 2);
            let s2 = self.bini(BinOp::I32And, s2, 1);
            self.bin(BinOp::I32Xor, msb, s2)
        };
        self.update_cf_of(cf, of, cnt0, known);
    }

    fn lift_shift_double(&mut self, i: &Instruction) {
        let left = i.mnemonic() == Mnemonic::Shld;
        let w = self.op_width(i, 0);
        let d = self.loc(i, 0);
        let src = self.read_op(i, 1);
        let (cnt, known) = match i.op_kind(2) {
            OpKind::Immediate8 => {
                let c = i.immediate8() as u32 & 31;
                (self.c32(c), Some(c))
            }
            _ => {
                let c = self.read_reg(i.op2_register());
                (self.bini(BinOp::I32And, c, 31), None)
            }
        };
        if known == Some(0) {
            return;
        }
        let a = self.read(d, w);
        // Concatenate into an i64: shld shifts a:src left, shrd shifts src:a
        // right.
        let a64 = self.un(UnOp::I64ExtendI32U, a);
        let s64 = self.un(UnOp::I64ExtendI32U, src);
        let wk = self.c64(w as u64);
        let n64 = self.un(UnOp::I64ExtendI32U, cnt);
        let (r, cf) = if left {
            let hi = self.bin(BinOp::I64Shl, a64, wk);
            let x = self.bin(BinOp::I64Or, hi, s64);
            let y = self.bin(BinOp::I64Shl, x, n64);
            let r = self.bin(BinOp::I64ShrU, y, wk);
            let r = self.un(UnOp::I32WrapI64, r);
            // CF = bit (w - cnt) of a = bit 2w-1 of (x << (cnt-1)).
            let one = self.c64(1);
            let nm1 = self.bin(BinOp::I64Sub, n64, one);
            let z = self.bin(BinOp::I64Shl, x, nm1);
            let top = self.c64(2 * w as u64 - 1);
            let c = self.bin(BinOp::I64ShrU, z, top);
            let c = self.un(UnOp::I32WrapI64, c);
            (r, self.bini(BinOp::I32And, c, 1))
        } else {
            let hi = self.bin(BinOp::I64Shl, s64, wk);
            let x = self.bin(BinOp::I64Or, hi, a64);
            let y = self.bin(BinOp::I64ShrU, x, n64);
            let r = self.un(UnOp::I32WrapI64, y);
            let one = self.c64(1);
            let nm1 = self.bin(BinOp::I64Sub, n64, one);
            let z = self.bin(BinOp::I64ShrU, x, nm1);
            let c = self.un(UnOp::I32WrapI64, z);
            (r, self.bini(BinOp::I32And, c, 1))
        };
        let r = self.mask(r, w);
        self.write(d, r, w);
        // OF: sign change (defined for count 1).
        let sa = self.sign_bit(a, w);
        let sr = self.sign_bit(r, w);
        let of = self.bin(BinOp::I32Xor, sa, sr);
        // SF, ZF, PF from the result.
        let zf = self.is_zero(r);
        let lo = self.bini(BinOp::I32And, r, 0xff);
        let pc = self.un(UnOp::I32Popcnt, lo);
        let pc = self.bini(BinOp::I32And, pc, 1);
        let pf = self.is_zero(pc);
        let e1 = self.bini(BinOp::I32Shl, pf, 2);
        let e2 = self.bini(BinOp::I32Shl, zf, 6);
        let e3 = self.bini(BinOp::I32Shl, sr, 7);
        let e4 = self.bini(BinOp::I32Shl, of, 11);
        let t1 = self.bin(BinOp::I32Or, cf, e1);
        let t2 = self.bin(BinOp::I32Or, e2, e3);
        let t3 = self.bin(BinOp::I32Or, t1, t2);
        let e = self.bin(BinOp::I32Or, t3, e4);
        match known {
            Some(_) => self.set_flags_explicit(e),
            None => {
                let nz = self.bini(BinOp::I32Ne, cnt, 0);
                let k = self.c32(fl::EXPLICIT);
                self.emit_to(FK, Op::Select { cond: nz, t: k, f: FK });
                self.emit_to(FR, Op::Select { cond: nz, t: e, f: FR });
            }
        }
    }

    fn lift_mul(&mut self, i: &Instruction) {
        let signed = i.mnemonic() == Mnemonic::Imul;
        if i.op_count() >= 2 {
            // imul r, r/m [, imm]: truncated product.
            let w = self.op_width(i, 0);
            let (a, b) = if i.op_count() == 3 {
                (self.read_op(i, 1), self.read_op_w(i, 2, w))
            } else {
                (self.read_op(i, 0), self.read_op(i, 1))
            };
            let sa = self.sext(a, w);
            let sb = self.sext(b, w);
            let a64 = self.un(UnOp::I64ExtendI32S, sa);
            let b64 = self.un(UnOp::I64ExtendI32S, sb);
            let p = self.bin(BinOp::I64Mul, a64, b64);
            let lo = self.un(UnOp::I32WrapI64, p);
            let r = self.mask(lo, w);
            let rs = self.sext(r, w);
            let rs64 = self.un(UnOp::I64ExtendI32S, rs);
            let ovf = self.bin(BinOp::I64Ne, rs64, p);
            self.write_reg(i.op0_register(), r);
            self.set_flags(fl::MUL, w, r, a, Some(ovf), None);
            return;
        }
        let w = self.op_width(i, 0);
        let b = self.read_op(i, 0);
        match w {
            8 => {
                let a = self.read_reg(Register::AL);
                let (x, y) = if signed {
                    (self.sext(a, 8), self.sext(b, 8))
                } else {
                    (a, b)
                };
                let p = self.bin(BinOp::I32Mul, x, y);
                let p = self.mask(p, 16);
                self.write_reg(Register::AX, p);
                let lo = self.mask(p, 8);
                let ovf = if signed {
                    let s = self.sext(lo, 8);
                    let s = self.mask(s, 16);
                    self.bin(BinOp::I32Ne, s, p)
                } else {
                    let hi = self.bini(BinOp::I32ShrU, p, 8);
                    self.bini(BinOp::I32Ne, hi, 0)
                };
                self.set_flags(fl::MUL, 8, lo, a, Some(ovf), None);
            }
            16 => {
                let a = self.read_reg(Register::AX);
                let (x, y) = if signed {
                    (self.sext(a, 16), self.sext(b, 16))
                } else {
                    (a, b)
                };
                let p = self.bin(BinOp::I32Mul, x, y);
                let lo = self.mask(p, 16);
                let hi = self.bini(BinOp::I32ShrU, p, 16);
                self.write_reg(Register::AX, lo);
                self.write_reg(Register::DX, hi);
                let ovf = if signed {
                    let s = self.sext(lo, 16);
                    self.bin(BinOp::I32Ne, s, p)
                } else {
                    let h = self.mask(hi, 16);
                    self.bini(BinOp::I32Ne, h, 0)
                };
                self.set_flags(fl::MUL, 16, lo, a, Some(ovf), None);
            }
            _ => {
                let a = self.copy(EAX);
                let ext = if signed {
                    UnOp::I64ExtendI32S
                } else {
                    UnOp::I64ExtendI32U
                };
                let a64 = self.un(ext, a);
                let b64 = self.un(ext, b);
                let p = self.bin(BinOp::I64Mul, a64, b64);
                let lo = self.un(UnOp::I32WrapI64, p);
                let k = self.c64(32);
                let hi = self.bin(BinOp::I64ShrU, p, k);
                let hi = self.un(UnOp::I32WrapI64, hi);
                self.set_gpr(EAX, lo);
                self.set_gpr(EDX, hi);
                let ovf = if signed {
                    let s = self.un(UnOp::I64ExtendI32S, lo);
                    self.bin(BinOp::I64Ne, s, p)
                } else {
                    self.bini(BinOp::I32Ne, hi, 0)
                };
                self.set_flags(fl::MUL, 32, lo, a, Some(ovf), None);
            }
        }
    }

    fn lift_div(&mut self, i: &Instruction) {
        let signed = i.mnemonic() == Mnemonic::Idiv;
        let w = self.op_width(i, 0);
        let d = self.read_op(i, 0);
        let zero = self.is_zero(d);
        self.fault_if(zero, fault::INTEGER_DIVIDE_BY_ZERO, d);
        // Dividend as i64.
        let n = match w {
            8 => {
                let ax = self.read_reg(Register::AX);
                let v = if signed { self.sext(ax, 16) } else { ax };
                self.un(UnOp::I64ExtendI32S, v)
            }
            16 => {
                let dx = self.read_reg(Register::DX);
                let ax = self.read_reg(Register::AX);
                let hi = self.bini(BinOp::I32Shl, dx, 16);
                let v = self.bin(BinOp::I32Or, hi, ax);
                if signed {
                    self.un(UnOp::I64ExtendI32S, v)
                } else {
                    self.un(UnOp::I64ExtendI32U, v)
                }
            }
            _ => {
                let hi = self.un(UnOp::I64ExtendI32U, EDX);
                let k = self.c64(32);
                let hi = self.bin(BinOp::I64Shl, hi, k);
                let lo = self.un(UnOp::I64ExtendI32U, EAX);
                self.bin(BinOp::I64Or, hi, lo)
            }
        };
        let (q, r) = if signed {
            let ds = self.sext(d, w);
            let d64 = self.un(UnOp::I64ExtendI32S, ds);
            // Avoid the WebAssembly trap on i64::MIN / -1.
            let m1 = self.c64(u64::MAX);
            let is_m1 = self.bin(BinOp::I64Eq, d64, m1);
            let one = self.c64(1);
            let safe = self.select(is_m1, one, d64);
            let q = self.bin(BinOp::I64DivS, n, safe);
            let z = self.c64(0);
            let negn = self.bin(BinOp::I64Sub, z, n);
            let q = self.select(is_m1, negn, q);
            let r = self.bin(BinOp::I64RemS, n, safe);
            let r = self.select(is_m1, z, r);
            // Overflow when the quotient does not fit in w bits (signed).
            let qlo = self.un(UnOp::I32WrapI64, q);
            let qs = self.sext(qlo, w);
            let qs = if w == 32 { qlo } else { qs };
            let back = self.un(UnOp::I64ExtendI32S, qs);
            let bad = self.bin(BinOp::I64Ne, back, q);
            // i64::MIN / -1 also overflows (negation wraps to itself).
            self.fault_if(bad, fault::INTEGER_OVERFLOW, d);
            (qlo, self.un(UnOp::I32WrapI64, r))
        } else {
            let d64 = self.un(UnOp::I64ExtendI32U, d);
            let q = self.bin(BinOp::I64DivU, n, d64);
            let r = self.bin(BinOp::I64RemU, n, d64);
            let lim = self.c64(width_mask(w) as u64);
            let bad = self.bin(BinOp::I64GtU, q, lim);
            self.fault_if(bad, fault::INTEGER_OVERFLOW, d);
            (
                self.un(UnOp::I32WrapI64, q),
                self.un(UnOp::I32WrapI64, r),
            )
        };
        match w {
            8 => {
                let q = self.mask(q, 8);
                let r = self.mask(r, 8);
                let r = self.bini(BinOp::I32Shl, r, 8);
                let ax = self.bin(BinOp::I32Or, q, r);
                self.write_reg(Register::AX, ax);
            }
            16 => {
                self.write_reg(Register::AX, q);
                self.write_reg(Register::DX, r);
            }
            _ => {
                self.set_gpr(EAX, q);
                self.set_gpr(EDX, r);
            }
        }
        // All arithmetic flags are undefined after division; leave them.
    }

    fn lift_bt(&mut self, i: &Instruction) {
        use Mnemonic as M;
        let m = i.mnemonic();
        let w = self.op_width(i, 0);
        let (loc, bitpos) = match (i.op0_kind(), i.op1_kind()) {
            (OpKind::Memory, OpKind::Register) => {
                // The bit offset is signed and can address outside the
                // operand.
                let off = self.read_reg(i.op1_register());
                let off = self.sext(off, w);
                let (a, sp) = self.ea(i);
                let shift = if w == 32 { 5 } else { 4 };
                let words = self.bini(BinOp::I32ShrS, off, shift);
                let bytes = self.bini(BinOp::I32Shl, words, (w / 8).trailing_zeros());
                let addr = self.bin(BinOp::I32Add, a, bytes);
                let pos = self.bini(BinOp::I32And, off, w - 1);
                (
                    Loc::Mem {
                        addr,
                        mem: self.mem(w / 8, sp),
                    },
                    pos,
                )
            }
            _ => {
                let l = self.loc(i, 0);
                let off = self.read_op(i, 1);
                let pos = self.bini(BinOp::I32And, off, w - 1);
                (l, pos)
            }
        };
        let v = self.read(loc, w);
        let s = self.bin(BinOp::I32ShrU, v, bitpos);
        let cf = self.bini(BinOp::I32And, s, 1);
        if m != M::Bt {
            let one = self.c32(1);
            let bitm = self.bin(BinOp::I32Shl, one, bitpos);
            let nv = match m {
                M::Bts => self.bin(BinOp::I32Or, v, bitm),
                M::Btr => {
                    let inv = self.bini(BinOp::I32Xor, bitm, u32::MAX);
                    self.bin(BinOp::I32And, v, inv)
                }
                _ => self.bin(BinOp::I32Xor, v, bitm),
            };
            self.write(loc, nv, w);
        }
        // CF = bit; ZF preserved; OF SF AF PF undefined.
        let zf = self.cond(Cc::E);
        let z = self.bini(BinOp::I32Shl, zf, 6);
        let e = self.bin(BinOp::I32Or, z, cf);
        self.set_flags_explicit(e);
    }

    fn lift_bitscan(&mut self, i: &Instruction) {
        use Mnemonic as M;
        let m = i.mnemonic();
        let w = self.op_width(i, 0);
        let src = self.read_op(i, 1);
        let r = i.op0_register();
        match m {
            M::Bsf | M::Bsr => {
                let idx = if m == M::Bsf {
                    self.un(UnOp::I32Ctz, src)
                } else {
                    let c = self.un(UnOp::I32Clz, src);
                    let k = self.c32(31);
                    self.bin(BinOp::I32Sub, k, c)
                };
                // Destination unchanged when the source is zero.
                let z = self.is_zero(src);
                let old = self.read_reg(r);
                let v = self.select(z, old, idx);
                self.write_reg(r, v);
                self.set_flags(fl::LOGIC, w, src, src, None, None);
            }
            M::Popcnt => {
                let v = self.un(UnOp::I32Popcnt, src);
                self.write_reg(r, v);
                // ZF = src == 0; CF OF SF AF PF cleared.
                let z = self.is_zero(src);
                let e = self.bini(BinOp::I32Shl, z, 6);
                self.set_flags_explicit(e);
            }
            _ => {
                // tzcnt/lzcnt: count with width w; CF = src == 0; ZF = res == 0.
                let v = if m == M::Tzcnt {
                    let t = self.un(UnOp::I32Ctz, src);
                    let k = self.c32(w);
                    let z = self.is_zero(src);
                    self.select(z, k, t)
                } else {
                    let c = self.un(UnOp::I32Clz, src);
                    self.bini(BinOp::I32Sub, c, 32 - w)
                };
                self.write_reg(r, v);
                let cf = self.is_zero(src);
                let zf = self.is_zero(v);
                let z = self.bini(BinOp::I32Shl, zf, 6);
                let e = self.bin(BinOp::I32Or, z, cf);
                self.set_flags_explicit(e);
            }
        }
    }

    fn lift_xadd(&mut self, i: &Instruction) {
        let w = self.op_width(i, 0);
        let d = self.loc(i, 0);
        let sreg = i.op1_register();
        let b = self.read_reg(sreg);
        let a = match d {
            Loc::Mem { addr, mem } if i.has_lock_prefix() => {
                let mut mm = mem;
                mm.size = (w / 8) as u8;
                mm.atomic = true;
                self.emit(
                    Ty::I32,
                    Op::AtomicRmw {
                        op: RmwOp::Add,
                        addr,
                        val: b,
                        mem: mm,
                    },
                )
            }
            _ => self.read(d, w),
        };
        let r = self.bin(BinOp::I32Add, a, b);
        let r = self.mask(r, w);
        self.write_reg(sreg, a);
        if !(i.has_lock_prefix() && matches!(d, Loc::Mem { .. })) {
            self.write(d, r, w);
        }
        self.set_flags(fl::ADD, w, r, a, Some(b), None);
    }

    fn lift_cmpxchg(&mut self, i: &Instruction) {
        let w = self.op_width(i, 0);
        let d = self.loc(i, 0);
        let acc_reg = match w {
            8 => Register::AL,
            16 => Register::AX,
            _ => Register::EAX,
        };
        let acc = self.read_reg(acc_reg);
        let src = self.read_reg(i.op1_register());
        let old = match d {
            Loc::Mem { addr, mem } if i.has_lock_prefix() => {
                let mut mm = mem;
                mm.size = (w / 8) as u8;
                mm.atomic = true;
                self.emit(
                    Ty::I32,
                    Op::AtomicCmpxchg {
                        addr,
                        expected: acc,
                        new: src,
                        mem: mm,
                    },
                )
            }
            _ => {
                let old = self.read(d, w);
                let eq = self.bin(BinOp::I32Eq, old, acc);
                let nv = self.select(eq, src, old);
                self.write(d, nv, w);
                old
            }
        };
        let r = self.bin(BinOp::I32Sub, acc, old);
        let r = self.mask(r, w);
        self.set_flags(fl::SUB, w, r, acc, Some(old), None);
        // The accumulator receives the old value (equal on success).
        self.write_reg(acc_reg, old);
    }

    fn lift_cmpxchg8b(&mut self, i: &Instruction) {
        let (addr, space) = self.ea(i);
        let mk64 = |l: &mut Self, hi: V, lo: V| {
            let h = l.un(UnOp::I64ExtendI32U, hi);
            let k = l.c64(32);
            let h = l.bin(BinOp::I64Shl, h, k);
            let lo = l.un(UnOp::I64ExtendI32U, lo);
            l.bin(BinOp::I64Or, h, lo)
        };
        let expected = mk64(self, EDX, EAX);
        let new = mk64(self, ECX, EBX);
        let mut mm = self.mem(8, space);
        let old = if i.has_lock_prefix() {
            mm.atomic = true;
            self.emit(
                Ty::I64,
                Op::AtomicCmpxchg {
                    addr,
                    expected,
                    new,
                    mem: mm,
                },
            )
        } else {
            let old = self.emit(Ty::I64, Op::Load { addr, mem: mm });
            let eq = self.bin(BinOp::I64Eq, old, expected);
            let nv = self.select(eq, new, old);
            self.effect(Op::Store {
                addr,
                val: nv,
                mem: mm,
            });
            old
        };
        let eq = self.bin(BinOp::I64Eq, old, expected);
        let lo = self.un(UnOp::I32WrapI64, old);
        let k = self.c64(32);
        let hi = self.bin(BinOp::I64ShrU, old, k);
        let hi = self.un(UnOp::I32WrapI64, hi);
        self.set_gpr(EAX, lo);
        self.set_gpr(EDX, hi);
        // ZF = success; other flags preserved.
        let e = self.eflags();
        let k = self.bini(BinOp::I32And, e, !fl::ZF);
        let z = self.bini(BinOp::I32Shl, eq, 6);
        let e = self.bin(BinOp::I32Or, k, z);
        self.set_flags_explicit(e);
    }

    fn lift_cpuid(&mut self) {
        // A Pentium 4-class CPU with SSE2.
        let leaf = self.copy(EAX);
        let is0 = self.bini(BinOp::I32Eq, leaf, 0);
        let is1 = self.bini(BinOp::I32Eq, leaf, 1);
        let pick = |l: &mut Self, v0: u32, v1: u32| {
            let a = l.c32(v0);
            let b = l.c32(v1);
            let z = l.c32(0);
            let t = l.select(is1, b, z);
            l.select(is0, a, t)
        };
        // "GenuineIntel"
        let eax = pick(self, 1, 0x0000_0f29);
        let ebx = pick(self, 0x756e_6547, 0x0001_0800);
        let ecx = pick(self, 0x6c65_746e, 0);
        let edx = pick(self, 0x4965_6e69, 0x0781_a3bf & !(1 << 9) & !(1 << 22));
        self.set_gpr(EAX, eax);
        self.set_gpr(EBX, ebx);
        self.set_gpr(ECX, ecx);
        self.set_gpr(EDX, edx);
    }

    // ---- String instructions ---------------------------------------------

    fn lift_string(&mut self, i: &Instruction) -> bool {
        use Mnemonic as M;
        let m = i.mnemonic();
        let size = i.memory_size().size() as u32;
        let w = size * 8;
        let rep = i.has_rep_prefix() || i.has_repe_prefix();
        let repne = i.has_repne_prefix();
        let is_cmp = matches!(m, M::Cmpsb | M::Cmpsw | M::Cmpsd | M::Scasb | M::Scasw | M::Scasd);
        // Source segment can be overridden (default DS); destination is ES.
        let src_seg = i.memory_segment();
        if !rep && !repne {
            self.string_iter(m, size, src_seg);
            return true;
        }
        // Fast paths: rep movs/stos forward without harmful overlap.
        let head = self.new_block();
        let body = self.new_block();
        let done = self.target_block(self.next);
        self.terminate(Term::Jump(head));
        self.switch_to(head);
        let z = self.is_zero(ECX);
        if !is_cmp && matches!(m, M::Movsb | M::Movsw | M::Movsd | M::Stosb | M::Stosw | M::Stosd)
            && !matches!(src_seg, Register::FS | Register::GS)
        {
            // if ecx == 0 -> done; if fast-path ok -> fast; else body
            let check = self.new_block();
            self.terminate(Term::Branch {
                cond: z,
                t: done,
                f: check,
            });
            self.switch_to(check);
            let len = self.bini(BinOp::I32Shl, ECX, size.trailing_zeros());
            let fwd = self.is_zero(DF);
            let ok = if matches!(m, M::Movsb | M::Movsw | M::Movsd) {
                let delta = self.bin(BinOp::I32Sub, EDI, ESI);
                let no_overlap = self.bin(BinOp::I32GeU, delta, len);
                self.bin(BinOp::I32And, fwd, no_overlap)
            } else if m == M::Stosb {
                fwd
            } else {
                // stosw/stosd can use fill only when all bytes are equal.
                let v = if size == 2 {
                    self.read_reg(Register::AX)
                } else {
                    self.copy(EAX)
                };
                let lo = self.bini(BinOp::I32And, v, 0xff);
                let rep_ = self.bini(BinOp::I32Mul, lo, if size == 2 { 0x0101 } else { 0x0101_0101 });
                let same = self.bin(BinOp::I32Eq, rep_, v);
                self.bin(BinOp::I32And, fwd, same)
            };
            let fast = self.new_block();
            self.terminate(Term::Branch {
                cond: ok,
                t: fast,
                f: body,
            });
            self.switch_to(fast);
            if matches!(m, M::Movsb | M::Movsw | M::Movsd) {
                self.effect(Op::MemCopy {
                    dst: EDI,
                    src: ESI,
                    len,
                });
                let s = self.bin(BinOp::I32Add, ESI, len);
                self.set_gpr(ESI, s);
            } else {
                let al = self.bini(BinOp::I32And, EAX, 0xff);
                self.effect(Op::MemFill {
                    dst: EDI,
                    val: al,
                    len,
                });
            }
            let d = self.bin(BinOp::I32Add, EDI, len);
            self.set_gpr(EDI, d);
            self.emit_to(ECX, Op::Const(0));
            self.terminate(Term::Jump(done));
        } else {
            self.terminate(Term::Branch {
                cond: z,
                t: done,
                f: body,
            });
        }
        self.switch_to(body);
        self.string_iter(m, size, src_seg);
        let n = self.bini(BinOp::I32Sub, ECX, 1);
        self.set_gpr(ECX, n);
        if is_cmp {
            // Continue while ecx != 0 and ZF matches the prefix.
            let nz = self.bini(BinOp::I32Ne, n, 0);
            let zc = self.cond(if repne { Cc::NE } else { Cc::E });
            let c = self.bin(BinOp::I32And, nz, zc);
            let back = self.new_block();
            self.terminate(Term::Branch {
                cond: c,
                t: back,
                f: done,
            });
            self.switch_to(back);
            self.terminate(Term::Jump(body));
        } else {
            self.terminate(Term::Jump(head));
        }
        let _ = w;
        false
    }

    /// One iteration of a string instruction, updating esi/edi by ±size.
    fn string_iter(&mut self, m: Mnemonic, size: u32, src_seg: Register) {
        use Mnemonic as M;
        let w = size * 8;
        // step = df ? -size : size
        let neg = self.c32(size.wrapping_neg());
        let pos = self.c32(size);
        let step = self.select(DF, neg, pos);
        let src_addr = |l: &mut Self| -> V {
            match src_seg {
                Register::FS => l.bin(BinOp::I32Add, ESI, FS_BASE),
                Register::GS => l.bin(BinOp::I32Add, ESI, GS_BASE),
                _ => l.copy(ESI),
            }
        };
        let acc = match size {
            1 => Register::AL,
            2 => Register::AX,
            _ => Register::EAX,
        };
        match m {
            M::Movsb | M::Movsw | M::Movsd => {
                let s = src_addr(self);
                let v = self.load(s, size, Space::Guest);
                let d = self.copy(EDI);
                self.store(d, v, size, Space::Guest);
                let ns = self.bin(BinOp::I32Add, ESI, step);
                self.set_gpr(ESI, ns);
                let nd = self.bin(BinOp::I32Add, EDI, step);
                self.set_gpr(EDI, nd);
            }
            M::Stosb | M::Stosw | M::Stosd => {
                let v = self.read_reg(acc);
                let d = self.copy(EDI);
                self.store(d, v, size, Space::Guest);
                let nd = self.bin(BinOp::I32Add, EDI, step);
                self.set_gpr(EDI, nd);
            }
            M::Lodsb | M::Lodsw | M::Lodsd => {
                let s = src_addr(self);
                let v = self.load(s, size, Space::Guest);
                self.write_reg(acc, v);
                let ns = self.bin(BinOp::I32Add, ESI, step);
                self.set_gpr(ESI, ns);
            }
            M::Cmpsb | M::Cmpsw | M::Cmpsd => {
                let s = src_addr(self);
                let a = self.load(s, size, Space::Guest);
                let d = self.copy(EDI);
                let b = self.load(d, size, Space::Guest);
                let r = self.bin(BinOp::I32Sub, a, b);
                let r = self.mask(r, w);
                self.set_flags(fl::SUB, w, r, a, Some(b), None);
                let ns = self.bin(BinOp::I32Add, ESI, step);
                self.set_gpr(ESI, ns);
                let nd = self.bin(BinOp::I32Add, EDI, step);
                self.set_gpr(EDI, nd);
            }
            _ => {
                // scas: compare accumulator with [edi].
                let a = self.read_reg(acc);
                let d = self.copy(EDI);
                let b = self.load(d, size, Space::Guest);
                let r = self.bin(BinOp::I32Sub, a, b);
                let r = self.mask(r, w);
                self.set_flags(fl::SUB, w, r, a, Some(b), None);
                let nd = self.bin(BinOp::I32Add, EDI, step);
                self.set_gpr(EDI, nd);
            }
        }
    }

    // ---- Branches and calls ----------------------------------------------

    fn branch(&mut self, cond: V, taken: u32, fall: u32) {
        let t = self.target_block(taken);
        let f = self.target_block(fall);
        self.terminate(Term::Branch { cond, t, f });
    }

    fn lift_jmp(&mut self, i: &Instruction) -> bool {
        match i.op0_kind() {
            OpKind::NearBranch32 => {
                let t = self.target_block(i.near_branch32());
                self.terminate(Term::Jump(t));
            }
            OpKind::Register | OpKind::Memory => {
                // Indirect jump: through a jump table when one was found.
                let (target, from_addr) = match i.op0_kind() {
                    OpKind::Memory => {
                        let (a, sp) = self.ea(i);
                        (self.load(a, 4, sp), Some(a))
                    }
                    _ => {
                        let r = i.op0_register();
                        let idx = gpr_index(r);
                        (self.read_reg(r), self.loaded_from[idx as usize])
                    }
                };
                match (self.disc.jump_tables.get(&self.eip), from_addr) {
                    (Some(jt), Some(addr)) => {
                        let jt = jt.clone();
                        // index = (addr - base) / 4, valid when aligned and
                        // in range; otherwise leave through the dispatcher.
                        let off = self.bini(BinOp::I32Sub, addr, jt.base);
                        let low = self.bini(BinOp::I32And, off, 3);
                        let ix = self.bini(BinOp::I32ShrU, off, 2);
                        // Misaligned offsets become out-of-range indexes.
                        let bad = self.c32(u32::MAX);
                        let index = self.select(low, bad, ix);
                        let targets: Vec<BlockId> =
                            jt.targets.iter().map(|&t| self.target_block(t)).collect();
                        self.terminate(Term::Switch {
                            index,
                            targets,
                            fallback: target,
                        });
                    }
                    _ => self.terminate(Term::JmpInd(target)),
                }
            }
            _ => self.unsupported(i, "far jump"),
        }
        false
    }

    fn lift_call(&mut self, i: &Instruction) -> bool {
        let ret = self.next;
        match i.op0_kind() {
            OpKind::NearBranch32 => {
                let t = i.near_branch32();
                let r = self.c32(ret);
                self.push_val(r, 4);
                if t == ret {
                    // get-PC idiom.
                    let b = self.target_block(ret);
                    self.terminate(Term::Jump(b));
                    return false;
                }
                let cont = self.target_block(ret);
                self.terminate(Term::Call {
                    target: CallTarget::Direct(t),
                    ret,
                    cont,
                });
            }
            OpKind::Register | OpKind::Memory => {
                // Read the target before pushing (call [esp] reads the old
                // stack top).
                let target = self.read_op(i, 0);
                let r = self.c32(ret);
                self.push_val(r, 4);
                let cont = self.target_block(ret);
                self.terminate(Term::Call {
                    target: CallTarget::Indirect(target),
                    ret,
                    cont,
                });
            }
            _ => self.unsupported(i, "far call"),
        }
        false
    }
}

