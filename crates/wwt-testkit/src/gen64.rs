//! Generates x86-64 instruction test cases: every legacy-encoded form valid
//! in 64-bit user mode, with the sixteen 64-bit registers, the REX-only
//! byte registers (spl..dil, r8b..r15b), xmm8-15, 64-bit immediates and
//! RIP-relative and 32-bit (0x67) addressing. Memory operands land in the
//! same data window as the 32-bit cases (mapped below 2 GB by the oracle)
//! and branches land on the oracle's stubs.

use iced_x86::{
    Code, CpuidFeature, Encoder, EncodingKind, Instruction, Mnemonic, OpCodeOperandKind as K,
    OpKind, Register,
};

use crate::case::{hex, Case};
use crate::gen::{integer_features_ok, simd_state, xmm_value, Group, Rng, SKIP_MNEMONICS};
use crate::layout::*;

/// Mnemonics skipped in 64-bit mode on top of the 32-bit list.
const SKIP_MNEMONICS64: &[Mnemonic] = &[
    Mnemonic::Iretq,
    Mnemonic::Swapgs,
    Mnemonic::Sysretq,
    Mnemonic::Rdfsbase,
    Mnemonic::Rdgsbase,
    Mnemonic::Wrfsbase,
    Mnemonic::Wrgsbase,
    // Their 512-byte images do not fit the window (fxsave/fxrstor are
    // skipped in 32-bit mode for the same reason).
    Mnemonic::Fxsave64,
    Mnemonic::Fxrstor64,
];

/// Classifies an instruction form for the x86-64 groups, or None when it
/// is not tested.
pub fn group_of64(code: Code) -> Option<Group> {
    let op = code.op_code();
    let m = code.mnemonic();
    if !op.is_instruction()
        || !op.mode64()
        || op.encoding() != EncodingKind::Legacy
        || op.is_privileged()
        || op.must_be_cpl0()
        || op.is_input_output()
        || op.address_size() == 16
        || SKIP_MNEMONICS.contains(&m)
        || SKIP_MNEMONICS64.contains(&m)
    {
        return None;
    }
    // Segment registers, far branches and 16-bit branches (which truncate
    // RIP on some CPUs) are out of scope, as in 32-bit mode.
    for i in 0..op.op_count() {
        if matches!(
            op.op_kind(i),
            K::seg_reg
                | K::farbr2_2
                | K::farbr4_2
                | K::br16_1
                | K::br16_2
                | K::es
                | K::cs
                | K::ss
                | K::ds
                | K::fs
                | K::gs
                | K::cr_reg
                | K::dr_reg
                | K::tr_reg
                | K::bnd_reg
                | K::r32_or_mem_mpx
                | K::r64_or_mem_mpx
                | K::mem_mpx
                | K::mem_mib
                | K::bnd_or_mem_mpx
                | K::xbegin_2
                | K::xbegin_4
                | K::brdisp_2
                | K::brdisp_4
                | K::sibmem
        ) {
            return None;
        }
    }
    // 16-bit stack frames, returns and pushes (as in 32-bit mode).
    if matches!(
        code,
        Code::Retnw
            | Code::Retnw_imm16
            | Code::Leavew
            | Code::Pushw_imm8
            | Code::Push_imm16
            | Code::Call_rm16
            | Code::Jmp_rm16
            | Code::Call_m1616
            | Code::Call_m1632
            | Code::Call_m1664
            | Code::Jmp_m1616
            | Code::Jmp_m1632
            | Code::Jmp_m1664
    ) {
        return None;
    }
    let feats = code.cpuid_features();
    let has_fpu_or_simd = (0..op.op_count()).any(|i| {
        matches!(
            op.op_kind(i),
            K::st0
                | K::sti_opcode
                | K::mm_reg
                | K::mm_rm
                | K::mm_or_mem
                | K::xmm_reg
                | K::xmm_rm
                | K::xmm_or_mem
        )
    });
    let other: Vec<CpuidFeature> = feats
        .iter()
        .copied()
        .filter(|f| *f != CpuidFeature::X64)
        .collect();
    if (integer_features_ok(&other) || other == [CpuidFeature::CMPXCHG16B]) && !has_fpu_or_simd {
        return Some(Group::Integer64);
    }
    if !other.is_empty()
        && other.iter().all(|f| {
            matches!(
                f,
                CpuidFeature::MMX | CpuidFeature::SSE | CpuidFeature::SSE2 | CpuidFeature::FXSR
            )
        })
    {
        return Some(Group::Sse64);
    }
    None
}

