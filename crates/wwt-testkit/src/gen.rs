//! Generates instruction test cases from iced-x86's table of instruction
//! forms: every legacy-encoded form valid in 32-bit user mode, with random
//! operands chosen so memory accesses land in the data window and branches
//! land on the oracle's stubs.

use iced_x86::{
    Code, CpuidFeature, Encoder, EncodingKind, Instruction, MemoryOperand, Mnemonic,
    OpCodeOperandKind as K, OpKind, Register,
};

use crate::case::{hex, Case};
use crate::layout::*;

/// Instruction groups, each recorded in its own fixture file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Integer,
    /// Flag producer followed by a flag consumer (tests flag fusion).
    Fusion,
    X87,
    Sse,
}

impl Group {
    pub fn name(self) -> &'static str {
        match self {
            Group::Integer => "integer",
            Group::Fusion => "fusion",
            Group::X87 => "x87",
            Group::Sse => "sse",
        }
    }
    pub fn all() -> [Group; 4] {
        [Group::Integer, Group::Fusion, Group::X87, Group::Sse]
    }
}

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    pub fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
    pub fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[self.below(xs.len() as u64) as usize]
    }
    /// A 32-bit value biased toward edge cases.
    pub fn value(&mut self) -> u32 {
        match self.below(10) {
            0 => self.pick(&[0, 1, 2, 0xffff_ffff, 0x8000_0000, 0x7fff_ffff, 0x80, 0x7f, 0xff, 0x8000, 0x7fff, 0xffff]),
            1 => self.below(64) as u32,
            2 => (self.below(64) as u32).wrapping_neg(),
            3 => 1u32 << self.below(32),
            _ => self.next() as u32,
        }
    }
}

const SKIP_MNEMONICS: &[Mnemonic] = &[
    Mnemonic::Int,
    Mnemonic::Int1,
    Mnemonic::Int3,
    Mnemonic::Into,
    Mnemonic::Hlt,
    Mnemonic::Cli,
    Mnemonic::Sti,
    Mnemonic::Iret,
    Mnemonic::Iretd,
    Mnemonic::Retf,
    Mnemonic::Bound,
    Mnemonic::Arpl,
    Mnemonic::Lar,
    Mnemonic::Lsl,
    Mnemonic::Verr,
    Mnemonic::Verw,
    Mnemonic::Sgdt,
    Mnemonic::Sidt,
    Mnemonic::Sldt,
    Mnemonic::Str,
    Mnemonic::Smsw,
    Mnemonic::Lds,
    Mnemonic::Les,
    Mnemonic::Lfs,
    Mnemonic::Lgs,
    Mnemonic::Lss,
    Mnemonic::Cpuid,
    Mnemonic::Rdtsc,
    Mnemonic::Rdtscp,
    Mnemonic::Rdpmc,
    Mnemonic::Salc,
    Mnemonic::Ud0,
    Mnemonic::Ud1,
    Mnemonic::Syscall,
    Mnemonic::Sysenter,
    Mnemonic::Sysexit,
    Mnemonic::Sysret,
    Mnemonic::Aaa,
    Mnemonic::Aas,
    Mnemonic::Daa,
    Mnemonic::Das,
    Mnemonic::Aam,
    Mnemonic::Aad,
    Mnemonic::Xbegin,
    Mnemonic::Xend,
    Mnemonic::Xabort,
    Mnemonic::Xtest,
    Mnemonic::Insb,
    Mnemonic::Insw,
    Mnemonic::Insd,
    Mnemonic::Outsb,
    Mnemonic::Outsw,
    Mnemonic::Outsd,
    Mnemonic::In,
    Mnemonic::Out,
    Mnemonic::Pushf, // 16-bit flags images: tested through pushfd
    Mnemonic::Popf,
    Mnemonic::Enter, // tested by hand-written cases (level must be 0)
    Mnemonic::Wait,
    Mnemonic::Clui,
    Mnemonic::Stui,
    Mnemonic::Testui,
    Mnemonic::Uiret,
    Mnemonic::Serialize,
    Mnemonic::Rdpid,
    Mnemonic::Rdpkru,
    Mnemonic::Wrpkru,
    Mnemonic::Xgetbv,
];

