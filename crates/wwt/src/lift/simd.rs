//! MMX, SSE and SSE2 on WebAssembly SIMD.
//!
//! XMM registers are 128-bit state vregs; MMX registers are 64-bit state
//! vregs widened into a vector (lane 0) for each operation. Where x86 and
//! WebAssembly differ, the lifting matches x86: `minps`/`maxps` use
//! `pmin`/`pmax` with swapped operands (x86 returns the second operand for
//! NaN and equal zeros), shifts by counts at least the lane width give zero
//! (or sign fill), out-of-range float-to-integer conversions give the
//! "integer indefinite" value, and scalar operations change only lane 0.

use iced_x86::{Instruction, MemorySize, Mnemonic, OpKind, Register};

use super::Lifter;
use crate::abi::{cpu, fault};
use crate::ir::*;

pub fn is_simd(i: &Instruction) -> bool {
    use iced_x86::CpuidFeature as C;
    i.cpuid_features()
        .iter()
        .any(|f| matches!(f, C::MMX | C::SSE | C::SSE2 | C::FXSR))
}

fn is_xmm(r: Register) -> bool {
    r >= Register::XMM0 && r <= Register::XMM15
}

fn is_mm(r: Register) -> bool {
    r >= Register::MM0 && r <= Register::MM7
}

fn xmm_vreg(r: Register) -> V {
    xmm(r as u32 - Register::XMM0 as u32)
}

fn mm_vreg(r: Register) -> V {
    MM0 + (r as u32 - Register::MM0 as u32)
}

/// Builds a byte shuffle from (operand, byte index) picks.
fn bytes(picks: &[(u8, u8)]) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (k, &(op, b)) in picks.iter().enumerate().take(16) {
        out[k] = op * 16 + b;
    }
    out
}

/// A shuffle selecting whole lanes of `size` bytes: (operand, lane).
fn lanes(size: u8, picks: &[(u8, u8)]) -> [u8; 16] {
    let mut v = vec![];
    for &(op, lane) in picks {
        for b in 0..size {
            v.push((op, lane * size + b));
        }
    }
    bytes(&v)
}

impl<'a> Lifter<'a> {
    fn vop(&mut self, op: VecOp, args: &[V]) -> V {
        let ty = op.result_ty();
        self.emit(ty, Op::Vec(op, args.to_vec()))
    }

    fn vbin(&mut self, op: VBin, a: V, b: V) -> V {
        self.vop(VecOp::Bin(op), &[a, b])
    }

    fn vun(&mut self, op: VUn, a: V) -> V {
        self.vop(VecOp::Un(op), &[a])
    }

    fn vzero(&mut self) -> V {
        self.vop(VecOp::Zero, &[])
    }

    fn shuffle(&mut self, a: V, b: V, mask: [u8; 16]) -> V {
        self.vop(VecOp::Shuffle(mask), &[a, b])
    }

    fn splat_i32(&mut self, v: u32) -> V {
        let c = self.c32(v);
        self.vop(VecOp::Splat(Lane::I32), &[c])
    }

    fn splat_f32(&mut self, v: f32) -> V {
        let c = self.emit(Ty::F32, Op::Const(v.to_bits() as u64));
        self.vop(VecOp::Splat(Lane::F32), &[c])
    }

    fn splat_f64(&mut self, v: f64) -> V {
        let c = self.emit(Ty::F64, Op::Const(v.to_bits()));
        self.vop(VecOp::Splat(Lane::F64), &[c])
    }

    fn vselect(&mut self, cond: V, t: V, f: V) -> V {
        self.emit(Ty::V128, Op::Select { cond, t, f })
    }

    /// (a & mask) | (b & !mask)
    fn vblend(&mut self, mask: V, a: V, b: V) -> V {
        let x = self.vbin(VBin::V128And, a, mask);
        let y = self.vbin(VBin::V128AndNot, b, mask);
        self.vbin(VBin::V128Or, x, y)
    }

    /// Marks the x87 unit as in MMX mode: top 0, all registers valid.
    fn mmx_touch(&mut self) {
        self.emit_to(FPU_TOP, Op::Const(0));
        let t = self.c32(0xff);
        self.store_native(t, 1, cpu::FPU_TAG);
    }

    /// Operand `n` as a vector (MMX values in lane 0). Memory operands are
    /// loaded at their size, zero-extended.
    fn vsrc(&mut self, i: &Instruction, n: u32) -> Option<V> {
        Some(match i.op_kind(n) {
            OpKind::Register => {
                let r = i.op_register(n);
                if is_xmm(r) {
                    self.copy(xmm_vreg(r))
                } else if is_mm(r) {
                    self.vop(VecOp::Splat(Lane::I64), &[mm_vreg(r)])
                } else {
                    // General register: zero-extended into lane 0.
                    let g = self.read_reg(r);
                    let z = self.vzero();
                    let lane = if r.size() == 8 { Lane::I64 } else { Lane::I32 };
                    self.vop(VecOp::Replace(lane, 0), &[z, g])
                }
            }
            OpKind::Memory => {
                let (a, sp) = self.ea(i);
                let size = i.memory_size().size() as u32;
                match size {
                    16 => {
                        let mem = self.mem(16, sp);
                        self.emit(Ty::V128, Op::Load { addr: a, mem })
                    }
                    8 | 4 => {
                        let mem = self.mem(size, sp);
                        self.emit(Ty::V128, Op::Load { addr: a, mem })
                    }
                    2 => {
                        let v = self.load(a, 2, sp);
                        let z = self.vzero();
                        self.vop(VecOp::Replace(Lane::I32, 0), &[z, v])
                    }
                    _ => return None,
                }
            }
            OpKind::Immediate8 => {
                let v = self.c32(i.immediate8() as u32);
                let z = self.vzero();
                self.vop(VecOp::Replace(Lane::I32, 0), &[z, v])
            }
            _ => return None,
        })
    }