/// Byte registers except spl (rsp is the stack pointer). ah..bh cannot be
/// encoded with a REX prefix; forms that pick them together with a REX-only
/// register fail to encode and are generated again.
const R8: [Register; 19] = [
    Register::AL,
    Register::CL,
    Register::DL,
    Register::BL,
    Register::AH,
    Register::CH,
    Register::DH,
    Register::BH,
    Register::BPL,
    Register::SIL,
    Register::DIL,
    Register::R8L,
    Register::R9L,
    Register::R10L,
    Register::R11L,
    Register::R12L,
    Register::R13L,
    Register::R14L,
    Register::R15L,
];
/// General registers except rsp, by width.
fn pool(bits: u32) -> Vec<Register> {
    let first = match bits {
        16 => Register::AX,
        32 => Register::EAX,
        _ => Register::RAX,
    };
    (0..16).filter(|&n| n != 4).map(|n| reg(first, n)).collect()
}

/// The `n`th register of the family starting at `first` (rax, eax, ax,
/// xmm0, ...).
fn reg(first: Register, n: usize) -> Register {
    Register::values()
        .nth(first as usize + n)
        .expect("register number in range")
}

/// The 64-bit register number (0..15) of a general register.
fn num(r: Register) -> usize {
    r.full_register().number()
}

const MM: [Register; 8] = [
    Register::MM0,
    Register::MM1,
    Register::MM2,
    Register::MM3,
    Register::MM4,
    Register::MM5,
    Register::MM6,
    Register::MM7,
];

fn xmm(rng: &mut Rng) -> Register {
    reg(Register::XMM0, rng.below(16) as usize)
}

/// A memory operand: base + index * scale + disp, RIP-relative (disp is
/// then the absolute target) or a 64-bit absolute offset (moffs).
#[derive(Clone, Copy)]
struct Mem {
    base: Register,
    index: Register,
    scale: u32,
    disp: i64,
    displ_size: u32,
}

impl Mem {
    fn ea(&self, regs: &[u64; 16]) -> u64 {
        if self.base == Register::RIP {
            return self.disp as u64;
        }
        let mut a = self.disp as u64;
        if self.base != Register::None {
            a = a.wrapping_add(regs[num(self.base)]);
        }
        if self.index != Register::None {
            let mut ix = regs[num(self.index)];
            if self.index.size() == 1 {
                ix &= 0xff; // xlat: [rbx + al]
            }
            a = a.wrapping_add(ix.wrapping_mul(self.scale as u64));
        }
        // 32-bit address size wraps.
        if self.base.size() == 4 || self.index.size() == 4 {
            a &= 0xffff_ffff;
        }
        a
    }
}

struct Builder<'r> {
    rng: &'r mut Rng,
    regs: [u64; 16],
    /// Registers whose values are pinned (pointers).
    pinned: [bool; 16],
    used: Vec<Register>,
    mem_patch: Vec<(u32, u32)>,
    align: u32,
}

impl<'r> Builder<'r> {
    fn free_reg(&mut self, pool: &[Register]) -> Register {
        for _ in 0..32 {
            let r = self.rng.pick(pool);
            if !self.pinned[num(r)] {
                self.used.push(r);
                return r;
            }
        }
        // Fall back to any register (may alias a pointer).
        let r = pool[0];
        self.used.push(r);
        r
    }