fn integer_features_ok(f: &[CpuidFeature]) -> bool {
    f.iter().all(|f| {
        matches!(
            f,
            CpuidFeature::INTEL8086
                | CpuidFeature::INTEL8086_ONLY
                | CpuidFeature::INTEL186
                | CpuidFeature::INTEL286
                | CpuidFeature::INTEL386
                | CpuidFeature::INTEL486
                | CpuidFeature::CMOV
                | CpuidFeature::CX8
                | CpuidFeature::MULTIBYTENOP
                | CpuidFeature::POPCNT
                | CpuidFeature::LZCNT
                | CpuidFeature::BMI1
        )
    })
}

/// Classifies an instruction form, or None when it is not tested.
pub fn group_of(code: Code) -> Option<Group> {
    let op = code.op_code();
    if !op.is_instruction()
        || !op.mode32()
        || op.encoding() != EncodingKind::Legacy
        || op.is_privileged()
        || op.must_be_cpl0()
        || op.is_input_output()
        || op.address_size() == 16
        || SKIP_MNEMONICS.contains(&code.mnemonic())
    {
        return None;
    }
    // Segment registers, far branches and 16-bit branches are out of scope.
    for i in 0..op.op_count() {
        match op.op_kind(i) {
            K::seg_reg | K::farbr2_2 | K::farbr4_2 | K::br16_1 | K::br16_2 | K::es | K::cs
            | K::ss | K::ds | K::fs | K::gs | K::cr_reg | K::dr_reg | K::tr_reg | K::bnd_reg
            | K::r32_or_mem_mpx | K::mem_mpx | K::mem_mib | K::bnd_or_mem_mpx | K::seg_rBX_al
            | K::xbegin_2 | K::xbegin_4 | K::brdisp_2 | K::brdisp_4 | K::sibmem => {
                return if code.mnemonic() == Mnemonic::Xlatb { Some(Group::Integer) } else { None }
            }
            _ => {}
        }
    }
    // 16-bit stack frames and 16-bit returns.
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
            | Code::Jmp_m1616
            | Code::Jmp_m1632
    ) {
        return None;
    }
    let feats = code.cpuid_features();
    if integer_features_ok(feats) {
        let has_fpu_or_simd = (0..op.op_count()).any(|i| {
            matches!(
                op.op_kind(i),
                K::st0 | K::sti_opcode | K::mm_reg | K::mm_rm | K::mm_or_mem | K::xmm_reg | K::xmm_rm | K::xmm_or_mem
            )
        });
        if !has_fpu_or_simd {
            return Some(Group::Integer);
        }
    }
    if feats.contains(&CpuidFeature::FPU) || feats.contains(&CpuidFeature::FPU287) || feats.contains(&CpuidFeature::FPU387) || feats.contains(&CpuidFeature::CMOV) && format!("{:?}", code.mnemonic()).starts_with('F') {
        return Some(Group::X87);
    }
    if feats.iter().all(|f| {
        matches!(
            f,
            CpuidFeature::MMX | CpuidFeature::SSE | CpuidFeature::SSE2 | CpuidFeature::FXSR
        )
    }) {
        return Some(Group::Sse);
    }
    None
}

pub fn forms(group: Group) -> Vec<Code> {
    Code::values().filter(|&c| group_of(c) == Some(group)).collect()
}

const R8: [Register; 8] = [
    Register::AL,
    Register::CL,
    Register::DL,
    Register::BL,
    Register::AH,
    Register::CH,
    Register::DH,
    Register::BH,
];
const R16: [Register; 7] = [
    Register::AX,
    Register::CX,
    Register::DX,
    Register::BX,
    Register::BP,
    Register::SI,
    Register::DI,
];
const R32: [Register; 7] = [
    Register::EAX,
    Register::ECX,
    Register::EDX,
    Register::EBX,
    Register::EBP,
    Register::ESI,
    Register::EDI,
];

