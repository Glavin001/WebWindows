//! Layer 4's output: a small typed intermediate form we own.
//!
//! Functions are control-flow graphs of blocks holding three-address
//! instructions over virtual registers (vregs). The first [`NUM_STATE`]
//! vregs are *state vregs*: guest registers and flag state that have a home
//! in the per-thread CPU struct. Inside a function they live in WebAssembly
//! locals; code generation loads them on entry when live and writes them back
//! at calls, exits and fault points.

use std::fmt;

use crate::abi::cpu;

pub type V = u32;
pub type BlockId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ty {
    I32,
    I64,
    F32,
    F64,
    V128,
}

// ---- State vregs ---------------------------------------------------------

pub const EAX: V = 0;
pub const ECX: V = 1;
pub const EDX: V = 2;
pub const EBX: V = 3;
pub const ESP: V = 4;
pub const EBP: V = 5;
pub const ESI: V = 6;
pub const EDI: V = 7;
/// Lazy flag state (see [`crate::abi::flags`]).
pub const FK: V = 8;
pub const FR: V = 9;
pub const FA: V = 10;
pub const FB: V = 11;
pub const FC: V = 12;
/// Direction flag, 0 or 1.
pub const DF: V = 13;
pub const FS_BASE: V = 14;
pub const GS_BASE: V = 15;
/// x87 top-of-stack index.
pub const FPU_TOP: V = 16;
/// x87 control word.
pub const FPU_CW: V = 17;
/// x87 status word (excluding top).
pub const FPU_SW: V = 18;
/// SSE control/status.
pub const MXCSR: V = 19;
/// xmm0..xmm7.
pub const XMM0: V = 20;
/// The CPU struct pointer (the function's parameter). Never written.
pub const CPU: V = 31;
/// mm0..mm7 (MMX), as i64.
pub const MM0: V = 32;
pub const NUM_STATE: u32 = 48;

pub const FLAG_STATE: [V; 5] = [FK, FR, FA, FB, FC];
pub const GPRS: [V; 8] = [EAX, ECX, EDX, EBX, ESP, EBP, ESI, EDI];

/// Home location of a state vreg in the CPU struct: (offset, type).
pub fn state_home(v: V) -> Option<(u32, Ty)> {
    Some(match v {
        0..=7 => (cpu::gpr(v), Ty::I32),
        FK => (cpu::FK, Ty::I32),
        FR => (cpu::FR, Ty::I32),
        FA => (cpu::FA, Ty::I32),
        FB => (cpu::FB, Ty::I32),
        FC => (cpu::FC, Ty::I32),
        DF => (cpu::DF, Ty::I32),
        FS_BASE => (cpu::FS_BASE, Ty::I32),
        GS_BASE => (cpu::GS_BASE, Ty::I32),
        FPU_TOP => (cpu::FPU_TOP, Ty::I32),
        FPU_CW => (cpu::FPU_CW, Ty::I32),
        FPU_SW => (cpu::FPU_SW, Ty::I32),
        MXCSR => (cpu::MXCSR, Ty::I32),
        XMM0..=27 => (cpu::XMM + (v - XMM0) * 16, Ty::V128),
        MM0..=39 => (cpu::MMX + (v - MM0) * 8, Ty::I64),
        _ => return None,
    })
}

/// Size in bytes of the home of a state vreg when it is narrower than its
/// type (the x87 control and status words are 16-bit).
pub fn state_home_size(v: V) -> u32 {
    match v {
        FPU_CW | FPU_SW => 2,
        _ => match state_home(v) {
            Some((_, Ty::V128)) => 16,
            Some((_, Ty::I64)) => 8,
            _ => 4,
        },
    }
}

pub fn state_name(v: V) -> Option<&'static str> {
    const NAMES: [&str; 32] = [
        "eax", "ecx", "edx", "ebx", "esp", "ebp", "esi", "edi", "fk", "fr", "fa", "fb", "fc", "df",
        "fsbase", "gsbase", "ftop", "fcw", "fsw", "mxcsr", "xmm0", "xmm1", "xmm2", "xmm3", "xmm4",
        "xmm5", "xmm6", "xmm7", "s28", "s29", "s30", "cpu",
    ];
    NAMES.get(v as usize).copied()
}

// ---- Conditions ----------------------------------------------------------

/// x86 condition codes in encoding order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Cc {
    O = 0,
    NO,
    B,
    AE,
    E,
    NE,
    BE,
    A,
    S,
    NS,
    P,
    NP,
    L,
    GE,
    LE,
    G,
}