    fn pin(&mut self, r: Register, v: u64) {
        let n = num(r);
        self.regs[n] = v;
        self.pinned[n] = true;
    }

    /// Writes a 64-bit value into the window.
    fn patch64(&mut self, off: u32, v: u64) {
        self.mem_patch.push((off, v as u32));
        self.mem_patch.push((off + 4, (v >> 32) as u32));
    }

    fn memory_aligned(&mut self, size: u32, align: u32) -> Mem {
        self.align = align;
        let m = self.memory(size);
        self.align = 1;
        m
    }

    /// A memory operand of `size` bytes inside the window, avoiding
    /// registers already used as plain operands.
    fn memory(&mut self, size: u32) -> Mem {
        let size = size.max(1);
        let mut off = 0x40 + self.rng.below(0x100 - size.min(0x80) as u64) as u32;
        off -= off % self.align.max(1);
        let target = (MEM_BASE + off) as u64;
        let avoid: Vec<usize> = self.used.iter().map(|r| num(*r)).collect();
        let candidates: Vec<usize> = [3usize, 6, 7, 5, 0, 1, 2, 8, 9, 10, 11, 12, 13, 14, 15]
            .into_iter()
            .filter(|n| !avoid.contains(n) && !self.pinned[*n])
            .collect();
        let none = Register::None;
        let mem = |base, index, scale, disp, displ_size| Mem {
            base,
            index,
            scale,
            disp,
            displ_size,
        };
        match self.rng.below(10) {
            0 | 1 => mem(none, none, 1, target as i64, 4),
            2 | 3 if !candidates.is_empty() => {
                let b = reg(Register::RAX, self.rng.pick(&candidates));
                let disp = self.rng.below(32) as i64 - 16;
                self.pin(b, (target as i64 - disp) as u64);
                // [rbp]/[r13] have no displacement-free encoding.
                let ds = if disp == 0 && num(b) & 7 != 5 { 0 } else { 1 };
                mem(b, none, 1, disp, ds)
            }
            4 | 5 if candidates.len() >= 2 => self.base_index(&candidates, target, false),
            8 => mem(Register::RIP, none, 1, target as i64, 4),
            // 0x67: 32-bit address arithmetic.
            9 if candidates.len() >= 2 => self.base_index(&candidates, target, true),
            _ => {
                // [rsp + disp]: rsp points near the top of the window.
                let disp = target as i64 - STACK as i64;
                mem(Register::RSP, none, 1, disp, 1)
            }
        }
    }

    /// [base + index * scale + disp] addressing `target`, with registers
    /// from `candidates` (by number). With 32-bit addressing (`addr32`)
    /// the registers' upper halves are ignored, so they get garbage.
    fn base_index(&mut self, candidates: &[usize], target: u64, addr32: bool) -> Mem {
        let b = self.rng.pick(candidates);
        let rest: Vec<usize> = candidates.iter().copied().filter(|&r| r != b).collect();
        let ix = self.rng.pick(&rest);
        let scale = self.rng.pick(&[1u32, 2, 4, 8]);
        let mut iv = self.rng.below(4);
        let disp = self.rng.below(16) as i64 - 8;
        let mut bv = (target as i64 - disp - (iv * scale as u64) as i64) as u64;
        let first = if addr32 {
            bv |= self.rng.next() << 32;
            iv |= self.rng.next() << 32;
            Register::EAX
        } else {
            Register::RAX
        };
        let (base, index) = (reg(first, b), reg(first, ix));
        self.pin(base, bv);
        self.pin(index, iv);
        Mem {
            base,
            index,
            scale,
            disp,
            displ_size: 1,
        }
    }
}