fn full32(r: Register) -> Register {
    r.full_register32()
}

struct Builder<'r> {
    rng: &'r mut Rng,
    regs: [u32; 8],
    /// Registers whose values are pinned (pointers).
    pinned: [bool; 8],
    used: Vec<Register>,
    mem_patch: Vec<(u32, u32)>,
}

impl<'r> Builder<'r> {
    fn free_reg(&mut self, pool: &[Register]) -> Register {
        for _ in 0..32 {
            let r = self.rng.pick(pool);
            if !self.pinned[full32(r).number()] {
                self.used.push(r);
                return r;
            }
        }
        // Fall back to any register (may alias a pointer).
        let r = pool[0];
        self.used.push(r);
        r
    }

    fn pin(&mut self, r: Register, v: u32) {
        let n = full32(r).number();
        self.regs[n] = v;
        self.pinned[n] = true;
    }

    /// A memory operand of `size` bytes inside the window, avoiding
    /// registers already used as plain operands.
    fn memory(&mut self, size: u32) -> MemoryOperand {
        let size = size.max(1);
        let off = 0x40 + self.rng.below(0x100 - size.min(0x80) as u64) as u32;
        let target = MEM_BASE + off;
        let avoid: Vec<usize> = self.used.iter().map(|r| full32(*r).number()).collect();
        let candidates: Vec<Register> = [Register::EBX, Register::ESI, Register::EDI, Register::EBP, Register::EAX, Register::ECX, Register::EDX]
            .into_iter()
            .filter(|r| !avoid.contains(&r.number()) && !self.pinned[r.number()])
            .collect();
        match self.rng.below(4) {
            0 => MemoryOperand::with_displ(target as u64, 4),
            1 if !candidates.is_empty() => {
                let b = self.rng.pick(&candidates);
                let disp = self.rng.below(32) as i64 - 16;
                self.pin(b, (target as i64 - disp) as u32);
                MemoryOperand::with_base_displ(b, disp)
            }
            2 if candidates.len() >= 2 => {
                let b = self.rng.pick(&candidates);
                let rest: Vec<Register> = candidates.iter().copied().filter(|&r| r != b).collect();
                let ix = self.rng.pick(&rest);
                let scale = self.rng.pick(&[1u32, 2, 4, 8]);
                let iv = self.rng.below(4) as u32;
                let disp = self.rng.below(16) as i64 - 8;
                let base = (target as i64 - disp - (iv * scale) as i64) as u32;
                self.pin(b, base);
                self.pin(ix, iv);
                MemoryOperand::with_base_index_scale_displ_size(b, ix, scale, disp, 1)
            }
            _ => {
                // [esp + disp]: esp points near the top of the window.
                let disp = (target as i64) - STACK as i64;
                MemoryOperand::with_base_displ(Register::ESP, disp)
            }
        }
    }
}

/// Generates `n` cases for one instruction form.
pub fn cases_for(code: Code, n: usize, rng: &mut Rng) -> Vec<Case> {
    let mut out = vec![];
    let mut attempts = 0;
    while out.len() < n && attempts < n * 4 {
        attempts += 1;
        if let Some(c) = one_case(code, rng) {
            out.push(c);
        }
    }
    out
}

fn one_case(code: Code, rng: &mut Rng) -> Option<Case> {
    one_case_at(code, rng, INS, true)
}

