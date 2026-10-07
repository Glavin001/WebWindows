//! x87 floating point.
//!
//! Registers are kept as f64 in the CPU struct (physical order) and indexed
//! through the stack top, which lives in a local like the general registers.
//! The control word's rounding mode is honored by integer stores and
//! `frndint`; precision control is not (results are always f64), matching
//! the plan's default. 80-bit loads and stores convert exactly.

use iced_x86::{Instruction, MemorySize, Mnemonic, OpKind, Register};

use super::Lifter;
use crate::abi::{cpu, flags as fl};
use crate::ir::*;

pub fn is_fpu(i: &Instruction) -> bool {
    use iced_x86::CpuidFeature as C;
    let f = i.cpuid_features();
    f.iter()
        .any(|f| matches!(f, C::FPU | C::FPU287 | C::FPU387 | C::FPU287XL_ONLY))
        || matches!(i.mnemonic(), Mnemonic::Wait)
}

// Status word condition bits.
const C0: u32 = 1 << 8;
const C1: u32 = 1 << 9;
const C2: u32 = 1 << 10;
const C3: u32 = 1 << 14;

fn sti(r: Register) -> u32 {
    assert!(
        r >= Register::ST0 && r <= Register::ST7,
        "not an x87 register: {r:?}"
    );
    r as u32 - Register::ST0 as u32
}

impl<'a> Lifter<'a> {
    fn f64c(&mut self, v: f64) -> V {
        self.emit(Ty::F64, Op::Const(v.to_bits()))
    }

    fn fbin(&mut self, op: BinOp, a: V, b: V) -> V {
        self.bin(op, a, b)
    }

    /// Address of physical register for st(i) (relative to the CPU struct
    /// base; the load/store adds the FPU_ST offset).
    pub(super) fn st_addr(&mut self, i: u32) -> V {
        let t = if i == 0 {
            self.copy(FPU_TOP)
        } else {
            let a = self.bini(BinOp::I32Add, FPU_TOP, i);
            self.bini(BinOp::I32And, a, 7)
        };
        let off = self.bini(BinOp::I32Shl, t, 3);
        self.bin(BinOp::I32Add, CPU, off)
    }

    pub(super) fn st(&mut self, i: u32) -> V {
        let a = self.st_addr(i);
        self.emit(
            Ty::F64,
            Op::Load {
                addr: a,
                mem: Mem::native(8, cpu::FPU_ST),
            },
        )
    }

    pub(super) fn set_st(&mut self, i: u32, v: V) {
        let a = self.st_addr(i);
        self.effect(Op::Store {
            addr: a,
            val: v,
            mem: Mem::native(8, cpu::FPU_ST),
        });
    }

    fn tag_update(&mut self, set: bool) {
        let tag = self.load_native(Ty::I32, 1, cpu::FPU_TAG);
        let one = self.c32(1);
        let bit = self.bin(BinOp::I32Shl, one, FPU_TOP);
        let n = if set {
            self.bin(BinOp::I32Or, tag, bit)
        } else {
            let inv = self.bini(BinOp::I32Xor, bit, 0xff);
            self.bin(BinOp::I32And, tag, inv)
        };
        self.store_native(n, 1, cpu::FPU_TAG);
    }

    fn fpush(&mut self, v: V) {
        let t = self.bini(BinOp::I32Sub, FPU_TOP, 1);
        let t = self.bini(BinOp::I32And, t, 7);
        self.emit_to(FPU_TOP, Op::Copy(t));
        self.set_st(0, v);
        self.tag_update(true);
    }

    fn fpop(&mut self) {
        self.tag_update(false);
        let t = self.bini(BinOp::I32Add, FPU_TOP, 1);
        let t = self.bini(BinOp::I32And, t, 7);
        self.emit_to(FPU_TOP, Op::Copy(t));
    }

    /// Clears C1 (most instructions do) and sets the given condition bits.
    fn set_cc(&mut self, bits: V, mask: u32) {
        let k = self.bini(BinOp::I32And, FPU_SW, !mask & 0xffff);
        self.emit_to(FPU_SW, Op::Bin(BinOp::I32Or, k, bits));
    }