/// Generates one case for `code` at `ip` (memory operands only when
/// `allow_mem`).
pub(crate) fn one_case_at(code: Code, rng: &mut Rng, ip: u32, allow_mem: bool) -> Option<Case> {
    let op = code.op_code();
    let m = code.mnemonic();
    let group = group_of64(code);
    let mut regs = [0u64; 16];
    for r in regs.iter_mut() {
        *r = rng.value64();
    }
    regs[4] = STACK as u64;
    let mut b = Builder {
        rng,
        regs,
        pinned: [false; 16],
        used: vec![],
        mem_patch: vec![],
        align: 1,
    };
    b.pinned[4] = true;
    let mut ins = Instruction::default();
    ins.set_code(code);
    let mem_size = |c: Code| c.op_code().memory_size().size() as u32;
    let mut mem_op: Option<Mem> = None;
    let mut imm_count = 0;
    // String instructions sometimes use 32-bit addressing (esi/edi/ecx).
    let addr32 = b.rng.chance(20);
    for i in 0..op.op_count() {
        let k = op.op_kind(i);
        let kind = match k {
            K::r8_reg | K::r8_opcode => {
                let r = b.free_reg(&R8);
                ins.set_op_register(i, r);
                OpKind::Register
            }
            K::r16_reg | K::r16_rm | K::r16_opcode | K::r16_reg_mem => {
                let r = b.free_reg(&pool(16));
                ins.set_op_register(i, r);
                OpKind::Register
            }
            K::r32_reg | K::r32_rm | K::r32_opcode | K::r32_reg_mem => {
                let r = b.free_reg(&pool(32));
                ins.set_op_register(i, r);
                OpKind::Register
            }
            K::r64_reg | K::r64_rm | K::r64_opcode | K::r64_reg_mem => {
                let r = b.free_reg(&pool(64));
                ins.set_op_register(i, r);
                OpKind::Register
            }
            K::r8_or_mem | K::r16_or_mem | K::r32_or_mem | K::r64_or_mem => {
                if !allow_mem || b.rng.chance(50) {
                    let r = match k {
                        K::r8_or_mem => b.free_reg(&R8),
                        K::r16_or_mem => b.free_reg(&pool(16)),
                        K::r32_or_mem => b.free_reg(&pool(32)),
                        _ => b.free_reg(&pool(64)),
                    };
                    ins.set_op_register(i, r);
                    OpKind::Register
                } else {
                    mem_op = Some(b.memory(mem_size(code)));
                    OpKind::Memory
                }
            }
            K::mem | K::mem_offs if !allow_mem => return None,
            K::mem_offs => {
                let off = 0x40 + b.rng.below(0x100) as u32;
                mem_op = Some(Mem {
                    base: Register::None,
                    index: Register::None,
                    scale: 1,
                    disp: (MEM_BASE + off) as i64,
                    displ_size: 8,
                });
                OpKind::Memory
            }
            K::mem => {
                let size = mem_size(code).max(1);
                mem_op = Some(
                    if group == Some(Group::Sse64) || m == Mnemonic::Cmpxchg16b {
                        b.memory_aligned(size, 16)
                    } else {
                        b.memory(size)
                    },
                );
                OpKind::Memory
            }
            K::al | K::cl | K::ax | K::dx | K::eax | K::rax => {
                let r = match k {
                    K::al => Register::AL,
                    K::cl => Register::CL,
                    K::ax => Register::AX,
                    K::dx => Register::DX,
                    K::eax => Register::EAX,
                    _ => Register::RAX,
                };
                b.used.push(r);
                ins.set_op_register(i, r);
                OpKind::Register
            }
            K::imm8 => {
                imm_count += 1;
                let v = if imm_count == 2 {
                    0
                } else {
                    b.rng.value() as u8
                };
                ins.set_immediate8(v);
                if imm_count == 2 {
                    OpKind::Immediate8_2nd
                } else {
                    OpKind::Immediate8
                }
            }
            K::imm8_const_1 => {
                ins.set_immediate8(1);
                OpKind::Immediate8
            }
            K::imm16 => {
                imm_count += 1;
                ins.set_immediate16(b.rng.value() as u16);
                OpKind::Immediate16
            }
            K::imm32 => {
                ins.set_immediate32(b.rng.value());
                OpKind::Immediate32
            }
            K::imm64 => {
                ins.set_immediate64(b.rng.value64());
                OpKind::Immediate64
            }
            K::imm8sex16 => {
                ins.set_immediate8to16(b.rng.value() as u8 as i8 as i16);
                OpKind::Immediate8to16
            }
            K::imm8sex32 => {
                ins.set_immediate8to32(b.rng.value() as u8 as i8 as i32);
                OpKind::Immediate8to32
            }
            K::imm8sex64 => {
                ins.set_immediate8to64(b.rng.value() as u8 as i8 as i64);
                OpKind::Immediate8to64
            }
            K::imm32sex64 => {
                ins.set_immediate32to64(b.rng.value() as i32 as i64);
                OpKind::Immediate32to64
            }
            K::br64_1 | K::br64_4 => {
                ins.set_near_branch64(TGT as u64);
                OpKind::NearBranch64
            }
            K::xmm_reg | K::xmm_rm => {
                let r = xmm(b.rng);
                ins.set_op_register(i, r);
                OpKind::Register
            }
            K::mm_reg | K::mm_rm => {
                let r = b.rng.pick(&MM);
                ins.set_op_register(i, r);
                OpKind::Register
            }
            K::xmm_or_mem | K::mm_or_mem => {
                if !allow_mem || b.rng.chance(50) {
                    let r = if k == K::xmm_or_mem {
                        xmm(b.rng)
                    } else {
                        b.rng.pick(&MM)
                    };
                    ins.set_op_register(i, r);
                    OpKind::Register
                } else {
                    mem_op = Some(b.memory_aligned(mem_size(code).max(1), 16));
                    OpKind::Memory
                }
            }
            K::seg_rSI if addr32 => OpKind::MemorySegESI,
            K::es_rDI if addr32 => OpKind::MemoryESEDI,
            K::seg_rSI => OpKind::MemorySegRSI,
            K::es_rDI => OpKind::MemoryESRDI,
            K::seg_rBX_al => {
                // xlat: [rbx + al].
                mem_op = Some(Mem {
                    base: Register::RBX,
                    index: Register::AL,
                    scale: 1,
                    disp: 0,
                    displ_size: 0,
                });
                OpKind::Memory
            }
            _ => return None,
        };
        ins.set_op_kind(i, kind);
    }
    if let Some(mo) = &mem_op {
        ins.set_memory_base(mo.base);
        ins.set_memory_index(mo.index);
        ins.set_memory_index_scale(mo.scale);
        ins.set_memory_displacement64(mo.disp as u64);
        ins.set_memory_displ_size(mo.displ_size);
        ins.set_segment_prefix(Register::None);
    }

    // Per-mnemonic adjustments.
    let mut eflags = (b.rng.next() as u32 & 0x8d5) | 2;
    let w = match ins.op0_kind() {
        OpKind::Register => ins.op0_register().size() as u32 * 8,
        OpKind::Memory => ins.memory_size().size() as u32 * 8,
        _ => 0,
    };
    use Mnemonic as M;
    match m {
        M::Movsb
        | M::Movsw
        | M::Movsd
        | M::Movsq
        | M::Stosb
        | M::Stosw
        | M::Stosd
        | M::Stosq
        | M::Lodsb
        | M::Lodsw
        | M::Lodsd
        | M::Lodsq
        | M::Cmpsb
        | M::Cmpsw
        | M::Cmpsd
        | M::Cmpsq
        | M::Scasb
        | M::Scasw
        | M::Scasd
        | M::Scasq
            if ins.is_string_instruction() =>
        {
            b.regs[6] = (MEM_BASE + 0x80) as u64 + b.rng.below(8);
            b.regs[7] = (MEM_BASE + 0x100) as u64 + b.rng.below(8);
            if b.rng.chance(15) {
                // Overlapping copies exercise the slow path.
                b.regs[7] = b.regs[6] + b.rng.below(6);
            }
            b.regs[1] = b.rng.below(7);
            if addr32 {
                // Only esi, edi and ecx take part.
                for r in [1, 6, 7] {
                    b.regs[r] |= b.rng.next() << 32;
                }
            }
            if op.can_use_rep_prefix() && b.rng.chance(60) {
                ins.set_has_rep_prefix(true);
            } else if op.can_use_repne_prefix() && b.rng.chance(50) {
                ins.set_has_repne_prefix(true);
            }
            if b.rng.chance(30) {
                eflags |= 0x400;
            }
        }
        M::Xlatb => b.pin(Register::RBX, (MEM_BASE + 0x40) as u64),
        M::Ldmxcsr => {
            // A valid MXCSR (reserved bits clear) with a random rounding mode.
            let ea = mem_op?.ea(&b.regs);
            let v = 0x1f80 | (b.rng.below(4) as u32) << 13;
            b.mem_patch.push((ea as u32 - MEM_BASE, v));
        }
        M::Loop | M::Loope | M::Loopne | M::Jecxz | M::Jrcxz => {
            b.regs[1] = b
                .rng
                .pick(&[0, 1, 2, 3, u64::MAX, 0x1_0000_0000, 0x1_0000_0001]);
        }
        M::Leave => b.regs[5] = (MEM_BASE + 0x1a0) as u64 + 8 * b.rng.below(4),
        M::Ret => {
            b.patch64(STACK - MEM_BASE, RET_TGT as u64);
            if op.op_count() == 1 {
                ins.set_immediate16(8 * b.rng.below(4) as u16);
            }
        }
        M::Popfq => {
            let v = (b.rng.next() as u32 & 0x8d5) | 2 | if b.rng.chance(30) { 0x400 } else { 0 };
            b.patch64(STACK - MEM_BASE, v as u64);
        }
        M::Jmp | M::Call if op.op_kind(0) != K::br64_1 && op.op_kind(0) != K::br64_4 => {
            match ins.op0_kind() {
                OpKind::Register => {
                    let r = ins.op0_register();
                    b.regs[num(r)] = TGT as u64;
                }
                _ => {
                    let ea = mem_op?.ea(&b.regs);
                    b.patch64(ea as u32 - MEM_BASE, TGT as u64);
                }
            }
        }
        M::Shl
        | M::Sal
        | M::Shr
        | M::Sar
        | M::Rol
        | M::Ror
        | M::Rcl
        | M::Rcr
        | M::Shld
        | M::Shrd => {
            // Interesting counts in cl.
            let c = b.rng.pick(&[
                0u64, 1, 2, 7, 8, 9, 15, 16, 17, 31, 32, 33, 48, 62, 63, 64, 65,
            ]);
            if !b.pinned[1] {
                b.regs[1] = (b.regs[1] & !0xff) | c;
            }
            if ins.op_kinds().any(|k| k == OpKind::Immediate8) && b.rng.chance(50) {
                ins.set_immediate8(c as u8);
            }
        }
        M::Div | M::Idiv => div_inputs(&mut b, &ins, mem_op, w, m == M::Idiv),
        M::Cmpxchg if b.rng.chance(50) => {
            // Equal operands half the time: rax (low w bits) = destination.
            let mask = width_mask(w);
            let dst = match ins.op0_kind() {
                OpKind::Register => b.regs[num(ins.op0_register())] >> high8_shift(&ins),
                _ => {
                    let ea = mem_op?.ea(&b.regs) as u32 - MEM_BASE;
                    let v = b.rng.value64();
                    b.patch64(ea, v);
                    v
                }
            };
            if !b.pinned[0] {
                b.regs[0] = (b.regs[0] & !mask) | (dst & mask);
            }
        }
        M::Cmpxchg8b | M::Cmpxchg16b if b.rng.chance(50) => {
            // Equal comparands half the time: memory = rdx:rax (edx:eax).
            let ea = mem_op?.ea(&b.regs) as u32 - MEM_BASE;
            if m == M::Cmpxchg8b {
                let v = (b.regs[2] as u32 as u64) << 32 | b.regs[0] as u32 as u64;
                b.patch64(ea, v);
            } else {
                let (lo, hi) = (b.regs[0], b.regs[2]);
                b.patch64(ea, lo);
                b.patch64(ea + 8, hi);
            }
        }
        _ => {}
    }
    // Lock prefix on memory destinations.
    if op.can_use_lock_prefix() && ins.op0_kind() == OpKind::Memory && b.rng.chance(30) {
        ins.set_has_lock_prefix(true);
    }
    // Memory operands of bit-test instructions with a register offset can
    // reach outside the operand; keep the offset small.
    if matches!(m, M::Bt | M::Bts | M::Btr | M::Btc)
        && ins.op0_kind() == OpKind::Memory
        && ins.op1_kind() == OpKind::Register
    {
        let r = ins.op1_register();
        let n = num(r);
        if !b.pinned[n] {
            let off = (b.rng.below(128) as i64 - 64) as u64;
            let mask = width_mask(r.size() as u32 * 8);
            b.regs[n] = (b.regs[n] & !mask) | (off & mask);
        }
    }
    let mut enc = Encoder::new(64);
    let len = enc.encode(&ins, ip as u64).ok()?;
    let bytes = enc.take_buffer();
    // Sanity: the bytes must decode back to the same form.
    let mut dec = iced_x86::Decoder::with_ip(64, &bytes, ip as u64, iced_x86::DecoderOptions::NONE);
    let back = dec.decode();
    if back.code() != code || back.len() != len {
        return None;
    }
    let fx = (group == Some(Group::Sse64)).then(|| hex(&simd_state64(b.rng)));
    Some(Case {
        form: format!("{code:?}"),
        code: hex(&bytes),
        x64: true,
        regs: b.regs.to_vec(),
        eflags,
        mem_seed: b.rng.next(),
        mem_patch: b.mem_patch,
        fx,
    })
}