    /// Writes a vector result to the destination register operand.
    fn vdst(&mut self, i: &Instruction, v: V) {
        let r = i.op0_register();
        if is_xmm(r) {
            self.emit_to(xmm_vreg(r), Op::Copy(v));
        } else if is_mm(r) {
            self.emit_to(mm_vreg(r), Op::Vec(VecOp::Extract(Lane::I64, 0), vec![v]));
            self.mmx_touch();
        } else {
            let lane = if r.size() == 8 { Lane::I64 } else { Lane::I32 };
            let x = self.vop(VecOp::Extract(lane, 0), &[v]);
            self.write_reg(r, x);
        }
    }

    fn uses_mmx(i: &Instruction) -> bool {
        (0..i.op_count()).any(|n| i.op_kind(n) == OpKind::Register && is_mm(i.op_register(n)))
    }

    /// Bytes in the destination register (8 for MMX, 16 for XMM).
    fn reg_bytes(i: &Instruction) -> u8 {
        if Self::uses_mmx(i) {
            8
        } else {
            16
        }
    }

    pub(super) fn lift_simd(&mut self, i: &Instruction) -> bool {
        use Mnemonic as M;
        let m = i.mnemonic();
        macro_rules! bail {
            ($why:expr) => {{
                self.unsupported(i, $why);
                return false;
            }};
        }
        macro_rules! src {
            ($n:expr) => {
                match self.vsrc(i, $n) {
                    Some(v) => v,
                    None => bail!("SIMD operand form"),
                }
            };
        }
        // Binary operations of the form dst = dst op src.
        let bin = match m {
            M::Paddb => Some(VBin::I8x16Add),
            M::Paddw => Some(VBin::I16x8Add),
            M::Paddd => Some(VBin::I32x4Add),
            M::Paddq => Some(VBin::I64x2Add),
            M::Psubb => Some(VBin::I8x16Sub),
            M::Psubw => Some(VBin::I16x8Sub),
            M::Psubd => Some(VBin::I32x4Sub),
            M::Psubq => Some(VBin::I64x2Sub),
            M::Paddsb => Some(VBin::I8x16AddSatS),
            M::Paddsw => Some(VBin::I16x8AddSatS),
            M::Paddusb => Some(VBin::I8x16AddSatU),
            M::Paddusw => Some(VBin::I16x8AddSatU),
            M::Psubsb => Some(VBin::I8x16SubSatS),
            M::Psubsw => Some(VBin::I16x8SubSatS),
            M::Psubusb => Some(VBin::I8x16SubSatU),
            M::Psubusw => Some(VBin::I16x8SubSatU),
            M::Pmullw => Some(VBin::I16x8Mul),
            M::Pmaddwd => Some(VBin::I32x4DotI16x8S),
            M::Pand | M::Andps | M::Andpd => Some(VBin::V128And),
            M::Por | M::Orps | M::Orpd => Some(VBin::V128Or),
            M::Pxor | M::Xorps | M::Xorpd => Some(VBin::V128Xor),
            M::Pcmpeqb => Some(VBin::I8x16Eq),
            M::Pcmpeqw => Some(VBin::I16x8Eq),
            M::Pcmpeqd => Some(VBin::I32x4Eq),
            M::Pcmpgtb => Some(VBin::I8x16GtS),
            M::Pcmpgtw => Some(VBin::I16x8GtS),
            M::Pcmpgtd => Some(VBin::I32x4GtS),
            M::Pavgb => Some(VBin::I8x16AvgrU),
            M::Pavgw => Some(VBin::I16x8AvgrU),
            M::Pmaxub => Some(VBin::I8x16MaxU),
            M::Pminub => Some(VBin::I8x16MinU),
            M::Pmaxsw => Some(VBin::I16x8MaxS),
            M::Pminsw => Some(VBin::I16x8MinS),
            M::Addps => Some(VBin::F32x4Add),
            M::Subps => Some(VBin::F32x4Sub),
            M::Mulps => Some(VBin::F32x4Mul),
            M::Divps => Some(VBin::F32x4Div),
            M::Addpd => Some(VBin::F64x2Add),
            M::Subpd => Some(VBin::F64x2Sub),
            M::Mulpd => Some(VBin::F64x2Mul),
            M::Divpd => Some(VBin::F64x2Div),
            _ => None,
        };
        if let Some(op) = bin {
            let a = src!(0);
            let b = src!(1);
            let r = self.vbin(op, a, b);
            self.vdst(i, r);
            return true;
        }
        match m {
            // ---- moves
            M::Movaps
            | M::Movups
            | M::Movapd
            | M::Movupd
            | M::Movdqa
            | M::Movdqu
            | M::Movntps
            | M::Movntpd
            | M::Movntdq
            | M::Lddqu => {
                if i.op0_kind() == OpKind::Memory {
                    let v = src!(1);
                    let (a, sp) = self.ea(i);
                    let mem = self.mem(16, sp);
                    self.effect(Op::Store {
                        addr: a,
                        val: v,
                        mem,
                    });
                } else {
                    let v = src!(1);
                    self.vdst(i, v);
                }
            }
            M::Movss | M::Movsd => {
                let size: u8 = if m == M::Movss { 4 } else { 8 };
                if i.op0_kind() == OpKind::Memory {
                    let v = src!(1);
                    let lane = if size == 4 { Lane::I32 } else { Lane::I64 };
                    let x = self.vop(VecOp::Extract(lane, 0), &[v]);
                    let (a, sp) = self.ea(i);
                    let mem = self.mem(size as u32, sp);
                    self.effect(Op::Store {
                        addr: a,
                        val: x,
                        mem,
                    });
                } else if i.op1_kind() == OpKind::Memory {
                    let v = src!(1);
                    self.vdst(i, v);
                } else {
                    // Register to register: only the low element moves.
                    let d = src!(0);
                    let s = src!(1);
                    let mut p: Vec<(u8, u8)> = (0..size).map(|b| (1, b)).collect();
                    p.extend((size..16).map(|b| (0, b)));
                    let r = self.shuffle(d, s, bytes(&p));
                    self.vdst(i, r);
                }
            }
            M::Movd => {
                if i.op0_kind() == OpKind::Register
                    && (is_xmm(i.op0_register()) || is_mm(i.op0_register()))
                {
                    // xmm/mm <- r32/m32, zero-extended.
                    let v = match i.op1_kind() {
                        OpKind::Register => self.read_reg(i.op1_register()),
                        _ => {
                            let (a, sp) = self.ea(i);
                            self.load(a, 4, sp)
                        }
                    };
                    let z = self.vzero();
                    let r = self.vop(VecOp::Replace(Lane::I32, 0), &[z, v]);
                    self.vdst(i, r);
                } else {
                    let v = src!(1);
                    let x = self.vop(VecOp::Extract(Lane::I32, 0), &[v]);
                    match i.op0_kind() {
                        OpKind::Register => self.write_reg(i.op0_register(), x),
                        _ => {
                            let (a, sp) = self.ea(i);
                            self.store(a, x, 4, sp);
                        }
                    }
                    if Self::uses_mmx(i) {
                        self.mmx_touch();
                    }
                }
            }
            M::Movq | M::Movnti | M::Movntq | M::Movq2dq | M::Movdq2q => {
                if m == M::Movnti {
                    let reg = i.op1_register();
                    let v = self.read_reg(reg);
                    let (a, sp) = self.ea(i);
                    self.store(a, v, reg.size() as u32, sp);
                    return true;
                }
                let v = src!(1);
                if i.op0_kind() == OpKind::Memory {
                    let x = self.vop(VecOp::Extract(Lane::I64, 0), &[v]);
                    let (a, sp) = self.ea(i);
                    let mem = self.mem(8, sp);
                    self.effect(Op::Store {
                        addr: a,
                        val: x,
                        mem,
                    });
                    if Self::uses_mmx(i) {
                        self.mmx_touch();
                    }
                } else {
                    // Zero the upper half.
                    let z = self.vzero();
                    let r = self.shuffle(v, z, lanes(8, &[(0, 0), (1, 0)]));
                    self.vdst(i, r);
                }
            }
            M::Movlps | M::Movlpd | M::Movhps | M::Movhpd => {
                let lane = if matches!(m, M::Movlps | M::Movlpd) {
                    0
                } else {
                    1
                };
                if i.op0_kind() == OpKind::Memory {
                    let v = src!(1);
                    let x = self.vop(VecOp::Extract(Lane::I64, lane), &[v]);
                    let (a, sp) = self.ea(i);
                    let mem = self.mem(8, sp);
                    self.effect(Op::Store {
                        addr: a,
                        val: x,
                        mem,
                    });
                } else {
                    let d = src!(0);
                    let (a, sp) = self.ea(i);
                    let mem = self.mem(8, sp);
                    let x = self.emit(Ty::I64, Op::Load { addr: a, mem });
                    let r = self.vop(VecOp::Replace(Lane::I64, lane), &[d, x]);
                    self.vdst(i, r);
                }
            }
            M::Movhlps | M::Movlhps => {
                let d = src!(0);
                let s = src!(1);
                let r = if m == M::Movhlps {
                    self.shuffle(d, s, lanes(8, &[(1, 1), (0, 1)]))
                } else {
                    self.shuffle(d, s, lanes(8, &[(0, 0), (1, 0)]))
                };
                self.vdst(i, r);
            }
            M::Movmskps | M::Movmskpd | M::Pmovmskb => {
                let s = src!(1);
                let lane = match m {
                    M::Movmskps => Lane::I32,
                    M::Movmskpd => Lane::I64,
                    _ => Lane::I8,
                };
                let mut x = self.vop(VecOp::Bitmask(lane), &[s]);
                if Self::uses_mmx(i) {
                    x = self.bini(BinOp::I32And, x, 0xff);
                    self.mmx_touch();
                }
                self.write_reg(i.op0_register(), x);
            }

            // ---- floating point
            M::Sqrtps | M::Sqrtpd => {
                let s = src!(1);
                let r = self.vun(
                    if m == M::Sqrtps {
                        VUn::F32x4Sqrt
                    } else {
                        VUn::F64x2Sqrt
                    },
                    s,
                );
                self.vdst(i, r);
            }
            M::Rcpps | M::Rsqrtps => {
                let s = src!(1);
                let one = self.splat_f32(1.0);
                let x = if m == M::Rsqrtps {
                    self.vun(VUn::F32x4Sqrt, s)
                } else {
                    s
                };
                let r = self.vbin(VBin::F32x4Div, one, x);
                self.vdst(i, r);
            }
            M::Minps | M::Maxps | M::Minpd | M::Maxpd => {
                let a = src!(0);
                let b = src!(1);
                let op = match m {
                    M::Minps => VBin::F32x4Pmin,
                    M::Maxps => VBin::F32x4Pmax,
                    M::Minpd => VBin::F64x2Pmin,
                    _ => VBin::F64x2Pmax,
                };
                // x86: a < b ? a : b (second operand on NaN or equal zeros).
                let r = self.vbin(op, b, a);
                self.vdst(i, r);
            }
            M::Andnps | M::Andnpd | M::Pandn => {
                let a = src!(0);
                let b = src!(1);
                let r = self.vbin(VBin::V128AndNot, b, a);
                self.vdst(i, r);
            }
            M::Addss
            | M::Subss
            | M::Mulss
            | M::Divss
            | M::Minss
            | M::Maxss
            | M::Sqrtss
            | M::Rcpss
            | M::Rsqrtss
            | M::Addsd
            | M::Subsd
            | M::Mulsd
            | M::Divsd
            | M::Minsd
            | M::Maxsd
            | M::Sqrtsd => {
                let d = src!(0);
                let s = src!(1);
                let dbl = matches!(
                    m,
                    M::Addsd | M::Subsd | M::Mulsd | M::Divsd | M::Minsd | M::Maxsd | M::Sqrtsd
                );
                let r = match m {
                    M::Addss => self.vbin(VBin::F32x4Add, d, s),
                    M::Subss => self.vbin(VBin::F32x4Sub, d, s),
                    M::Mulss => self.vbin(VBin::F32x4Mul, d, s),
                    M::Divss => self.vbin(VBin::F32x4Div, d, s),
                    M::Minss => self.vbin(VBin::F32x4Pmin, s, d),
                    M::Maxss => self.vbin(VBin::F32x4Pmax, s, d),
                    M::Sqrtss => self.vun(VUn::F32x4Sqrt, s),
                    M::Rcpss | M::Rsqrtss => {
                        let one = self.splat_f32(1.0);
                        let x = if m == M::Rsqrtss {
                            self.vun(VUn::F32x4Sqrt, s)
                        } else {
                            s
                        };
                        self.vbin(VBin::F32x4Div, one, x)
                    }
                    M::Addsd => self.vbin(VBin::F64x2Add, d, s),
                    M::Subsd => self.vbin(VBin::F64x2Sub, d, s),
                    M::Mulsd => self.vbin(VBin::F64x2Mul, d, s),
                    M::Divsd => self.vbin(VBin::F64x2Div, d, s),
                    M::Minsd => self.vbin(VBin::F64x2Pmin, s, d),
                    M::Maxsd => self.vbin(VBin::F64x2Pmax, s, d),
                    _ => self.vun(VUn::F64x2Sqrt, s),
                };
                let n: u8 = if dbl { 8 } else { 4 };
                let mut p: Vec<(u8, u8)> = (0..n).map(|b| (1, b)).collect();
                p.extend((n..16).map(|b| (0, b)));
                let out = self.shuffle(d, r, bytes(&p));
                self.vdst(i, out);
            }
            M::Cmpps | M::Cmppd | M::Cmpss | M::Cmpsd => {
                let d = src!(0);
                let s = src!(1);
                let f64x = matches!(m, M::Cmppd | M::Cmpsd);
                let pred = i.immediate8() & 7;
                let (eq, ne, lt, le) = if f64x {
                    (VBin::F64x2Eq, VBin::F64x2Ne, VBin::F64x2Lt, VBin::F64x2Le)
                } else {
                    (VBin::F32x4Eq, VBin::F32x4Ne, VBin::F32x4Lt, VBin::F32x4Le)
                };
                let unord = |l: &mut Self| {
                    let x = l.vbin(ne, d, d);
                    let y = l.vbin(ne, s, s);
                    l.vbin(VBin::V128Or, x, y)
                };
                let r = match pred {
                    0 => self.vbin(eq, d, s),
                    1 => self.vbin(lt, d, s),
                    2 => self.vbin(le, d, s),
                    3 => unord(self),
                    4 => self.vbin(ne, d, s),
                    5 => {
                        let x = self.vbin(lt, d, s);
                        self.vun(VUn::V128Not, x)
                    }
                    6 => {
                        let x = self.vbin(le, d, s);
                        self.vun(VUn::V128Not, x)
                    }
                    _ => {
                        let x = unord(self);
                        self.vun(VUn::V128Not, x)
                    }
                };
                let out = if matches!(m, M::Cmpss | M::Cmpsd) {
                    let n: u8 = if f64x { 8 } else { 4 };
                    let mut p: Vec<(u8, u8)> = (0..n).map(|b| (1, b)).collect();
                    p.extend((n..16).map(|b| (0, b)));
                    self.shuffle(d, r, bytes(&p))
                } else {
                    r
                };
                self.vdst(i, out);
            }
            M::Comiss | M::Ucomiss | M::Comisd | M::Ucomisd => {
                let d = src!(0);
                let s = src!(1);
                let (a, b) = if matches!(m, M::Comiss | M::Ucomiss) {
                    let a = self.vop(VecOp::Extract(Lane::F32, 0), &[d]);
                    let b = self.vop(VecOp::Extract(Lane::F32, 0), &[s]);
                    (
                        self.un(UnOp::F64PromoteF32, a),
                        self.un(UnOp::F64PromoteF32, b),
                    )
                } else {
                    (
                        self.vop(VecOp::Extract(Lane::F64, 0), &[d]),
                        self.vop(VecOp::Extract(Lane::F64, 0), &[s]),
                    )
                };
                let lt = self.bin(BinOp::F64Lt, a, b);
                let gt = self.bin(BinOp::F64Gt, a, b);
                let eq = self.bin(BinOp::F64Eq, a, b);
                let o = self.bin(BinOp::I32Or, lt, gt);
                let o = self.bin(BinOp::I32Or, o, eq);
                let un = self.is_zero(o);
                let cf = self.bin(BinOp::I32Or, lt, un);
                let zf = self.bin(BinOp::I32Or, eq, un);
                let pf = self.bini(BinOp::I32Shl, un, 2);
                let z = self.bini(BinOp::I32Shl, zf, 6);
                let e = self.bin(BinOp::I32Or, cf, pf);
                let e = self.bin(BinOp::I32Or, e, z);
                self.set_flags_explicit(e);
            }
            M::Shufps | M::Shufpd => {
                let d = src!(0);
                let s = src!(1);
                let imm = i.immediate8();
                let mask = if m == M::Shufps {
                    lanes(
                        4,
                        &[
                            (0, imm & 3),
                            (0, imm >> 2 & 3),
                            (1, imm >> 4 & 3),
                            (1, imm >> 6 & 3),
                        ],
                    )
                } else {
                    lanes(8, &[(0, imm & 1), (1, imm >> 1 & 1)])
                };
                let r = self.shuffle(d, s, mask);
                self.vdst(i, r);
            }
            M::Unpcklps | M::Unpckhps | M::Unpcklpd | M::Unpckhpd => {
                let d = src!(0);
                let s = src!(1);
                let mask = match m {
                    M::Unpcklps => lanes(4, &[(0, 0), (1, 0), (0, 1), (1, 1)]),
                    M::Unpckhps => lanes(4, &[(0, 2), (1, 2), (0, 3), (1, 3)]),
                    M::Unpcklpd => lanes(8, &[(0, 0), (1, 0)]),
                    _ => lanes(8, &[(0, 1), (1, 1)]),
                };
                let r = self.shuffle(d, s, mask);
                self.vdst(i, r);
            }

            // ---- conversions
            M::Cvtsi2ss | M::Cvtsi2sd => {
                let d = src!(0);
                let x = match i.op1_kind() {
                    OpKind::Register => self.read_reg(i.op1_register()),
                    _ => {
                        let (a, sp) = self.ea(i);
                        let size = i.memory_size().size() as u32;
                        self.load(a, size, sp)
                    }
                };
                let wide = self.f.ty(x) == Ty::I64;
                let r = if m == M::Cvtsi2ss {
                    let op = if wide {
                        UnOp::F32ConvertI64S
                    } else {
                        UnOp::F32ConvertI32S
                    };
                    let f = self.un(op, x);
                    self.vop(VecOp::Replace(Lane::F32, 0), &[d, f])
                } else {
                    let op = if wide {
                        UnOp::F64ConvertI64S
                    } else {
                        UnOp::F64ConvertI32S
                    };
                    let f = self.un(op, x);
                    self.vop(VecOp::Replace(Lane::F64, 0), &[d, f])
                };
                self.vdst(i, r);
            }
            M::Cvtss2sd | M::Cvtsd2ss => {
                let d = src!(0);
                let s = src!(1);
                let r = if m == M::Cvtss2sd {
                    let x = self.vop(VecOp::Extract(Lane::F32, 0), &[s]);
                    let y = self.un(UnOp::F64PromoteF32, x);
                    self.vop(VecOp::Replace(Lane::F64, 0), &[d, y])
                } else {
                    let x = self.vop(VecOp::Extract(Lane::F64, 0), &[s]);
                    let y = self.un(UnOp::F32DemoteF64, x);
                    self.vop(VecOp::Replace(Lane::F32, 0), &[d, y])
                };
                self.vdst(i, r);
            }
            M::Cvttss2si | M::Cvtss2si | M::Cvttsd2si | M::Cvtsd2si => {
                let s = src!(1);
                let x = if matches!(m, M::Cvttss2si | M::Cvtss2si) {
                    let f = self.vop(VecOp::Extract(Lane::F32, 0), &[s]);
                    self.un(UnOp::F64PromoteF32, f)
                } else {
                    self.vop(VecOp::Extract(Lane::F64, 0), &[s])
                };
                let r = if matches!(m, M::Cvttss2si | M::Cvttsd2si) {
                    self.un(UnOp::F64Trunc, x)
                } else {
                    self.mxcsr_round(x)
                };
                let v = if i.op0_register().size() == 8 {
                    self.f64_to_i64_indefinite(r)
                } else {
                    self.f64_to_i32_indefinite(r)
                };
                self.write_reg(i.op0_register(), v);
            }
            M::Cvtdq2ps => {
                let s = src!(1);
                let r = self.vun(VUn::F32x4ConvertI32x4S, s);
                self.vdst(i, r);
            }
            M::Cvtps2dq | M::Cvttps2dq => {
                let s = src!(1);
                let x = if m == M::Cvttps2dq {
                    self.vun(VUn::F32x4Trunc, s)
                } else {
                    self.vround_f32(s)
                };
                let r = self.vun(VUn::I32x4TruncSatF32x4S, x);
                // Lanes out of range (or NaN) become 0x80000000.
                let lo = self.splat_f32(-2147483648.0);
                let hi = self.splat_f32(2147483648.0);
                let ge = self.vbin(VBin::F32x4Le, lo, x);
                let lt = self.vbin(VBin::F32x4Lt, x, hi);
                let ok = self.vbin(VBin::V128And, ge, lt);
                let ind = self.splat_i32(0x8000_0000);
                let out = self.vblend(ok, r, ind);
                self.vdst(i, out);
            }
            M::Cvtdq2pd => {
                let s = src!(1);
                let r = self.vun(VUn::F64x2ConvertLowI32x4S, s);
                self.vdst(i, r);
            }
            M::Cvtpd2dq | M::Cvttpd2dq => {
                let s = src!(1);
                let x = if m == M::Cvttpd2dq {
                    self.vun(VUn::F64x2Trunc, s)
                } else {
                    self.vround_f64(s)
                };
                let r = self.vun(VUn::I32x4TruncSatF64x2SZero, x);
                let lo = self.splat_f64(-2147483648.0);
                let hi = self.splat_f64(2147483648.0);
                let ge = self.vbin(VBin::F64x2Le, lo, x);
                let lt = self.vbin(VBin::F64x2Lt, x, hi);
                let ok = self.vbin(VBin::V128And, ge, lt);
                // 64-bit masks -> 32-bit lanes 0, 1 (upper lanes zero).
                let z = self.vzero();
                let ok32 = self.shuffle(ok, z, lanes(4, &[(0, 0), (0, 2), (1, 0), (1, 0)]));
                let ind = self.splat_i32(0x8000_0000);
                let ind = self.shuffle(ind, z, lanes(4, &[(0, 0), (0, 0), (1, 0), (1, 0)]));
                let out = self.vblend(ok32, r, ind);
                self.vdst(i, out);
            }
            M::Cvtps2pd => {
                let s = src!(1);
                let r = self.vun(VUn::F64x2PromoteLowF32x4, s);
                self.vdst(i, r);
            }
            M::Cvtpd2ps => {
                let s = src!(1);
                let r = self.vun(VUn::F32x4DemoteF64x2Zero, s);
                self.vdst(i, r);
            }

            M::Cvtpi2ps | M::Cvtpi2pd => {
                // Two int32 from mm/m64 to floats.
                let s = src!(1);
                if m == M::Cvtpi2ps {
                    let d = src!(0);
                    let f = self.vun(VUn::F32x4ConvertI32x4S, s);
                    let r = self.shuffle(f, d, lanes(4, &[(0, 0), (0, 1), (1, 2), (1, 3)]));
                    self.vdst(i, r);
                } else {
                    let r = self.vun(VUn::F64x2ConvertLowI32x4S, s);
                    self.vdst(i, r);
                }
                self.mmx_touch();
            }
            M::Cvtps2pi | M::Cvttps2pi => {
                let s = src!(1);
                let x = if m == M::Cvttps2pi {
                    self.vun(VUn::F32x4Trunc, s)
                } else {
                    self.vround_f32(s)
                };
                let r = self.vun(VUn::I32x4TruncSatF32x4S, x);
                let lo = self.splat_f32(-2147483648.0);
                let hi = self.splat_f32(2147483648.0);
                let ge = self.vbin(VBin::F32x4Le, lo, x);
                let lt = self.vbin(VBin::F32x4Lt, x, hi);
                let ok = self.vbin(VBin::V128And, ge, lt);
                let ind = self.splat_i32(0x8000_0000);
                let out = self.vblend(ok, r, ind);
                self.vdst(i, out);
            }
            M::Cvtpd2pi | M::Cvttpd2pi => {
                let s = src!(1);
                let x = if m == M::Cvttpd2pi {
                    self.vun(VUn::F64x2Trunc, s)
                } else {
                    self.vround_f64(s)
                };
                let r = self.vun(VUn::I32x4TruncSatF64x2SZero, x);
                let lo = self.splat_f64(-2147483648.0);
                let hi = self.splat_f64(2147483648.0);
                let ge = self.vbin(VBin::F64x2Le, lo, x);
                let lt = self.vbin(VBin::F64x2Lt, x, hi);
                let ok = self.vbin(VBin::V128And, ge, lt);
                let ok32 = self.shuffle(ok, ok, lanes(4, &[(0, 0), (0, 2), (0, 0), (0, 2)]));
                let ind = self.splat_i32(0x8000_0000);
                let out = self.vblend(ok32, r, ind);
                self.vdst(i, out);
            }

            // ---- integer
            M::Pmulhw | M::Pmulhuw => {
                let a = src!(0);
                let b = src!(1);
                let (lo, hi, shr, nar) = if m == M::Pmulhw {
                    (
                        VBin::I32x4ExtMulLowI16x8S,
                        VBin::I32x4ExtMulHighI16x8S,
                        true,
                        VBin::I16x8NarrowI32x4S,
                    )
                } else {
                    (
                        VBin::I32x4ExtMulLowI16x8U,
                        VBin::I32x4ExtMulHighI16x8U,
                        false,
                        VBin::I16x8NarrowI32x4U,
                    )
                };
                let l = self.vbin(lo, a, b);
                let h = self.vbin(hi, a, b);
                let k = self.c32(16);
                let sh = if shr {
                    VecOp::ShrS(Lane::I32)
                } else {
                    VecOp::ShrU(Lane::I32)
                };
                let l = self.vop(sh.clone(), &[l, k]);
                let h = self.vop(sh, &[h, k]);
                let r = self.vbin(nar, l, h);
                self.vdst(i, r);
            }
            M::Pmuludq => {
                let a = src!(0);
                let b = src!(1);
                // Lanes 0 and 2 to lanes 0 and 1.
                let m02 = lanes(4, &[(0, 0), (0, 2), (0, 1), (0, 3)]);
                let a2 = self.shuffle(a, a, m02);
                let b2 = self.shuffle(b, b, m02);
                let r = self.vbin(VBin::I64x2ExtMulLowI32x4U, a2, b2);
                self.vdst(i, r);
            }
            M::Psadbw => {
                let a = src!(0);
                let b = src!(1);
                let mx = self.vbin(VBin::I8x16MaxU, a, b);
                let mn = self.vbin(VBin::I8x16MinU, a, b);
                let d = self.vbin(VBin::I8x16Sub, mx, mn);
                let w = self.vun(VUn::I16x8ExtAddPairwiseI8x16U, d);
                let q = self.vun(VUn::I32x4ExtAddPairwiseI16x8U, w);
                // q = [s0, s1, s2, s3] -> lanes 0 and 2 get s0+s1 and s2+s3.
                let sw = self.shuffle(q, q, lanes(4, &[(0, 1), (0, 0), (0, 3), (0, 2)]));
                let sum = self.vbin(VBin::I32x4Add, q, sw);
                let z = self.vzero();
                let r = self.shuffle(sum, z, lanes(4, &[(0, 0), (1, 0), (0, 2), (1, 0)]));
                self.vdst(i, r);
            }
            M::Psllw
            | M::Pslld
            | M::Psllq
            | M::Psrlw
            | M::Psrld
            | M::Psrlq
            | M::Psraw
            | M::Psrad => {
                let (lane, w) = match m {
                    M::Psllw | M::Psrlw | M::Psraw => (Lane::I16, 16u32),
                    M::Pslld | M::Psrld | M::Psrad => (Lane::I32, 32),
                    _ => (Lane::I64, 64),
                };
                let arith = matches!(m, M::Psraw | M::Psrad);
                let op = match m {
                    M::Psllw | M::Pslld | M::Psllq => VecOp::Shl(lane),
                    M::Psraw | M::Psrad => VecOp::ShrS(lane),
                    _ => VecOp::ShrU(lane),
                };
                let v = src!(0);
                let r = if i.op1_kind() == OpKind::Immediate8 {
                    let c = i.immediate8() as u32;
                    if c >= w && !arith {
                        self.vzero()
                    } else {
                        let k = self.c32(c.min(w - 1));
                        self.vop(op, &[v, k])
                    }
                } else {
                    let cv = src!(1);
                    let c64 = self.vop(VecOp::Extract(Lane::I64, 0), &[cv]);
                    let lim = self.c64(w as u64 - 1);
                    let big = self.bin(BinOp::I64GtU, c64, lim);
                    let c32 = self.un(UnOp::I32WrapI64, c64);
                    if arith {
                        let wm = self.c32(w - 1);
                        let c = self.select(big, wm, c32);
                        self.vop(op, &[v, c])
                    } else {
                        let sh = self.vop(op, &[v, c32]);
                        let z = self.vzero();
                        self.vselect(big, z, sh)
                    }
                };
                self.vdst(i, r);
            }
            M::Pslldq | M::Psrldq => {
                let v = src!(0);
                let n = (i.immediate8() as u32).min(16) as u8;
                let z = self.vzero();
                let mut p = vec![];
                for k in 0..16u8 {
                    if m == M::Pslldq {
                        p.push(if k >= n { (0, k - n) } else { (1, 0) });
                    } else {
                        p.push(if k + n < 16 { (0, k + n) } else { (1, 0) });
                    }
                }
                let r = self.shuffle(v, z, bytes(&p));
                self.vdst(i, r);
            }
            M::Punpcklbw
            | M::Punpcklwd
            | M::Punpckldq
            | M::Punpcklqdq
            | M::Punpckhbw
            | M::Punpckhwd
            | M::Punpckhdq
            | M::Punpckhqdq => {
                let d = src!(0);
                let s = src!(1);
                let size = match m {
                    M::Punpcklbw | M::Punpckhbw => 1u8,
                    M::Punpcklwd | M::Punpckhwd => 2,
                    M::Punpckldq | M::Punpckhdq => 4,
                    _ => 8,
                };
                let half = Self::reg_bytes(i) / 2;
                let start = if matches!(
                    m,
                    M::Punpckhbw | M::Punpckhwd | M::Punpckhdq | M::Punpckhqdq
                ) {
                    half
                } else {
                    0
                };
                let mut picks = vec![];
                for k in 0..half / size {
                    picks.push((0, (start / size) + k));
                    picks.push((1, (start / size) + k));
                }
                let r = self.shuffle(d, s, lanes(size, &picks));
                self.vdst(i, r);
            }
            M::Packsswb | M::Packssdw | M::Packuswb => {
                let d = src!(0);
                let s = src!(1);
                let op = match m {
                    M::Packsswb => VBin::I8x16NarrowI16x8S,
                    M::Packssdw => VBin::I16x8NarrowI32x4S,
                    _ => VBin::I8x16NarrowI16x8U,
                };
                let r = if Self::uses_mmx(i) {
                    // Combine both 64-bit inputs first.
                    let c = self.shuffle(d, s, lanes(8, &[(0, 0), (1, 0)]));
                    self.vbin(op, c, c)
                } else {
                    self.vbin(op, d, s)
                };
                self.vdst(i, r);
            }
            M::Pshufd | M::Pshuflw | M::Pshufhw | M::Pshufw => {
                let s = src!(1);
                let imm = i.immediate8();
                let sel = |k: u8| imm >> (2 * k) & 3;
                let mask = match m {
                    M::Pshufd => lanes(4, &[(0, sel(0)), (0, sel(1)), (0, sel(2)), (0, sel(3))]),
                    M::Pshuflw => lanes(
                        2,
                        &[
                            (0, sel(0)),
                            (0, sel(1)),
                            (0, sel(2)),
                            (0, sel(3)),
                            (0, 4),
                            (0, 5),
                            (0, 6),
                            (0, 7),
                        ],
                    ),
                    M::Pshufhw => lanes(
                        2,
                        &[
                            (0, 0),
                            (0, 1),
                            (0, 2),
                            (0, 3),
                            (0, 4 + sel(0)),
                            (0, 4 + sel(1)),
                            (0, 4 + sel(2)),
                            (0, 4 + sel(3)),
                        ],
                    ),
                    _ => lanes(
                        2,
                        &[
                            (0, sel(0)),
                            (0, sel(1)),
                            (0, sel(2)),
                            (0, sel(3)),
                            (0, 0),
                            (0, 1),
                            (0, 2),
                            (0, 3),
                        ],
                    ),
                };
                let r = self.shuffle(s, s, mask);
                self.vdst(i, r);
            }
            M::Pextrw => {
                let s = src!(1);
                let n = if Self::uses_mmx(i) { 3 } else { 7 };
                let x = self.vop(VecOp::Extract(Lane::I16, i.immediate8() & n), &[s]);
                self.write_reg(i.op0_register(), x);
                if Self::uses_mmx(i) {
                    self.mmx_touch();
                }
            }
            M::Pinsrw => {
                let d = src!(0);
                let x = match i.op1_kind() {
                    OpKind::Register => self.read_reg(i.op1_register()),
                    _ => {
                        let (a, sp) = self.ea(i);
                        self.load(a, 2, sp)
                    }
                };
                let n = if Self::uses_mmx(i) { 3 } else { 7 };
                let r = self.vop(VecOp::Replace(Lane::I16, i.immediate8() & n), &[d, x]);
                self.vdst(i, r);
            }
            M::Emms => {
                let z = self.c32(0);
                self.store_native(z, 1, cpu::FPU_TAG);
            }
            M::Ldmxcsr => {
                let (a, sp) = self.ea(i);
                let v = self.load(a, 4, sp);
                self.emit_to(MXCSR, Op::Copy(v));
            }
            M::Stmxcsr => {
                let (a, sp) = self.ea(i);
                let v = self.copy(MXCSR);
                self.store(a, v, 4, sp);
            }
            M::Fxsave | M::Fxsave64 => self.lift_fxsave(i, true),
            M::Fxrstor | M::Fxrstor64 => self.lift_fxsave(i, false),
            M::Maskmovq | M::Maskmovdqu => bail!("masked stores"),
            _ => bail!("SIMD instruction not supported"),
        }
        let _ = (fault::UNSUPPORTED, MemorySize::Unknown);
        true
    }