    /// Reads a memory operand as f64 (float, double, extended or integer).
    fn fload_mem(&mut self, i: &Instruction) -> Option<V> {
        let (a, space) = self.ea(i);
        let m = |l: &Self, size| l.mem(size, space);
        Some(match i.memory_size() {
            MemorySize::Float32 => {
                let f = self.emit(
                    Ty::F32,
                    Op::Load {
                        addr: a,
                        mem: m(self, 4),
                    },
                );
                self.un(UnOp::F64PromoteF32, f)
            }
            MemorySize::Float64 => self.emit(
                Ty::F64,
                Op::Load {
                    addr: a,
                    mem: m(self, 8),
                },
            ),
            MemorySize::Float80 => {
                let lo = self.emit(
                    Ty::I64,
                    Op::Load {
                        addr: a,
                        mem: m(self, 8),
                    },
                );
                let a8 = self.bini(BinOp::I32Add, a, 8);
                let hi = self.emit(
                    Ty::I32,
                    Op::Load {
                        addr: a8,
                        mem: m(self, 2),
                    },
                );
                self.emit(Ty::F64, Op::CallHelper(Helper::F80ToF64, vec![lo, hi]))
            }
            MemorySize::Int16 => {
                let mut mm = m(self, 2);
                mm.signed = true;
                let v = self.emit(Ty::I32, Op::Load { addr: a, mem: mm });
                self.un(UnOp::F64ConvertI32S, v)
            }
            MemorySize::Int32 => {
                let v = self.emit(
                    Ty::I32,
                    Op::Load {
                        addr: a,
                        mem: m(self, 4),
                    },
                );
                self.un(UnOp::F64ConvertI32S, v)
            }
            MemorySize::Int64 => {
                let v = self.emit(
                    Ty::I64,
                    Op::Load {
                        addr: a,
                        mem: m(self, 8),
                    },
                );
                self.un(UnOp::F64ConvertI64S, v)
            }
            _ => return None,
        })
    }

    /// Source operand `n`: a stack register or memory.
    fn fsrc(&mut self, i: &Instruction, n: u32) -> Option<V> {
        match i.op_kind(n) {
            OpKind::Register => Some(self.st(sti(i.op_register(n)))),
            OpKind::Memory => self.fload_mem(i),
            _ => None,
        }
    }

    /// Rounds according to the control word's rounding mode.
    pub(super) fn fround(&mut self, v: V) -> V {
        let rc = self.bini(BinOp::I32ShrU, FPU_CW, 10);
        let rc = self.bini(BinOp::I32And, rc, 3);
        let near = self.un(UnOp::F64Nearest, v);
        let down = self.un(UnOp::F64Floor, v);
        let up = self.un(UnOp::F64Ceil, v);
        let zero = self.un(UnOp::F64Trunc, v);
        let is1 = self.bini(BinOp::I32Eq, rc, 1);
        let is2 = self.bini(BinOp::I32Eq, rc, 2);
        let is3 = self.bini(BinOp::I32Eq, rc, 3);
        let x = self.select(is3, zero, near);
        let x = self.select(is2, up, x);
        self.select(is1, down, x)
    }