impl Cc {
    pub fn from_u8(v: u8) -> Cc {
        const ALL: [Cc; 16] = [
            Cc::O,
            Cc::NO,
            Cc::B,
            Cc::AE,
            Cc::E,
            Cc::NE,
            Cc::BE,
            Cc::A,
            Cc::S,
            Cc::NS,
            Cc::P,
            Cc::NP,
            Cc::L,
            Cc::GE,
            Cc::LE,
            Cc::G,
        ];
        ALL[(v & 15) as usize]
    }
    pub fn negate(self) -> Cc {
        Cc::from_u8(self as u8 ^ 1)
    }
    /// True for the odd (negated) form of a condition pair.
    pub fn is_negated(self) -> bool {
        self as u8 & 1 != 0
    }
    pub fn name(self) -> &'static str {
        [
            "o", "no", "b", "ae", "e", "ne", "be", "a", "s", "ns", "p", "np", "l", "ge", "le", "g",
        ][self as usize]
    }
}

// ---- Operations ----------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinOp {
    I32Add,
    I32Sub,
    I32Mul,
    I32DivS,
    I32DivU,
    I32RemS,
    I32RemU,
    I32And,
    I32Or,
    I32Xor,
    I32Shl,
    I32ShrS,
    I32ShrU,
    I32Rotl,
    I32Rotr,
    I32Eq,
    I32Ne,
    I32LtS,
    I32LtU,
    I32GtS,
    I32GtU,
    I32LeS,
    I32LeU,
    I32GeS,
    I32GeU,
    I64Add,
    I64Sub,
    I64Mul,
    I64DivS,
    I64DivU,
    I64RemS,
    I64RemU,
    I64And,
    I64Or,
    I64Xor,
    I64Shl,
    I64ShrS,
    I64ShrU,
    I64Eq,
    I64Ne,
    I64LtS,
    I64LtU,
    I64GtS,
    I64GtU,
    F64Add,
    F64Sub,
    F64Mul,
    F64Div,
    F64Min,
    F64Max,
    F64Copysign,
    F64Eq,
    F64Ne,
    F64Lt,
    F64Gt,
    F64Le,
    F64Ge,
    F32Add,
    F32Sub,
    F32Mul,
    F32Div,
    F32Min,
    F32Max,
    F32Eq,
    F32Lt,
    F32Le,
}