    /// Rounds an f64 by MXCSR's rounding control.
    fn mxcsr_round(&mut self, v: V) -> V {
        let rc = self.bini(BinOp::I32ShrU, MXCSR, 13);
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

    fn vround_by(&mut self, v: V, ops: [VUn; 4]) -> V {
        let rc = self.bini(BinOp::I32ShrU, MXCSR, 13);
        let rc = self.bini(BinOp::I32And, rc, 3);
        let near = self.vun(ops[0], v);
        let down = self.vun(ops[1], v);
        let up = self.vun(ops[2], v);
        let zero = self.vun(ops[3], v);
        let is1 = self.bini(BinOp::I32Eq, rc, 1);
        let is2 = self.bini(BinOp::I32Eq, rc, 2);
        let is3 = self.bini(BinOp::I32Eq, rc, 3);
        let x = self.vselect(is3, zero, near);
        let x = self.vselect(is2, up, x);
        self.vselect(is1, down, x)
    }

    fn vround_f32(&mut self, v: V) -> V {
        self.vround_by(
            v,
            [
                VUn::F32x4Nearest,
                VUn::F32x4Floor,
                VUn::F32x4Ceil,
                VUn::F32x4Trunc,
            ],
        )
    }

    fn vround_f64(&mut self, v: V) -> V {
        self.vround_by(
            v,
            [
                VUn::F64x2Nearest,
                VUn::F64x2Floor,
                VUn::F64x2Ceil,
                VUn::F64x2Trunc,
            ],
        )
    }

    /// f64 (already rounded) to i32 with x86's out-of-range result.
    fn f64_to_i32_indefinite(&mut self, r: V) -> V {
        let lo = self.emit(Ty::F64, Op::Const((-2147483648.0f64).to_bits()));
        let hi = self.emit(Ty::F64, Op::Const(2147483648.0f64.to_bits()));
        let ge = self.bin(BinOp::F64Ge, r, lo);
        let lt = self.bin(BinOp::F64Lt, r, hi);
        let ok = self.bin(BinOp::I32And, ge, lt);
        let x = self.un(UnOp::I32TruncSatF64S, r);
        let ind = self.c32(0x8000_0000);
        self.select(ok, x, ind)
    }

    /// f64 (already rounded) to i64 with x86's out-of-range result.
    fn f64_to_i64_indefinite(&mut self, r: V) -> V {
        let lo = self.emit(Ty::F64, Op::Const((-9223372036854775808.0f64).to_bits()));
        let hi = self.emit(Ty::F64, Op::Const(9223372036854775808.0f64.to_bits()));
        let ge = self.bin(BinOp::F64Ge, r, lo);
        let lt = self.bin(BinOp::F64Lt, r, hi);
        let ok = self.bin(BinOp::I32And, ge, lt);
        let x = self.un(UnOp::I64TruncSatF64S, r);
        let ind = self.c64(1 << 63);
        self.select(ok, x, ind)
    }

    /// fxsave/fxrstor with the 32-bit layout (in 64-bit code, xmm8-15 too).
    fn lift_fxsave(&mut self, i: &Instruction, save: bool) {
        let (a, sp) = self.ea(i);
        let nxmm = if self.f.mode == Mode::X64 { 16 } else { 8 };
        let at = |l: &mut Self, off: u32| l.addr_add(a, off);
        if save {
            let cw = self.copy(FPU_CW);
            self.store(a, cw, 2, sp);
            let top = self.bini(BinOp::I32Shl, FPU_TOP, 11);
            let sw = self.bini(BinOp::I32And, FPU_SW, !0x3800 & 0xffff);
            let sw = self.bin(BinOp::I32Or, sw, top);
            let p = at(self, 2);
            self.store(p, sw, 2, sp);
            let tag = self.load_native(Ty::I32, 1, cpu::FPU_TAG);
            let p = at(self, 4);
            self.store(p, tag, 1, sp);
            let z = self.c32(0);
            for off in [5, 6, 8, 12, 16, 20] {
                let p = at(self, off);
                self.store(
                    p,
                    z,
                    if off == 5 {
                        1
                    } else if off == 6 {
                        2
                    } else {
                        4
                    },
                    sp,
                );
            }
            let mx = self.copy(MXCSR);
            let p = at(self, 24);
            self.store(p, mx, 4, sp);
            let mask = self.c32(0xffff);
            let p = at(self, 28);
            self.store(p, mask, 4, sp);
            for r in 0..8u32 {
                let v = self.st(r);
                let lo = self.emit(Ty::I64, Op::CallHelper(Helper::F64ToF80Lo, vec![v]));
                let hi = self.emit(Ty::I32, Op::CallHelper(Helper::F64ToF80Hi, vec![v]));
                let p = at(self, 32 + r * 16);
                let mem = self.mem(8, sp);
                self.effect(Op::Store {
                    addr: p,
                    val: lo,
                    mem,
                });
                let p = at(self, 40 + r * 16);
                self.store(p, hi, 2, sp);
            }
            for r in 0..nxmm {
                let p = at(self, 160 + r * 16);
                let mem = self.mem(16, sp);
                self.effect(Op::Store {
                    addr: p,
                    val: xmm(r),
                    mem,
                });
            }
        } else {
            let cw = self.load(a, 2, sp);
            self.emit_to(FPU_CW, Op::Copy(cw));
            let p = at(self, 2);
            let sw = self.load(p, 2, sp);
            let top = self.bini(BinOp::I32ShrU, sw, 11);
            let top = self.bini(BinOp::I32And, top, 7);
            let sw = self.bini(BinOp::I32And, sw, !0x3800 & 0xffff);
            self.emit_to(FPU_SW, Op::Copy(sw));
            self.emit_to(FPU_TOP, Op::Copy(top));
            let p = at(self, 4);
            let tag = self.load(p, 1, sp);
            self.store_native(tag, 1, cpu::FPU_TAG);
            let p = at(self, 24);
            let mx = self.load(p, 4, sp);
            self.emit_to(MXCSR, Op::Copy(mx));
            for r in 0..8u32 {
                let p = at(self, 32 + r * 16);
                let mem = self.mem(8, sp);
                let lo = self.emit(Ty::I64, Op::Load { addr: p, mem });
                let p = at(self, 40 + r * 16);
                let hi = self.load(p, 2, sp);
                let v = self.emit(Ty::F64, Op::CallHelper(Helper::F80ToF64, vec![lo, hi]));
                self.set_st(r, v);
            }
            for r in 0..nxmm {
                let p = at(self, 160 + r * 16);
                let mem = self.mem(16, sp);
                self.emit_to(xmm(r), Op::Load { addr: p, mem });
            }
        }
    }
}