    /// Stores st(0) to memory as float, double, extended or integer.
    fn fstore_mem(&mut self, i: &Instruction, v: V) -> bool {
        let (a, space) = self.ea(i);
        let mem = |l: &Self, size| l.mem(size, space);
        match i.memory_size() {
            MemorySize::Float32 => {
                let f = self.un(UnOp::F32DemoteF64, v);
                let mm = mem(self, 4);
                self.effect(Op::Store {
                    addr: a,
                    val: f,
                    mem: mm,
                });
            }
            MemorySize::Float64 => {
                let mm = mem(self, 8);
                self.effect(Op::Store {
                    addr: a,
                    val: v,
                    mem: mm,
                });
            }
            MemorySize::Float80 => {
                let lo = self.emit(Ty::I64, Op::CallHelper(Helper::F64ToF80Lo, vec![v]));
                let hi = self.emit(Ty::I32, Op::CallHelper(Helper::F64ToF80Hi, vec![v]));
                let mm = mem(self, 8);
                self.effect(Op::Store {
                    addr: a,
                    val: lo,
                    mem: mm,
                });
                let a8 = self.bini(BinOp::I32Add, a, 8);
                let mm = mem(self, 2);
                self.effect(Op::Store {
                    addr: a8,
                    val: hi,
                    mem: mm,
                });
            }
            MemorySize::Int16 | MemorySize::Int32 | MemorySize::Int64 => {
                let size = i.memory_size().size() as u32;
                let r = if i.mnemonic() == Mnemonic::Fisttp {
                    self.un(UnOp::F64Trunc, v)
                } else {
                    self.fround(v)
                };
                // Out-of-range values and NaN store the "integer indefinite".
                let (lo, hi) = match size {
                    2 => (-32768.0, 32767.0),
                    4 => (-2147483648.0, 2147483647.0),
                    _ => (-9223372036854775808.0, 9223372036854775807.0),
                };
                let lo_c = self.f64c(lo);
                let hi_c = self.f64c(hi);
                let ge = self.fbin(BinOp::F64Ge, r, lo_c);
                let le = if size == 8 {
                    // 2^63 is not representable as i64; compare with <.
                    let lim = self.f64c(9223372036854775808.0);
                    self.fbin(BinOp::F64Lt, r, lim)
                } else {
                    self.fbin(BinOp::F64Le, r, hi_c)
                };
                let ok = self.bin(BinOp::I32And, ge, le);
                if size == 8 {
                    let x = self.un(UnOp::I64TruncSatF64S, r);
                    let ind = self.c64(1u64 << 63);
                    let x = self.select(ok, x, ind);
                    let mm = mem(self, 8);
                    self.effect(Op::Store {
                        addr: a,
                        val: x,
                        mem: mm,
                    });
                } else {
                    let x = self.un(UnOp::I32TruncSatF64S, r);
                    let ind = self.c32(if size == 2 { 0x8000 } else { 0x8000_0000 });
                    let x = self.select(ok, x, ind);
                    let mm = mem(self, size);
                    self.effect(Op::Store {
                        addr: a,
                        val: x,
                        mem: mm,
                    });
                }
            }
            _ => return false,
        }
        true
    }

    /// Compares a and b, returning (C0, C2, C3) as 0/1 values.
    fn fcompare(&mut self, a: V, b: V) -> (V, V, V) {
        let lt = self.fbin(BinOp::F64Lt, a, b);
        let gt = self.fbin(BinOp::F64Gt, a, b);
        let eq = self.fbin(BinOp::F64Eq, a, b);
        let o1 = self.bin(BinOp::I32Or, lt, gt);
        let ordered = self.bin(BinOp::I32Or, o1, eq);
        let un = self.is_zero(ordered);
        let c0 = self.bin(BinOp::I32Or, lt, un);
        let c3 = self.bin(BinOp::I32Or, eq, un);
        (c0, un, c3)
    }

    fn set_fcom_cc(&mut self, a: V, b: V) {
        let (c0, c2, c3) = self.fcompare(a, b);
        let b0 = self.bini(BinOp::I32Shl, c0, 8);
        let b2 = self.bini(BinOp::I32Shl, c2, 10);
        let b3 = self.bini(BinOp::I32Shl, c3, 14);
        let t = self.bin(BinOp::I32Or, b0, b2);
        let t = self.bin(BinOp::I32Or, t, b3);
        self.set_cc(t, C0 | C1 | C2 | C3);
    }