fn one_case_at(code: Code, rng: &mut Rng, ip: u32, allow_mem: bool) -> Option<Case> {
    let op = code.op_code();
    let m = code.mnemonic();
    let mut regs = [0u32; 8];
    for r in regs.iter_mut() {
        *r = rng.value();
    }
    regs[4] = STACK;
    let mut b = Builder {
        rng,
        regs,
        pinned: [false; 8],
        used: vec![],
        mem_patch: vec![],
    };
    b.pinned[4] = true;
    let mut ins = Instruction::default();
    ins.set_code(code);
    let mem_size = |c: Code| c.op_code().memory_size().size() as u32;
    let mut mem_op: Option<MemoryOperand> = None;
    let mut imm_count = 0;
    for i in 0..op.op_count() {
        let k = op.op_kind(i);
        let kind = match k {
            K::r8_reg | K::r8_opcode => {
                let r = b.free_reg(&R8);
                ins.set_op_register(i, r);
                OpKind::Register
            }
            K::r16_reg | K::r16_rm | K::r16_opcode | K::r16_reg_mem => {
                let r = b.free_reg(&R16);
                ins.set_op_register(i, r);
                OpKind::Register
            }
            K::r32_reg | K::r32_rm | K::r32_opcode | K::r32_reg_mem => {
                let r = b.free_reg(&R32);
                ins.set_op_register(i, r);
                OpKind::Register
            }
            K::r8_or_mem | K::r16_or_mem | K::r32_or_mem => {
                if !allow_mem || b.rng.chance(50) {
                    let pool: &[Register] = match k {
                        K::r8_or_mem => &R8,
                        K::r16_or_mem => &R16,
                        _ => &R32,
                    };
                    let r = b.free_reg(pool);
                    ins.set_op_register(i, r);
                    OpKind::Register
                } else {
                    mem_op = Some(b.memory(mem_size(code)));
                    OpKind::Memory
                }
            }
            K::mem | K::mem_offs if !allow_mem => return None,
            K::mem | K::mem_offs => {
                mem_op = Some(if k == K::mem_offs {
                    let off = 0x40 + b.rng.below(0x100) as u32;
                    MemoryOperand::with_displ((MEM_BASE + off) as u64, 4)
                } else {
                    b.memory(mem_size(code).max(1))
                });
                OpKind::Memory
            }
            K::al | K::cl | K::ax | K::dx | K::eax => {
                let r = match k {
                    K::al => Register::AL,
                    K::cl => Register::CL,
                    K::ax => Register::AX,
                    K::dx => Register::DX,
                    _ => Register::EAX,
                };
                b.used.push(r);
                ins.set_op_register(i, r);
                OpKind::Register
            }
            K::imm8 => {
                imm_count += 1;
                let v = if imm_count == 2 { 0 } else { b.rng.value() as u8 };
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
            K::imm8sex16 => {
                ins.set_immediate8to16(b.rng.value() as u8 as i8 as i16);
                OpKind::Immediate8to16
            }
            K::imm8sex32 => {
                ins.set_immediate8to32(b.rng.value() as u8 as i8 as i32);
                OpKind::Immediate8to32
            }
            K::br32_1 | K::br32_4 => {
                ins.set_near_branch32(TGT);
                OpKind::NearBranch32
            }
            K::seg_rSI => OpKind::MemorySegESI,
            K::es_rDI => OpKind::MemoryESEDI,
            K::seg_rDI => OpKind::MemorySegEDI,
            _ => return None,
        };
        ins.set_op_kind(i, kind);
    }
    if let Some(mo) = &mem_op {
        ins.set_memory_base(mo.base);
        ins.set_memory_index(mo.index);
        ins.set_memory_index_scale(mo.scale);
        ins.set_memory_displacement32(mo.displacement as u32);
        ins.set_memory_displ_size(if mo.base == Register::None && mo.index == Register::None {
            4
        } else if mo.displacement == 0 && mo.base != Register::EBP {
            0
        } else {
            1
        });
        ins.set_segment_prefix(Register::None);
    }
    let mem_ea = |b: &Builder| -> Option<u32> {
        mem_op.as_ref().map(|mo| {
            let mut a = mo.displacement as u32;
            if mo.base != Register::None {
                a = a.wrapping_add(b.regs[mo.base.number()]);
            }
            if mo.index != Register::None {
                a = a.wrapping_add(b.regs[mo.index.number()].wrapping_mul(mo.scale));
            }
            a
        })
    };

    // Per-mnemonic adjustments.
    let mut eflags = (b.rng.next() as u32 & 0x8d5) | 2;
    use Mnemonic as M;
    match m {
        M::Movsb | M::Movsw | M::Movsd | M::Stosb | M::Stosw | M::Stosd | M::Lodsb | M::Lodsw
        | M::Lodsd | M::Cmpsb | M::Cmpsw | M::Cmpsd | M::Scasb | M::Scasw | M::Scasd => {
            b.regs[6] = MEM_BASE + 0x80 + b.rng.below(8) as u32;
            b.regs[7] = MEM_BASE + 0x100 + b.rng.below(8) as u32;
            if b.rng.chance(15) {
                // Overlapping copies exercise the slow path.
                b.regs[7] = b.regs[6] + b.rng.below(6) as u32;
            }
            b.regs[1] = b.rng.below(7) as u32;
            if op.can_use_rep_prefix() && b.rng.chance(60) {
                ins.set_has_rep_prefix(true);
            } else if op.can_use_repne_prefix() && b.rng.chance(50) {
                ins.set_has_repne_prefix(true);
            }
            if b.rng.chance(30) {
                eflags |= 0x400;
            }
        }
        M::Xlatb => b.regs[3] = MEM_BASE + 0x40,
        M::Loop | M::Loope | M::Loopne | M::Jecxz => {
            b.regs[1] = b.rng.pick(&[0, 1, 2, 3, 0xffff_ffff]);
        }
        M::Leave => b.regs[5] = MEM_BASE + 0x1a0 + 4 * b.rng.below(4) as u32,
        M::Ret => {
            b.mem_patch.push((STACK - MEM_BASE, RET_TGT));
            if op.op_count() == 1 {
                ins.set_immediate16(4 * b.rng.below(4) as u16);
            }
        }
        M::Popfd => {
            let v = (b.rng.next() as u32 & 0x8d5) | 2 | if b.rng.chance(30) { 0x400 } else { 0 };
            b.mem_patch.push((STACK - MEM_BASE, v));
        }
        M::Jmp | M::Call if op.op_kind(0) != K::br32_1 && op.op_kind(0) != K::br32_4 => {
            match ins.op0_kind() {
                OpKind::Register => {
                    let r = ins.op0_register();
                    b.regs[full32(r).number()] = TGT;
                }
                _ => {
                    let ea = mem_ea(&b)?;
                    b.mem_patch.push((ea - MEM_BASE, TGT));
                }
            }
        }
        M::Shl | M::Sal | M::Shr | M::Sar | M::Rol | M::Ror | M::Rcl | M::Rcr | M::Shld
        | M::Shrd => {
            // Interesting counts in cl.
            let c = b.rng.pick(&[0u32, 1, 2, 7, 8, 9, 15, 16, 17, 31, 32, 33, 63]);
            if !b.pinned[1] {
                b.regs[1] = (b.regs[1] & !0xff) | c;
            }
            if ins.op_kinds().any(|k| k == OpKind::Immediate8) && b.rng.chance(50) {
                ins.set_immediate8(c as u8);
            }
        }
        M::Div | M::Idiv => {
            // Mostly non-faulting divisions: small high halves.
            if b.rng.chance(80) && !b.pinned[2] {
                b.regs[2] = b.rng.below(4) as u32;
                if m == M::Idiv && b.rng.chance(50) {
                    b.regs[2] = if b.regs[0] & 0x8000_0000 != 0 { u32::MAX } else { 0 };
                }
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
    if matches!(m, M::Bt | M::Bts | M::Btr | M::Btc) && ins.op0_kind() == OpKind::Memory && ins.op1_kind() == OpKind::Register {
        let r = ins.op1_register();
        let n = full32(r).number();
        if !b.pinned[n] {
            let off = (b.rng.below(128) as i32 - 64) as u32;
            b.regs[n] = if r.size() == 2 { (b.regs[n] & 0xffff_0000) | (off & 0xffff) } else { off };
        }
    }
    let mut enc = Encoder::new(32);
    let len = enc.encode(&ins, ip as u64).ok()?;
    let bytes = enc.take_buffer();
    // Sanity: the bytes must decode back to the same form.
    let mut dec = iced_x86::Decoder::with_ip(32, &bytes, ip as u64, iced_x86::DecoderOptions::NONE);
    let back = dec.decode();
    if back.code() != code || back.len() != len {
        return None;
    }
    Some(Case {
        form: format!("{code:?}"),
        code: hex(&bytes),
        regs: b.regs,
        eflags,
        mem_seed: b.rng.next(),
        mem_patch: b.mem_patch,
        fx: None,
    })
}

/// Hand-written cases for forms the generator skips.
pub fn extra_cases(group: Group, rng: &mut Rng) -> Vec<Case> {
    let mut out = vec![];
    if group == Group::Fusion {
        return fusion_cases(4000, rng);
    }
    if group != Group::Integer {
        return out;
    }
    // enter imm16, 0
    for size in [0u16, 4, 8, 32] {
        let mut ins = Instruction::with2(Code::Enterd_imm16_imm8, size as u32, 0u32).unwrap();
        ins.set_op1_kind(OpKind::Immediate8_2nd);
        let mut enc = Encoder::new(32);
        enc.encode(&ins, INS as u64).unwrap();
        let mut regs = [0u32; 8];
        for r in regs.iter_mut() {
            *r = rng.value();
        }
        regs[4] = STACK;
        out.push(Case {
            form: "Enterd_imm16_imm8".into(),
            code: hex(&enc.take_buffer()),
            regs,
            eflags: 2,
            mem_seed: rng.next(),
            mem_patch: vec![],
            fx: None,
        });
    }
    out
}

/// Flag producer/consumer pairs.
pub fn fusion_cases(n: usize, rng: &mut Rng) -> Vec<Case> {
    use Mnemonic as M;
    let ints = forms(Group::Integer);
    let producers: Vec<Code> = ints
        .iter()
        .copied()
        .filter(|c| {
            matches!(
                c.mnemonic(),
                M::Add | M::Sub | M::Cmp | M::Test | M::And | M::Or | M::Xor | M::Inc | M::Dec
                    | M::Neg | M::Shl | M::Shr | M::Sar | M::Adc | M::Sbb | M::Imul | M::Bt
                    | M::Bsf | M::Rol | M::Rcr | M::Mul | M::Cmpxchg | M::Xadd
            )
        })
        .collect();
    let consumers: Vec<Code> = ints
        .iter()
        .copied()
        .filter(|c| {
            let m = format!("{:?}", c.mnemonic());
            (m.starts_with('J') && m != "Jmp" && m != "Jecxz" && m != "Jcxz")
                || m.starts_with("Set")
                || m.starts_with("Cmov")
                || matches!(c.mnemonic(), M::Adc | M::Sbb | M::Lahf | M::Pushfd | M::Rcl | M::Inc)
        })
        .collect();
    let mut out = vec![];
    while out.len() < n {
        let p = rng.pick(&producers);
        let c = rng.pick(&consumers);
        let Some(mut first) = one_case_at(p, rng, INS, true) else { continue };
        let len1 = first.code.len() as u32 / 2;
        let Some(second) = one_case_at(c, rng, INS + len1, false) else { continue };
        if len1 + second.code.len() as u32 / 2 > 15 {
            continue;
        }
        first.form = format!("{p:?}+{c:?}");
        first.code.push_str(&second.code);
        // Skip pairs whose consumer reads flags the producer left undefined.
        let bytes = first.code_bytes();
        let mut d = iced_x86::Decoder::with_ip(32, &bytes, INS as u64, iced_x86::DecoderOptions::NONE);
        let i1 = d.decode();
        let i2 = d.decode();
        let (undef, _) = crate::compare::undefined_flags(&i1, &first);
        if undef & crate::compare::rflags_read(&i2) != 0 {
            continue;
        }
        out.push(first);
    }
    out
}
