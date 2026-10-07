//! x87 floating point.

use iced_x86::{Instruction, Mnemonic};

use super::Lifter;

pub fn is_fpu(i: &Instruction) -> bool {
    let _ = Mnemonic::Fld;
    i.op_code().encoding() == iced_x86::EncodingKind::Legacy
        && matches!(i.op_code().op_code(), 0xD8..=0xDF)
        && i.op_code().table() == iced_x86::OpCodeTableKind::Normal
        || matches!(i.mnemonic(), Mnemonic::Wait)
}

impl<'a> Lifter<'a> {
    pub(super) fn lift_fpu(&mut self, i: &Instruction) -> bool {
        self.unsupported(i, "x87 not yet supported");
        false
    }
}