    pub(super) fn lift_fpu(&mut self, i: &Instruction) -> bool {
        use Mnemonic as M;
        let m = i.mnemonic();
        macro_rules! bail {
            () => {{
                self.unsupported(i, "x87 form not supported");
                return false;
            }};
        }
        match m {
            M::Wait | M::Fnop => {}
            M::Fninit => {
                self.emit_to(FPU_CW, Op::Const(0x37f));
                self.emit_to(FPU_SW, Op::Const(0));
                self.emit_to(FPU_TOP, Op::Const(0));
                let z = self.c32(0);
                self.store_native(z, 1, cpu::FPU_TAG);
            }
            M::Fnclex => {
                let k = self.bini(BinOp::I32And, FPU_SW, 0x7f00);
                self.emit_to(FPU_SW, Op::Copy(k));
            }
            M::Fldcw => {
                let (a, sp) = self.ea(i);
                let v = self.load(a, 2, sp);
                self.emit_to(FPU_CW, Op::Copy(v));
            }
            M::Fnstcw => {
                let (a, sp) = self.ea(i);
                let v = self.copy(FPU_CW);
                self.store(a, v, 2, sp);
            }
            M::Fnstsw => {
                let top = self.bini(BinOp::I32Shl, FPU_TOP, 11);
                let sw = self.bini(BinOp::I32And, FPU_SW, !0x3800 & 0xffff);
                let v = self.bin(BinOp::I32Or, sw, top);
                if i.op0_kind() == OpKind::Register {
                    self.write_reg(Register::AX, v);
                } else {
                    let (a, sp) = self.ea(i);
                    self.store(a, v, 2, sp);
                }
            }
            M::Fld => {
                let v = match self.fsrc(i, i.op_count() - 1) {
                    Some(v) => v,
                    None => bail!(),
                };
                self.fpush(v);
            }
            M::Fild => {
                let v = match self.fload_mem(i) {
                    Some(v) => v,
                    None => bail!(),
                };
                self.fpush(v);
            }
            M::Fld1 | M::Fldz | M::Fldpi | M::Fldl2e | M::Fldl2t | M::Fldlg2 | M::Fldln2 => {
                let c = match m {
                    M::Fld1 => 1.0,
                    M::Fldz => 0.0,
                    M::Fldpi => std::f64::consts::PI,
                    M::Fldl2e => std::f64::consts::LOG2_E,
                    M::Fldl2t => std::f64::consts::LOG2_10,
                    M::Fldlg2 => std::f64::consts::LOG10_2,
                    _ => std::f64::consts::LN_2,
                };
                let v = self.f64c(c);
                self.fpush(v);
            }
            M::Fst | M::Fstp | M::Fist | M::Fistp | M::Fisttp => {
                let v = self.st(0);
                match i.op0_kind() {
                    OpKind::Register => {
                        let d = sti(i.op0_register());
                        self.set_st(d, v);
                        if d != 0 || m == M::Fstp {
                            // fst st(i) marks the target valid.
                            let tag = self.load_native(Ty::I32, 1, cpu::FPU_TAG);
                            let t = self.bini(BinOp::I32Add, FPU_TOP, d);
                            let t = self.bini(BinOp::I32And, t, 7);
                            let one = self.c32(1);
                            let bit = self.bin(BinOp::I32Shl, one, t);
                            let n = self.bin(BinOp::I32Or, tag, bit);
                            self.store_native(n, 1, cpu::FPU_TAG);
                        }
                    }
                    OpKind::Memory => {
                        if !self.fstore_mem(i, v) {
                            bail!();
                        }
                    }
                    _ => bail!(),
                }
                if matches!(m, M::Fstp | M::Fistp | M::Fisttp) {
                    self.fpop();
                }
            }
            M::Fxch => {
                let j = if i.op_count() == 2 {
                    sti(i.op1_register())
                } else {
                    1
                };
                let a = self.st(0);
                let b = self.st(j);
                self.set_st(0, b);
                self.set_st(j, a);
                // Both registers become valid (exchanging with an empty
                // register would raise a stack fault).
                let z = self.c32(0);
                self.set_cc(z, C1);
            }
            M::Fadd
            | M::Faddp
            | M::Fsub
            | M::Fsubp
            | M::Fsubr
            | M::Fsubrp
            | M::Fmul
            | M::Fmulp
            | M::Fdiv
            | M::Fdivp
            | M::Fdivr
            | M::Fdivrp
            | M::Fiadd
            | M::Fisub
            | M::Fisubr
            | M::Fimul
            | M::Fidiv
            | M::Fidivr => {
                let (dst, a, b) = if i.op_count() == 0 {
                    // faddp with no operands: st(1) = st(1) op st(0)
                    (1, self.st(1), self.st(0))
                } else if i.op_count() == 1 {
                    // Memory source: st(0) = st(0) op m
                    let a = self.st(0);
                    let b = match self.fsrc(i, 0) {
                        Some(b) => b,
                        None => bail!(),
                    };
                    (0, a, b)
                } else {
                    let d = match i.op0_kind() {
                        OpKind::Register => sti(i.op0_register()),
                        _ => bail!(),
                    };
                    let a = self.st(d);
                    let b = match self.fsrc(i, 1) {
                        Some(b) => b,
                        None => bail!(),
                    };
                    (d, a, b)
                };
                let r = match m {
                    M::Fadd | M::Faddp | M::Fiadd => self.fbin(BinOp::F64Add, a, b),
                    M::Fsub | M::Fsubp | M::Fisub => self.fbin(BinOp::F64Sub, a, b),
                    M::Fsubr | M::Fsubrp | M::Fisubr => self.fbin(BinOp::F64Sub, b, a),
                    M::Fmul | M::Fmulp | M::Fimul => self.fbin(BinOp::F64Mul, a, b),
                    M::Fdiv | M::Fdivp | M::Fidiv => self.fbin(BinOp::F64Div, a, b),
                    _ => self.fbin(BinOp::F64Div, b, a),
                };
                self.set_st(dst, r);
                if matches!(
                    m,
                    M::Faddp | M::Fsubp | M::Fsubrp | M::Fmulp | M::Fdivp | M::Fdivrp
                ) {
                    self.fpop();
                }
                let z = self.c32(0);
                self.set_cc(z, C1);
            }
            M::Fchs | M::Fabs | M::Fsqrt | M::Frndint => {
                let v = self.st(0);
                let r = match m {
                    M::Fchs => self.un(UnOp::F64Neg, v),
                    M::Fabs => self.un(UnOp::F64Abs, v),
                    M::Fsqrt => self.un(UnOp::F64Sqrt, v),
                    _ => self.fround(v),
                };
                self.set_st(0, r);
                let z = self.c32(0);
                self.set_cc(z, C1);
            }
            M::Fcom | M::Fcomp | M::Fucom | M::Fucomp | M::Ficom | M::Ficomp => {
                let a = self.st(0);
                let b = if i.op_count() == 0 {
                    self.st(1)
                } else {
                    match self.fsrc(i, i.op_count() - 1) {
                        Some(b) => b,
                        None => bail!(),
                    }
                };
                self.set_fcom_cc(a, b);
                if matches!(m, M::Fcomp | M::Fucomp | M::Ficomp) {
                    self.fpop();
                }
            }
            M::Fcompp | M::Fucompp => {
                let a = self.st(0);
                let b = self.st(1);
                self.set_fcom_cc(a, b);
                self.fpop();
                self.fpop();
            }
            M::Ftst => {
                let a = self.st(0);
                let z = self.f64c(0.0);
                self.set_fcom_cc(a, z);
            }
            M::Fcomi | M::Fcomip | M::Fucomi | M::Fucomip => {
                let a = self.st(0);
                let b = self.st(sti(i.op1_register()));
                let (c0, c2, c3) = self.fcompare(a, b);
                let p = self.bini(BinOp::I32Shl, c2, 2);
                let z = self.bini(BinOp::I32Shl, c3, 6);
                let e = self.bin(BinOp::I32Or, c0, p);
                let e = self.bin(BinOp::I32Or, e, z);
                self.set_flags_explicit(e);
                let zero = self.c32(0);
                self.set_cc(zero, C1);
                if matches!(m, M::Fcomip | M::Fucomip) {
                    self.fpop();
                }
            }
            M::Fcmovb
            | M::Fcmove
            | M::Fcmovbe
            | M::Fcmovu
            | M::Fcmovnb
            | M::Fcmovne
            | M::Fcmovnbe
            | M::Fcmovnu => {
                let cc = match m {
                    M::Fcmovb => Cc::B,
                    M::Fcmove => Cc::E,
                    M::Fcmovbe => Cc::BE,
                    M::Fcmovu => Cc::P,
                    M::Fcmovnb => Cc::AE,
                    M::Fcmovne => Cc::NE,
                    M::Fcmovnbe => Cc::A,
                    _ => Cc::NP,
                };
                let c = self.cond(cc);
                let a = self.st(0);
                let b = self.st(sti(i.op1_register()));
                let v = self.select(c, b, a);
                self.set_st(0, v);
            }
            M::Fxam => {
                let v = self.st(0);
                let bits = self.un(UnOp::I64ReinterpretF64, v);
                let k63 = self.c64(63);
                let sign = self.bin(BinOp::I64ShrU, bits, k63);
                let sign = self.un(UnOp::I32WrapI64, sign);
                let abs = self.un(UnOp::F64Abs, v);
                let inf = self.f64c(f64::INFINITY);
                let is_inf = self.fbin(BinOp::F64Eq, abs, inf);
                let is_nan = self.fbin(BinOp::F64Ne, v, v);
                let zero = self.f64c(0.0);
                let is_zero = self.fbin(BinOp::F64Eq, v, zero);
                // Empty register?
                let tag = self.load_native(Ty::I32, 1, cpu::FPU_TAG);
                let tb = self.bin(BinOp::I32ShrU, tag, FPU_TOP);
                let tb = self.bini(BinOp::I32And, tb, 1);
                let empty = self.is_zero(tb);
                // class: normal C2; inf C2|C0; nan C0; zero C3; empty C3|C0
                let normal = self.c32(C2);
                let infc = self.c32(C2 | C0);
                let nanc = self.c32(C0);
                let zc = self.c32(C3);
                let ec = self.c32(C3 | C0);
                let x = self.select(is_zero, zc, normal);
                let x = self.select(is_inf, infc, x);
                let x = self.select(is_nan, nanc, x);
                let x = self.select(empty, ec, x);
                let s = self.bini(BinOp::I32Shl, sign, 9);
                let x = self.bin(BinOp::I32Or, x, s);
                self.set_cc(x, C0 | C1 | C2 | C3);
            }
            M::Ffree => {
                let j = sti(i.op0_register());
                let tag = self.load_native(Ty::I32, 1, cpu::FPU_TAG);
                let t = self.bini(BinOp::I32Add, FPU_TOP, j);
                let t = self.bini(BinOp::I32And, t, 7);
                let one = self.c32(1);
                let bit = self.bin(BinOp::I32Shl, one, t);
                let inv = self.bini(BinOp::I32Xor, bit, 0xff);
                let n = self.bin(BinOp::I32And, tag, inv);
                self.store_native(n, 1, cpu::FPU_TAG);
            }
            M::Fincstp | M::Fdecstp => {
                let t = if m == M::Fincstp {
                    self.bini(BinOp::I32Add, FPU_TOP, 1)
                } else {
                    self.bini(BinOp::I32Sub, FPU_TOP, 1)
                };
                let t = self.bini(BinOp::I32And, t, 7);
                self.emit_to(FPU_TOP, Op::Copy(t));
                let z = self.c32(0);
                self.set_cc(z, C1);
            }
            M::Fsin | M::Fcos | M::Fptan | M::Fsincos | M::F2xm1 => {
                let v = self.st(0);
                let zero = self.f64c(0.0);
                let op = match m {
                    M::Fsin => MathOp::Sin,
                    M::Fcos => MathOp::Cos,
                    M::Fptan => MathOp::Tan,
                    M::Fsincos => MathOp::Sin,
                    _ => MathOp::Exp2m1,
                };
                let r = self.emit(Ty::F64, Op::Math { op, a: v, b: zero });
                match m {
                    M::Fptan => {
                        self.set_st(0, r);
                        let one = self.f64c(1.0);
                        self.fpush(one);
                    }
                    M::Fsincos => {
                        let c = self.emit(
                            Ty::F64,
                            Op::Math {
                                op: MathOp::Cos,
                                a: v,
                                b: zero,
                            },
                        );
                        self.set_st(0, r);
                        self.fpush(c);
                    }
                    _ => self.set_st(0, r),
                }
                // C2 = 0: the operand was in range.
                let z = self.c32(0);
                self.set_cc(z, C1 | C2);
            }
            M::Fpatan | M::Fyl2x | M::Fyl2xp1 | M::Fscale | M::Fprem | M::Fprem1 => {
                let a = self.st(0);
                let b = self.st(1);
                let zero = self.f64c(0.0);
                match m {
                    M::Fpatan => {
                        let r = self.emit(
                            Ty::F64,
                            Op::Math {
                                op: MathOp::Atan2,
                                a: b,
                                b: a,
                            },
                        );
                        self.set_st(1, r);
                        self.fpop();
                    }
                    M::Fyl2x | M::Fyl2xp1 => {
                        let op = if m == M::Fyl2x {
                            MathOp::Log2
                        } else {
                            MathOp::Log2p1
                        };
                        let l = self.emit(Ty::F64, Op::Math { op, a, b: zero });
                        let r = self.fbin(BinOp::F64Mul, b, l);
                        self.set_st(1, r);
                        self.fpop();
                    }
                    M::Fscale => {
                        let r = self.emit(
                            Ty::F64,
                            Op::Math {
                                op: MathOp::Scale,
                                a,
                                b,
                            },
                        );
                        self.set_st(0, r);
                    }
                    _ => {
                        let op = if m == M::Fprem {
                            MathOp::Fmod
                        } else {
                            MathOp::Remainder
                        };
                        let r = self.emit(Ty::F64, Op::Math { op, a, b });
                        self.set_st(0, r);
                        // Quotient bits: C0 = q2, C3 = q1, C1 = q0; C2 = 0
                        // (reduction complete).
                        // The quotient is exactly (a - r) / b, an integer.
                        let d = self.fbin(BinOp::F64Sub, a, r);
                        let q = self.fbin(BinOp::F64Div, d, b);
                        let q = self.un(UnOp::F64Nearest, q);
                        let q = self.un(UnOp::F64Abs, q);
                        let qi = self.un(UnOp::I64TruncSatF64S, q);
                        let qi = self.un(UnOp::I32WrapI64, qi);
                        let q0 = self.bini(BinOp::I32And, qi, 1);
                        let q1 = self.bini(BinOp::I32ShrU, qi, 1);
                        let q1 = self.bini(BinOp::I32And, q1, 1);
                        let q2 = self.bini(BinOp::I32ShrU, qi, 2);
                        let q2 = self.bini(BinOp::I32And, q2, 1);
                        let b1 = self.bini(BinOp::I32Shl, q0, 9);
                        let b3 = self.bini(BinOp::I32Shl, q1, 14);
                        let b0 = self.bini(BinOp::I32Shl, q2, 8);
                        let t = self.bin(BinOp::I32Or, b1, b3);
                        let t = self.bin(BinOp::I32Or, t, b0);
                        self.set_cc(t, C0 | C1 | C2 | C3);
                    }
                }
            }
            M::Fxtract => {
                let v = self.st(0);
                // exponent = floor(log2|v|) for normal values; significand =
                // v / 2^exponent. Uses the f64 bit pattern.
                let bits = self.un(UnOp::I64ReinterpretF64, v);
                let k52 = self.c64(52);
                let e = self.bin(BinOp::I64ShrU, bits, k52);
                let e = self.un(UnOp::I32WrapI64, e);
                let e = self.bini(BinOp::I32And, e, 0x7ff);
                let e = self.bini(BinOp::I32Sub, e, 1023);
                let ef = self.un(UnOp::F64ConvertI32S, e);
                let mask = self.c64(!(0x7ffu64 << 52));
                let m_ = self.bin(BinOp::I64And, bits, mask);
                let one_exp = self.c64(1023u64 << 52);
                let m_ = self.bin(BinOp::I64Or, m_, one_exp);
                let sig = self.un(UnOp::F64ReinterpretI64, m_);
                self.set_st(0, ef);
                self.fpush(sig);
            }
            M::Fnstenv | M::Fldenv | M::Fnsave | M::Frstor => {
                self.lift_fpu_env(i);
            }
            _ => bail!(),
        }
        let _ = fl::ARITH;
        true
    }