fn width_mask(w: u32) -> u64 {
    if w >= 64 {
        u64::MAX
    } else {
        (1 << w) - 1
    }
}

/// Shift of a byte register's value within its 64-bit register (8 for
/// ah..bh).
fn high8_shift(ins: &Instruction) -> u32 {
    let r = ins.op0_register();
    if matches!(r, Register::AH | Register::CH | Register::DH | Register::BH) {
        8
    } else {
        0
    }
}

/// Dividends for div/idiv: mostly ones whose quotient fits. 64-bit
/// divisions also get full 128-bit dividends in rdx:rax, built from a
/// divisor chosen here; some divisors are zero, and the rest of the
/// dividends are random (often #DE from an overflowing quotient).
fn div_inputs(b: &mut Builder, ins: &Instruction, mem_op: Option<Mem>, w: u32, signed: bool) {
    if b.rng.chance(5) {
        // Division by zero.
        match (ins.op0_kind(), mem_op) {
            (OpKind::Register, _) => {
                let mask = width_mask(w) << high8_shift(ins);
                b.regs[num(ins.op0_register())] &= !mask;
            }
            (_, Some(mo)) => {
                let ea = mo.ea(&b.regs) as u32 - MEM_BASE;
                b.patch64(ea, 0);
            }
            _ => {}
        }
        return;
    }
    if b.pinned[2] || b.pinned[0] {
        return;
    }
    let mode = b.rng.below(10);
    if w == 64 {
        let divisor_reg = (ins.op0_kind() == OpKind::Register).then(|| num(ins.op0_register()));
        if mode < 4 && !matches!(divisor_reg, Some(0) | Some(2)) {
            // A random divisor and a dividend with a quotient that fits.
            let d = match b.rng.below(3) {
                0 => b.rng.below(1000) + 1,
                1 => b.rng.value() as u64 | 1,
                _ => b.rng.next() | 1,
            };
            let d = if signed && b.rng.chance(50) {
                d.wrapping_neg()
            } else {
                d
            };
            match (divisor_reg, mem_op) {
                (Some(n), _) => b.regs[n] = d,
                (None, Some(mo)) => {
                    let ea = mo.ea(&b.regs) as u32 - MEM_BASE;
                    b.patch64(ea, d);
                }
                _ => return,
            }
            let dividend: u128 = if signed {
                let d = d as i64 as i128;
                let q = (b.rng.next() as i64 >> b.rng.below(63).max(2)) as i128;
                let r = (b.rng.next() as i128).rem_euclid(d.abs()) * if q < 0 { -1 } else { 1 };
                (q * d + r) as u128
            } else {
                let hi = b.rng.next() % d;
                (hi as u128) << 64 | b.rng.next() as u128
            };
            b.regs[0] = dividend as u64;
            b.regs[2] = (dividend >> 64) as u64;
            return;
        }
        if mode < 8 {
            // A small dividend in rax.
            b.regs[2] = if signed && b.rng.chance(50) {
                ((b.regs[0] as i64) >> 63) as u64
            } else {
                b.rng.below(4)
            };
        }
        return;
    }
    if mode >= 8 {
        return;
    }
    // The high half: dx/edx for 16/32-bit, ah for 8-bit divisions. Bits
    // above it stay random (32-bit results zero-extend).
    let (reg, shift) = if w == 8 { (0, 8) } else { (2, 0) };
    let half = if w == 8 { 8 } else { w };
    let low = if w == 8 { b.regs[0] & 0xff } else { b.regs[0] };
    let hi = if signed && b.rng.chance(50) {
        if low >> (half - 1) & 1 != 0 {
            width_mask(half)
        } else {
            0
        }
    } else {
        b.rng.below(4)
    };
    let mask = width_mask(half) << shift;
    b.regs[reg] = (b.regs[reg] & !mask) | hi << shift;
}

