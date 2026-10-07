//! MMX and SSE/SSE2.

use iced_x86::Instruction;

use super::Lifter;

pub fn is_simd(i: &Instruction) -> bool {
    let _ = i;
    false
}

impl<'a> Lifter<'a> {
    pub(super) fn lift_simd(&mut self, i: &Instruction) -> bool {
        self.unsupported(i, "SIMD not yet supported");
        false
    }
}