    /// fnstenv/fldenv (and the register-less parts of fnsave/frstor) using
    /// the 32-bit protected-mode layout.
    fn lift_fpu_env(&mut self, i: &Instruction) {
        use Mnemonic as M;
        let (a, sp) = self.ea(i);
        let store = matches!(i.mnemonic(), M::Fnstenv | M::Fnsave);
        let at = |l: &mut Self, off: u32| l.bini(BinOp::I32Add, a, off);
        if store {
            let cw = self.bini(BinOp::I32Or, FPU_CW, 0xffff_0000);
            self.store(a, cw, 4, sp);
            let top = self.bini(BinOp::I32Shl, FPU_TOP, 11);
            let sw = self.bini(BinOp::I32And, FPU_SW, !0x3800 & 0xffff);
            let sw = self.bin(BinOp::I32Or, sw, top);
            let sw = self.bini(BinOp::I32Or, sw, 0xffff_0000);
            let p = at(self, 4);
            self.store(p, sw, 4, sp);
            // Full tag word: 11 (empty) or 00 (valid) per physical register.
            let tag = self.load_native(Ty::I32, 1, cpu::FPU_TAG);
            let mut full = self.c32(0xffff_ffff);
            for r in 0..8u32 {
                let b = self.bini(BinOp::I32ShrU, tag, r);
                let b = self.bini(BinOp::I32And, b, 1);
                let m = self.bini(BinOp::I32Mul, b, 3 << (2 * r));
                let inv = self.bini(BinOp::I32Xor, m, u32::MAX);
                full = self.bin(BinOp::I32And, full, inv);
            }
            let p = at(self, 8);
            self.store(p, full, 4, sp);
            let z = self.c32(0);
            for off in [12, 16, 20, 24] {
                let p = at(self, off);
                self.store(p, z, 4, sp);
            }
            if i.mnemonic() == M::Fnsave {
                for r in 0..8u32 {
                    let v = self.st(r);
                    let lo = self.emit(Ty::I64, Op::CallHelper(Helper::F64ToF80Lo, vec![v]));
                    let hi = self.emit(Ty::I32, Op::CallHelper(Helper::F64ToF80Hi, vec![v]));
                    let p = at(self, 28 + r * 10);
                    let mm = self.mem(8, sp);
                    self.effect(Op::Store {
                        addr: p,
                        val: lo,
                        mem: mm,
                    });
                    let p = at(self, 36 + r * 10);
                    self.store(p, hi, 2, sp);
                }
                // fnsave reinitializes the FPU.
                self.emit_to(FPU_CW, Op::Const(0x37f));
                self.emit_to(FPU_SW, Op::Const(0));
                self.emit_to(FPU_TOP, Op::Const(0));
                let z = self.c32(0);
                self.store_native(z, 1, cpu::FPU_TAG);
            }
        } else {
            let cw = self.load(a, 2, sp);
            self.emit_to(FPU_CW, Op::Copy(cw));
            let p = at(self, 4);
            let sw = self.load(p, 2, sp);
            let top = self.bini(BinOp::I32ShrU, sw, 11);
            let top = self.bini(BinOp::I32And, top, 7);
            let sw = self.bini(BinOp::I32And, sw, !0x3800 & 0xffff);
            self.emit_to(FPU_SW, Op::Copy(sw));
            self.emit_to(FPU_TOP, Op::Copy(top));
            let p = at(self, 8);
            let full = self.load(p, 2, sp);
            let mut tag = self.c32(0);
            for r in 0..8u32 {
                let b = self.bini(BinOp::I32ShrU, full, 2 * r);
                let b = self.bini(BinOp::I32And, b, 3);
                let valid = self.bini(BinOp::I32Ne, b, 3);
                let s = self.bini(BinOp::I32Shl, valid, r);
                tag = self.bin(BinOp::I32Or, tag, s);
            }
            self.store_native(tag, 1, cpu::FPU_TAG);
            if i.mnemonic() == M::Frstor {
                for r in 0..8u32 {
                    let p = at(self, 28 + r * 10);
                    let mm = self.mem(8, sp);
                    let lo = self.emit(Ty::I64, Op::Load { addr: p, mem: mm });
                    let p = at(self, 36 + r * 10);
                    let hi = self.load(p, 2, sp);
                    let v = self.emit(Ty::F64, Op::CallHelper(Helper::F80ToF64, vec![lo, hi]));
                    self.set_st(r, v);
                }
            }
        }
    }
}
