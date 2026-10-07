//! WebAssembly bodies for the 80-bit conversion helpers used by x87
//! `tbyte` loads and stores. They mirror [`crate::fpu`], which the tests use
//! as the reference.

use wasm_encoder::{BlockType, Instruction as W, ValType};

use crate::ir::Helper;

pub fn gen(h: Helper, out: &mut Vec<W<'static>>, locals: &mut Vec<ValType>) {
    match h {
        Helper::F64ToF80Lo | Helper::F64ToF80Hi => f64_to_f80(h == Helper::F64ToF80Lo, out, locals),
        Helper::F80ToF64 => f80_to_f64(out, locals),
        _ => unreachable!("not an FPU helper"),
    }
}

/// param 0: f64. Locals: 1 bits (i64), 2 exp (i32), 3 frac (i64), 4 lz (i64).
fn f64_to_f80(lo: bool, out: &mut Vec<W<'static>>, locals: &mut Vec<ValType>) {
    locals.extend([ValType::I64, ValType::I32, ValType::I64, ValType::I64]);
    out.extend([
        W::LocalGet(0),
        W::I64ReinterpretF64,
        W::LocalSet(1),
        W::LocalGet(1),
        W::I64Const(52),
        W::I64ShrU,
        W::I32WrapI64,
        W::I32Const(0x7ff),
        W::I32And,
        W::LocalSet(2),
        W::LocalGet(1),
        W::I64Const((1i64 << 52) - 1),
        W::I64And,
        W::LocalSet(3),
        // lz = clz(frac) - 11
        W::LocalGet(3),
        W::I64Clz,
        W::I64Const(11),
        W::I64Sub,
        W::LocalSet(4),
    ]);
    if lo {
        out.extend([
            W::LocalGet(2),
            W::I32Eqz,
            W::If(BlockType::Empty),
            // Zero or subnormal: normalize the fraction.
            W::LocalGet(3),
            W::LocalGet(3),
            W::LocalGet(4),
            W::I64Const(11),
            W::I64Add,
            W::I64Shl,
            W::LocalGet(3),
            W::I64Eqz,
            W::Select,
            W::Return,
            W::End,
            W::LocalGet(3),
            W::I64Const(11),
            W::I64Shl,
            W::I64Const(i64::MIN),
            W::I64Or,
        ]);
    } else {
        // sign << 15 in local 2's place after computing the exponent word.
        out.extend([
            W::LocalGet(1),
            W::I64Const(63),
            W::I64ShrU,
            W::I32WrapI64,
            W::I32Const(15),
            W::I32Shl,
            W::LocalGet(2),
            W::I32Const(0x7ff),
            W::I32Eq,
            W::If(BlockType::Result(ValType::I32)),
            W::I32Const(0x7fff),
            W::Else,
            W::LocalGet(2),
            W::I32Eqz,
            W::If(BlockType::Result(ValType::I32)),
            // zero: 0; subnormal: 1 - 1023 - lz + 16383
            W::I32Const(1 - 1023 + 16383),
            W::LocalGet(4),
            W::I32WrapI64,
            W::I32Sub,
            W::I32Const(0),
            W::LocalGet(3),
            W::I64Eqz,
            W::I32Eqz,
            W::Select,
            W::Else,
            W::LocalGet(2),
            W::I32Const(16383 - 1023),
            W::I32Add,
            W::End,
            W::End,
            W::I32Or,
        ]);
    }
}

/// params: 0 significand (i64), 1 sign/exponent (i32).
/// Locals: 2 exp (i32), 3 f (f64), 4 k (i32), 5 step (i32).
fn f80_to_f64(out: &mut Vec<W<'static>>, locals: &mut Vec<ValType>) {
    locals.extend([ValType::I32, ValType::F64, ValType::I32, ValType::I32]);
    out.extend([
        W::LocalGet(1),
        W::I32Const(0x7fff),
        W::I32And,
        W::LocalSet(2),
        // Infinity / NaN.
        W::LocalGet(2),
        W::I32Const(0x7fff),
        W::I32Eq,
        W::If(BlockType::Empty),
        W::LocalGet(1),
        W::I64ExtendI32U,
        W::I64Const(15),
        W::I64ShrU,
        W::I64Const(63),
        W::I64Shl,
        W::I64Const(0x7ff0_0000_0000_0000),
        W::I64Or,
        // NaN: quiet, keeping the top fraction bits.
        W::LocalGet(0),
        W::I64Const(1),
        W::I64Shl,
        W::I64Const(12),
        W::I64ShrU,
        W::I64Const(0x0008_0000_0000_0000),
        W::I64Or,
        W::I64Const(0),
        W::LocalGet(0),
        W::I64Const(1),
        W::I64Shl,
        W::I64Eqz,
        W::Select,
        W::I64Or,
        W::F64ReinterpretI64,
        W::Return,
        W::End,
        // value = significand * 2^(exp - 16383 - 63), exp 0 counts as 1.
        W::LocalGet(0),
        W::F64ConvertI64U,
        W::LocalSet(3),
        W::LocalGet(2),
        W::I32Const(16383 + 63),
        W::I32Sub,
        W::LocalGet(2),
        W::I32Eqz,
        W::I32Add,
        W::LocalSet(4),
        W::Block(BlockType::Empty),
        W::Loop(BlockType::Empty),
        W::LocalGet(4),
        W::I32Eqz,
        W::BrIf(1),
        // step = clamp(k, -1022, 1023)
        W::LocalGet(4),
        W::I32Const(-1022),
        W::LocalGet(4),
        W::I32Const(-1022),
        W::I32GtS,
        W::Select,
        W::LocalTee(5),
        W::I32Const(1023),
        W::LocalGet(5),
        W::I32Const(1023),
        W::I32LtS,
        W::Select,
        W::LocalSet(5),
        W::LocalGet(3),
        W::LocalGet(5),
        W::I32Const(1023),
        W::I32Add,
        W::I64ExtendI32U,
        W::I64Const(52),
        W::I64Shl,
        W::F64ReinterpretI64,
        W::F64Mul,
        W::LocalSet(3),
        W::LocalGet(4),
        W::LocalGet(5),
        W::I32Sub,
        W::LocalSet(4),
        W::Br(0),
        W::End,
        W::End,
        // Apply the sign.
        W::LocalGet(3),
        W::F64Neg,
        W::LocalGet(3),
        W::LocalGet(1),
        W::I32Const(0x8000),
        W::I32And,
        W::Select,
    ]);
}