/// FXSAVE image for x86-64 SIMD cases: as [`simd_state`], plus xmm8-15.
pub fn simd_state64(rng: &mut Rng) -> Vec<u8> {
    let mut fx = simd_state(rng);
    for i in 8..16 {
        let v = xmm_value(rng);
        fx[160 + i * 16..176 + i * 16].copy_from_slice(&v);
    }
    fx
}

/// Hand-written cases for forms the generator skips, and the fusion pairs.
pub fn extra_cases(group: Group, rng: &mut Rng) -> Vec<Case> {
    if group == Group::Fusion64 {
        return crate::gen::fusion_cases_in(Group::Integer64, 4000, rng);
    }
    let mut out = vec![];
    if group != Group::Integer64 {
        return out;
    }
    // enter imm16, 0
    for size in [0u16, 8, 16, 40] {
        let mut ins = Instruction::with2(Code::Enterq_imm16_imm8, size as u32, 0u32).unwrap();
        ins.set_op1_kind(OpKind::Immediate8_2nd);
        let mut enc = Encoder::new(64);
        enc.encode(&ins, INS as u64).unwrap();
        let mut regs = vec![0u64; 16];
        for r in regs.iter_mut() {
            *r = rng.value64();
        }
        regs[4] = STACK as u64;
        out.push(Case {
            form: "Enterq_imm16_imm8".into(),
            code: hex(&enc.take_buffer()),
            x64: true,
            regs,
            eflags: 2,
            mem_seed: rng.next(),
            mem_patch: vec![],
            fx: None,
        });
    }
    out
}
