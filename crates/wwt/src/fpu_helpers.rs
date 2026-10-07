//! WebAssembly implementations of x87 operations that have no single
//! WebAssembly instruction (transcendentals, partial remainder, 80-bit
//! conversions).

use wasm_encoder::{Instruction as W, ValType};

use crate::ir::Helper;

pub fn gen(h: Helper, out: &mut Vec<W<'static>>, locals: &mut Vec<ValType>) {
    let _ = locals;
    // Filled in with the x87 lifter.
    let _ = h;
    out.push(W::Unreachable);
}