impl BinOp {
    pub fn result_ty(self) -> Ty {
        use BinOp::*;
        match self {
            I64Add | I64Sub | I64Mul | I64DivS | I64DivU | I64RemS | I64RemU | I64And | I64Or
            | I64Xor | I64Shl | I64ShrS | I64ShrU => Ty::I64,
            F64Add | F64Sub | F64Mul | F64Div | F64Min | F64Max | F64Copysign => Ty::F64,
            F32Add | F32Sub | F32Mul | F32Div | F32Min | F32Max => Ty::F32,
            _ => Ty::I32,
        }
    }
    pub fn commutative(self) -> bool {
        use BinOp::*;
        matches!(
            self,
            I32Add
                | I32Mul
                | I32And
                | I32Or
                | I32Xor
                | I32Eq
                | I32Ne
                | I64Add
                | I64Mul
                | I64And
                | I64Or
                | I64Xor
                | I64Eq
                | I64Ne
        )
    }
    /// Operations that trap in WebAssembly for some inputs.
    pub fn can_trap(self) -> bool {
        use BinOp::*;
        matches!(
            self,
            I32DivS | I32DivU | I32RemS | I32RemU | I64DivS | I64DivU | I64RemS | I64RemU
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnOp {
    I32Eqz,
    I32Clz,
    I32Ctz,
    I32Popcnt,
    I32Extend8S,
    I32Extend16S,
    I64Eqz,
    I64ExtendI32S,
    I64ExtendI32U,
    I32WrapI64,
    F64Neg,
    F64Abs,
    F64Sqrt,
    F64Ceil,
    F64Floor,
    F64Trunc,
    F64Nearest,
    F64ConvertI32S,
    F64ConvertI64S,
    F64PromoteF32,
    F32DemoteF64,
    F32ConvertI32S,
    /// Saturating truncations; callers check range first for x86 semantics.
    I32TruncSatF64S,
    I64TruncSatF64S,
    I32TruncSatF32S,
    F64ReinterpretI64,
    I64ReinterpretF64,
    F32ReinterpretI32,
    I32ReinterpretF32,
}

impl UnOp {
    pub fn result_ty(self) -> Ty {
        use UnOp::*;
        match self {
            I64ExtendI32S | I64ExtendI32U | I64TruncSatF64S | I64ReinterpretF64 => Ty::I64,
            F64Neg | F64Abs | F64Sqrt | F64Ceil | F64Floor | F64Trunc | F64Nearest
            | F64ConvertI32S | F64ConvertI64S | F64PromoteF32 | F64ReinterpretI64 => Ty::F64,
            F32DemoteF64 | F32ConvertI32S | F32ReinterpretI32 => Ty::F32,
            _ => Ty::I32,
        }
    }
}

/// Where a memory access goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Space {
    /// The guest (Windows process) region.
    Guest,
    /// Guest memory the translator trusts: the stack and constant addresses
    /// in data sections. No null checks and no code-write checks.
    Trusted,
    /// The native region (CPU struct and runtime tables). Never checked.
    Native,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Mem {
    /// Access size in bytes: 1, 2, 4, 8 or 16.
    pub size: u8,
    /// Sign-extend narrow loads.
    pub signed: bool,
    pub space: Space,
    /// Constant offset added to the address (no wrap-around).
    pub offset: u32,
    /// Atomic (sequentially consistent) access, for `lock`/`xchg`.
    pub atomic: bool,
}

impl Mem {
    pub fn guest(size: u8) -> Mem {
        Mem {
            size,
            signed: false,
            space: Space::Guest,
            offset: 0,
            atomic: false,
        }
    }
    pub fn native(size: u8, offset: u32) -> Mem {
        Mem {
            size,
            signed: false,
            space: Space::Native,
            offset,
            atomic: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RmwOp {
    Add,
    Sub,
    And,
    Or,
    Xor,
    Xchg,
}

/// Functions emitted into every module that uses them (see
/// `codegen::gen_helper`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Helper {
    /// (fk, fr, fa, fb, fc) -> arithmetic eflags
    Eflags,
    /// (cc, fk, fr, fa, fb, fc) -> 0/1
    EvalCond,
    /// (f64) -> low 8 bytes (significand) of the 80-bit encoding
    F64ToF80Lo,
    /// (f64) -> sign and exponent word of the 80-bit encoding
    F64ToF80Hi,
    /// (significand: i64, sign/exponent: i32) -> f64
    F80ToF64,
}

impl Helper {
    pub fn signature(self) -> (&'static [Ty], Ty) {
        use Helper::*;
        use Ty::*;
        match self {
            Eflags => (&[I32, I32, I32, I32, I32], I32),
            EvalCond => (&[I32, I32, I32, I32, I32, I32], I32),
            F64ToF80Lo => (&[F64], I64),
            F64ToF80Hi => (&[F64], I32),
            F80ToF64 => (&[I64, I32], F64),
        }
    }
}

/// Floating-point operations with no WebAssembly instruction, provided by
/// the host as `env.math(op, a, b) -> f64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum MathOp {
    Sin = 0,
    Cos = 1,
    Tan = 2,
    /// atan2(a, b)
    Atan2 = 3,
    Log2 = 4,
    /// 2^a - 1
    Exp2m1 = 5,
    /// Truncating remainder (C fmod).
    Fmod = 6,
    /// IEEE remainder (round-to-nearest quotient).
    Remainder = 7,
    /// a * 2^trunc(b)
    Scale = 8,
    /// log2(1 + a)
    Log2p1 = 9,
}

/// Lane shapes for 128-bit vector operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lane {
    I8,
    I16,
    I32,
    I64,
    F32,
    F64,
}

impl Lane {
    /// The scalar type of one lane.
    pub fn scalar_ty(self) -> Ty {
        match self {
            Lane::I8 | Lane::I16 | Lane::I32 => Ty::I32,
            Lane::I64 => Ty::I64,
            Lane::F32 => Ty::F32,
            Lane::F64 => Ty::F64,
        }
    }
}

/// Binary 128-bit vector operations, named after the WebAssembly SIMD
/// instructions they become.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VBin {
    I8x16Add,
    I8x16Sub,
    I8x16AddSatS,
    I8x16AddSatU,
    I8x16SubSatS,
    I8x16SubSatU,
    I8x16Eq,
    I8x16GtS,
    I8x16MinU,
    I8x16MaxU,
    I8x16AvgrU,
    I8x16NarrowI16x8S,
    I8x16NarrowI16x8U,
    I16x8Add,
    I16x8Sub,
    I16x8AddSatS,
    I16x8AddSatU,
    I16x8SubSatS,
    I16x8SubSatU,
    I16x8Mul,
    I16x8Eq,
    I16x8GtS,
    I16x8MinS,
    I16x8MaxS,
    I16x8AvgrU,
    I16x8NarrowI32x4S,
    I16x8NarrowI32x4U,
    I32x4Add,
    I32x4Sub,
    I32x4Mul,
    I32x4Eq,
    I32x4GtS,
    I32x4DotI16x8S,
    I32x4ExtMulLowI16x8S,
    I32x4ExtMulHighI16x8S,
    I32x4ExtMulLowI16x8U,
    I32x4ExtMulHighI16x8U,
    I64x2Add,
    I64x2Sub,
    I64x2Eq,
    I64x2ExtMulLowI32x4U,
    F32x4Add,
    F32x4Sub,
    F32x4Mul,
    F32x4Div,
    F32x4Eq,
    F32x4Ne,
    F32x4Lt,
    F32x4Le,
    F32x4Pmin,
    F32x4Pmax,
    F64x2Add,
    F64x2Sub,
    F64x2Mul,
    F64x2Div,
    F64x2Eq,
    F64x2Ne,
    F64x2Lt,
    F64x2Le,
    F64x2Pmin,
    F64x2Pmax,
    V128And,
    V128Or,
    V128Xor,
    /// a & !b
    V128AndNot,
}

/// Unary 128-bit vector operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VUn {
    V128Not,
    F32x4Sqrt,
    F64x2Sqrt,
    F32x4Nearest,
    F32x4Floor,
    F32x4Ceil,
    F32x4Trunc,
    F64x2Nearest,
    F64x2Floor,
    F64x2Ceil,
    F64x2Trunc,
    F32x4ConvertI32x4S,
    I32x4TruncSatF32x4S,
    F64x2ConvertLowI32x4S,
    I32x4TruncSatF64x2SZero,
    F32x4DemoteF64x2Zero,
    F64x2PromoteLowF32x4,
    I16x8ExtAddPairwiseI8x16U,
    I32x4ExtAddPairwiseI16x8U,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VecOp {
    Bin(VBin),
    Un(VUn),
    /// (v128, i32 count): the count is taken modulo the lane width.
    Shl(Lane),
    ShrS(Lane),
    ShrU(Lane),
    /// Lanes 0-15 pick from the first operand, 16-31 from the second.
    Shuffle([u8; 16]),
    Splat(Lane),
    /// Small integer lanes are zero-extended.
    Extract(Lane, u8),
    Replace(Lane, u8),
    Bitmask(Lane),
    Zero,
}

impl VecOp {
    pub fn result_ty(&self) -> Ty {
        match self {
            VecOp::Extract(l, _) => l.scalar_ty(),
            VecOp::Bitmask(_) => Ty::I32,
            _ => Ty::V128,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// Constant; the bits are interpreted according to the destination type.
    Const(u64),
    Copy(V),
    Bin(BinOp, V, V),
    Un(UnOp, V),
    /// cond != 0 ? t : f
    Select {
        cond: V,
        t: V,
        f: V,
    },
    Load {
        addr: V,
        mem: Mem,
    },
    Store {
        addr: V,
        val: V,
        mem: Mem,
    },
    /// Atomic read-modify-write; produces the old value.
    AtomicRmw {
        op: RmwOp,
        addr: V,
        val: V,
        mem: Mem,
    },
    /// Atomic compare-exchange; produces the old value.
    AtomicCmpxchg {
        addr: V,
        expected: V,
        new: V,
        mem: Mem,
    },
    /// memmove(dst, src, len) in guest memory.
    MemCopy {
        dst: V,
        src: V,
        len: V,
    },
    /// memset(dst, val, len) in guest memory.
    MemFill {
        dst: V,
        val: V,
        len: V,
    },
    /// Reads a condition from the current flag state. Lowered away by the
    /// flag pass before optimization.
    Cond(Cc),
    /// Reads the arithmetic flags (CF PF AF ZF SF OF) as an eflags value.
    /// Lowered away by the flag pass.
    Eflags,
    CallHelper(Helper, Vec<V>),
    /// Host math function on f64 values.
    Math {
        op: MathOp,
        a: V,
        b: V,
    },
    /// A 128-bit vector operation.
    Vec(VecOp, Vec<V>),
    /// Raises a fault when `cond` is non-zero, after writing back registers.
    FaultIf {
        cond: V,
        code: u32,
        info: V,
    },
}

impl Op {
    /// Registers read by the operation.
    pub fn uses(&self) -> Vec<V> {
        match self {
            Op::Const(_) | Op::Cond(_) | Op::Eflags => vec![],
            Op::Copy(a) | Op::Un(_, a) => vec![*a],
            Op::Bin(_, a, b) => vec![*a, *b],
            Op::Select { cond, t, f } => vec![*cond, *t, *f],
            Op::Load { addr, .. } => vec![*addr],
            Op::Store { addr, val, .. } => vec![*addr, *val],
            Op::AtomicRmw { addr, val, .. } => vec![*addr, *val],
            Op::AtomicCmpxchg {
                addr,
                expected,
                new,
                ..
            } => vec![*addr, *expected, *new],
            Op::MemCopy { dst, src, len } => vec![*dst, *src, *len],
            Op::MemFill { dst, val, len } => vec![*dst, *val, *len],
            Op::CallHelper(_, args) => args.clone(),
            Op::Math { a, b, .. } => vec![*a, *b],
            Op::Vec(_, args) => args.clone(),
            Op::FaultIf { cond, info, .. } => vec![*cond, *info],
        }
    }

    pub fn uses_mut(&mut self) -> Vec<&mut V> {
        match self {
            Op::Const(_) | Op::Cond(_) | Op::Eflags => vec![],
            Op::Copy(a) | Op::Un(_, a) => vec![a],
            Op::Bin(_, a, b) => vec![a, b],
            Op::Select { cond, t, f } => vec![cond, t, f],
            Op::Load { addr, .. } => vec![addr],
            Op::Store { addr, val, .. } => vec![addr, val],
            Op::AtomicRmw { addr, val, .. } => vec![addr, val],
            Op::AtomicCmpxchg {
                addr,
                expected,
                new,
                ..
            } => vec![addr, expected, new],
            Op::MemCopy { dst, src, len } => vec![dst, src, len],
            Op::MemFill { dst, val, len } => vec![dst, val, len],
            Op::CallHelper(_, args) => args.iter_mut().collect(),
            Op::Math { a, b, .. } => vec![a, b],
            Op::Vec(_, args) => args.iter_mut().collect(),
            Op::FaultIf { cond, info, .. } => vec![cond, info],
        }
    }

    /// Operations that must be kept even when their result is unused.
    pub fn has_side_effects(&self) -> bool {
        match self {
            Op::Store { .. }
            | Op::AtomicRmw { .. }
            | Op::AtomicCmpxchg { .. }
            | Op::MemCopy { .. }
            | Op::MemFill { .. }
            | Op::FaultIf { .. } => true,
            // Checked guest loads can fault; atomic loads order memory.
            Op::Load { mem, .. } => mem.atomic || mem.space == Space::Guest,
            Op::Bin(op, _, _) => op.can_trap(),
            _ => false,
        }
    }

    /// Operations that may raise a guest fault.
    pub fn may_fault(&self) -> bool {
        match self {
            Op::FaultIf { .. } => true,
            Op::Load { mem, .. }
            | Op::Store { mem, .. }
            | Op::AtomicRmw { mem, .. }
            | Op::AtomicCmpxchg { mem, .. } => mem.space == Space::Guest,
            Op::MemCopy { .. } | Op::MemFill { .. } => true,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Inst {
    pub dst: Option<V>,
    pub op: Op,
    /// Address of the x86 instruction this came from.
    pub eip: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CallTarget {
    Direct(u32),
    Indirect(V),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Term {
    Jump(BlockId),
    Branch {
        cond: V,
        t: BlockId,
        f: BlockId,
    },
    /// Jump table: `index` selects among `targets`; out-of-range indexes
    /// leave the function through the dispatcher with `fallback` as the
    /// next address.
    Switch {
        index: V,
        targets: Vec<BlockId>,
        fallback: V,
    },
    /// x86 `call`: the return address has already been pushed. Continues at
    /// `cont` when the callee returns to `ret`, otherwise leaves the function.
    Call {
        target: CallTarget,
        ret: u32,
        cont: BlockId,
    },
    /// Leave the function, continuing at a known address.
    Exit(u32),
    /// x86 `ret`: leave the function, returning the popped address to the
    /// caller, which continues inline when it matches the expected address.
    Ret(V),
    /// Indirect `jmp`: leave the function, continuing at a computed address.
    JmpInd(V),
    /// Raise a fault at `eip`.
    Fault {
        code: u32,
        eip: u32,
    },
    /// Placeholder during construction.
    None,
}

impl Term {
    pub fn successors(&self) -> Vec<BlockId> {
        match self {
            Term::Jump(b) => vec![*b],
            Term::Branch { t, f, .. } => {
                if t == f {
                    vec![*t]
                } else {
                    vec![*t, *f]
                }
            }
            Term::Switch { targets, .. } => {
                let mut v = targets.clone();
                v.sort_unstable();
                v.dedup();
                v
            }
            Term::Call { cont, .. } => vec![*cont],
            _ => vec![],
        }
    }

    pub fn successors_mut(&mut self) -> Vec<&mut BlockId> {
        match self {
            Term::Jump(b) => vec![b],
            Term::Branch { t, f, .. } => vec![t, f],
            Term::Switch { targets, .. } => targets.iter_mut().collect(),
            Term::Call { cont, .. } => vec![cont],
            _ => vec![],
        }
    }

    pub fn uses(&self) -> Vec<V> {
        match self {
            Term::Branch { cond, .. } => vec![*cond],
            Term::Switch {
                index, fallback, ..
            } => vec![*index, *fallback],
            Term::Call {
                target: CallTarget::Indirect(v),
                ..
            } => vec![*v],
            Term::Ret(v) | Term::JmpInd(v) => vec![*v],
            _ => vec![],
        }
    }

    pub fn uses_mut(&mut self) -> Vec<&mut V> {
        match self {
            Term::Branch { cond, .. } => vec![cond],
            Term::Switch {
                index, fallback, ..
            } => vec![index, fallback],
            Term::Call {
                target: CallTarget::Indirect(v),
                ..
            } => vec![v],
            Term::Ret(v) | Term::JmpInd(v) => vec![v],
            _ => vec![],
        }
    }

    /// Terminators where state vregs are written back to the CPU struct (on
    /// at least one path out of the block).
    pub fn syncs(&self) -> bool {
        matches!(
            self,
            Term::Call { .. }
                | Term::Exit(_)
                | Term::Ret(_)
                | Term::JmpInd(_)
                | Term::Fault { .. }
                | Term::Switch { .. }
        )
    }

    /// Terminators after which every state vreg must be reloaded.
    pub fn clobbers_state(&self) -> bool {
        matches!(self, Term::Call { .. })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    /// x86 address this block starts at (for diagnostics and maps).
    pub addr: u32,
    pub insts: Vec<Inst>,
    pub term: Term,
}

#[derive(Debug, Clone)]
pub struct Function {
    /// x86 address of the entry point.
    pub entry: u32,
    /// Block 0 is the entry block.
    pub blocks: Vec<Block>,
    /// Type of every vreg, including state vregs.
    pub vtypes: Vec<Ty>,
    /// What the code may assume about state across calls and returns.
    pub abi: CallAbi,
}

/// What a function may assume about the machine state across its calls and
/// returns. The default assumes nothing: all state is written back at both.
/// Compiled C never reads the arithmetic flags across a call or return and
/// its callees preserve ebx, esi, edi and ebp; not all hand-written
/// assembly does (Delphi's runtime returns comparison results in the
/// flags, MSVC's `_aulldvrm` returns in ebx:ecx), so these are only assumed
/// for code known or shown to rely on them nowhere.
#[derive(Debug, Clone, Default)]
pub struct CallAbi {
    /// No caller reads the flags after this function returns: they are not
    /// written back at returns.
    pub flags_dead_at_ret: bool,
    /// Which callees do not read the flags on entry: the flags are not
    /// written back before calls to them.
    pub flag_free_callees: FlagFreeCallees,
    /// Callees preserve ebx, esi, edi and ebp, which keep their values in
    /// locals across a call instead of being reloaded. (They are still
    /// written back before it, for code that captures them: setjmp,
    /// exception dispatch.)
    pub callee_saved: bool,
}

#[derive(Debug, Clone, Default)]
pub enum FlagFreeCallees {
    /// None known: always write the flags back.
    #[default]
    None,
    /// Every callee.
    All,
    /// Direct calls to the functions listed, and indirect calls except
    /// through the import slots listed.
    Known(std::sync::Arc<FlagFree>),
}

#[derive(Debug, Clone, Default)]
pub struct FlagFree {
    /// Function entries that do not read the flags.
    pub entries: std::collections::HashSet<u32>,
    /// Import address table slots of functions that do (`call [slot]`).
    pub reading_slots: std::collections::HashSet<u32>,
}

impl CallAbi {
    /// The C calling convention, for code known to be compiled C.
    pub fn c() -> CallAbi {
        CallAbi {
            flags_dead_at_ret: true,
            flag_free_callees: FlagFreeCallees::All,
            callee_saved: true,
        }
    }

    /// Whether a direct call to `target` may read the flags on entry.
    pub fn direct_callee_reads_flags(&self, target: u32) -> bool {
        match &self.flag_free_callees {
            FlagFreeCallees::None => true,
            FlagFreeCallees::All => false,
            FlagFreeCallees::Known(k) => !k.entries.contains(&target),
        }
    }

    /// Whether an indirect call may read the flags on entry, given the
    /// import slot it calls through when known.
    pub fn indirect_callee_reads_flags(&self, slot: Option<u32>) -> bool {
        match &self.flag_free_callees {
            FlagFreeCallees::None => true,
            FlagFreeCallees::All => false,
            FlagFreeCallees::Known(k) => slot.is_some_and(|s| k.reading_slots.contains(&s)),
        }
    }
}

impl Function {
    pub fn new(entry: u32) -> Function {
        let mut vtypes = vec![Ty::I32; NUM_STATE as usize];
        for i in 0..8 {
            vtypes[(XMM0 + i) as usize] = Ty::V128;
            vtypes[(MM0 + i) as usize] = Ty::I64;
        }
        Function {
            entry,
            blocks: vec![],
            vtypes,
            abi: CallAbi::default(),
        }
    }

    pub fn new_vreg(&mut self, ty: Ty) -> V {
        self.vtypes.push(ty);
        self.vtypes.len() as V - 1
    }

    pub fn new_block(&mut self, addr: u32) -> BlockId {
        self.blocks.push(Block {
            addr,
            insts: vec![],
            term: Term::None,
        });
        self.blocks.len() as BlockId - 1
    }

    pub fn ty(&self, v: V) -> Ty {
        self.vtypes[v as usize]
    }

    pub fn predecessors(&self) -> Vec<Vec<BlockId>> {
        let mut preds = vec![vec![]; self.blocks.len()];
        for (i, b) in self.blocks.iter().enumerate() {
            for s in b.term.successors() {
                preds[s as usize].push(i as BlockId);
            }
        }
        preds
    }

    /// Blocks reachable from the entry in reverse post-order.
    pub fn rpo(&self) -> Vec<BlockId> {
        let n = self.blocks.len();
        let mut visited = vec![false; n];
        let mut post = Vec::with_capacity(n);
        // Iterative DFS keeping successor order stable.
        let mut stack: Vec<(BlockId, usize)> = vec![(0, 0)];
        visited[0] = true;
        while let Some((b, i)) = stack.pop() {
            let succs = self.blocks[b as usize].term.successors();
            if i < succs.len() {
                stack.push((b, i + 1));
                let s = succs[i];
                if !visited[s as usize] {
                    visited[s as usize] = true;
                    stack.push((s, 0));
                }
            } else {
                post.push(b);
            }
        }
        post.reverse();
        post
    }

    /// Removes blocks unreachable from the entry, renumbering the rest.
    pub fn remove_unreachable(&mut self) {
        let order = self.rpo();
        if order.len() == self.blocks.len() {
            return;
        }
        let mut remap = vec![u32::MAX; self.blocks.len()];
        let mut keep: Vec<BlockId> = order.clone();
        keep.sort_unstable();
        for (new, &old) in keep.iter().enumerate() {
            remap[old as usize] = new as u32;
        }
        let old_blocks = std::mem::take(&mut self.blocks);
        for (i, mut b) in old_blocks.into_iter().enumerate() {
            if remap[i] == u32::MAX {
                continue;
            }
            for s in b.term.successors_mut() {
                *s = remap[*s as usize];
            }
            self.blocks.push(b);
        }
    }
}

// ---- Printing ------------------------------------------------------------

pub struct VName(pub V);

impl fmt::Display for VName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match state_name(self.0) {
            Some(n) if self.0 < NUM_STATE => write!(f, "{n}"),
            _ => write!(f, "v{}", self.0),
        }
    }
}

fn fmt_mem(m: &Mem) -> String {
    let space = match m.space {
        Space::Guest => "",
        Space::Trusted => ".trusted",
        Space::Native => ".native",
    };
    let sign = if m.signed { "s" } else { "" };
    let atomic = if m.atomic { ".atomic" } else { "" };
    let off = if m.offset != 0 {
        format!("+{:#x}", m.offset)
    } else {
        String::new()
    };
    format!("{}{sign}{space}{atomic}{off}", m.size)
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Op::Const(c) => write!(f, "const {c:#x}"),
            Op::Copy(a) => write!(f, "{}", VName(*a)),
            Op::Bin(op, a, b) => write!(f, "{op:?} {}, {}", VName(*a), VName(*b)),
            Op::Un(op, a) => write!(f, "{op:?} {}", VName(*a)),
            Op::Select { cond, t, f: fv } => write!(
                f,
                "select {} ? {} : {}",
                VName(*cond),
                VName(*t),
                VName(*fv)
            ),
            Op::Load { addr, mem } => write!(f, "load{} [{}]", fmt_mem(mem), VName(*addr)),
            Op::Store { addr, val, mem } => write!(
                f,
                "store{} [{}], {}",
                fmt_mem(mem),
                VName(*addr),
                VName(*val)
            ),
            Op::AtomicRmw { op, addr, val, mem } => write!(
                f,
                "rmw.{op:?}{} [{}], {}",
                fmt_mem(mem),
                VName(*addr),
                VName(*val)
            ),
            Op::AtomicCmpxchg {
                addr,
                expected,
                new,
                mem,
            } => write!(
                f,
                "cmpxchg{} [{}], {}, {}",
                fmt_mem(mem),
                VName(*addr),
                VName(*expected),
                VName(*new)
            ),
            Op::MemCopy { dst, src, len } => write!(
                f,
                "memcopy {}, {}, {}",
                VName(*dst),
                VName(*src),
                VName(*len)
            ),
            Op::MemFill { dst, val, len } => write!(
                f,
                "memfill {}, {}, {}",
                VName(*dst),
                VName(*val),
                VName(*len)
            ),
            Op::Cond(cc) => write!(f, "cond.{}", cc.name()),
            Op::Eflags => write!(f, "eflags"),
            Op::CallHelper(h, args) => {
                write!(f, "helper.{h:?}(")?;
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", VName(*a))?;
                }
                write!(f, ")")
            }
            Op::Math { op, a, b } => write!(f, "math.{op:?}({}, {})", VName(*a), VName(*b)),
            Op::Vec(op, args) => {
                write!(f, "vec.{op:?}(")?;
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", VName(*a))?;
                }
                write!(f, ")")
            }
            Op::FaultIf { cond, code, info } => {
                write!(
                    f,
                    "fault_if {} code={code:#x} info={}",
                    VName(*cond),
                    VName(*info)
                )
            }
        }
    }
}

impl fmt::Display for Function {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "function {:#x}:", self.entry)?;
        for (i, b) in self.blocks.iter().enumerate() {
            writeln!(f, "  b{i} @{:#x}:", b.addr)?;
            for inst in &b.insts {
                match inst.dst {
                    Some(d) => writeln!(f, "    {} = {}", VName(d), inst.op)?,
                    None => writeln!(f, "    {}", inst.op)?,
                }
            }
            let t = match &b.term {
                Term::Jump(t) => format!("jump b{t}"),
                Term::Branch { cond, t, f } => format!("br {} ? b{t} : b{f}", VName(*cond)),
                Term::Switch {
                    index,
                    targets,
                    fallback,
                } => format!(
                    "switch {} [{}] else exit {}",
                    VName(*index),
                    targets
                        .iter()
                        .map(|t| format!("b{t}"))
                        .collect::<Vec<_>>()
                        .join(", "),
                    VName(*fallback)
                ),
                Term::Call { target, ret, cont } => match target {
                    CallTarget::Direct(a) => format!("call {a:#x} ret={ret:#x} -> b{cont}"),
                    CallTarget::Indirect(v) => {
                        format!("call [{}] ret={ret:#x} -> b{cont}", VName(*v))
                    }
                },
                Term::Exit(a) => format!("exit {a:#x}"),
                Term::Ret(v) => format!("ret {}", VName(*v)),
                Term::JmpInd(v) => format!("jmp {}", VName(*v)),
                Term::Fault { code, eip } => format!("fault {code:#x} at {eip:#x}"),
                Term::None => "<none>".into(),
            };
            writeln!(f, "    {t}")?;
        }
        Ok(())
    }
}
