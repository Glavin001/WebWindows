//! Compares translated outcomes with recorded ones, ignoring what x86
//! leaves undefined.

use iced_x86::{Decoder, DecoderOptions, Mnemonic, OpKind, RflagsBits};

use crate::case::{Case, Outcome};
use crate::layout::*;

const CF: u32 = 1;
const PF: u32 = 4;
const AF: u32 = 0x10;
const ZF: u32 = 0x40;
const SF: u32 = 0x80;
const OF: u32 = 0x800;
const DF: u32 = 0x400;

fn rflags_to_eflags(r: u32) -> u32 {
    let mut e = 0;
    if r & RflagsBits::OF != 0 {
        e |= OF;
    }
    if r & RflagsBits::SF != 0 {
        e |= SF;
    }
    if r & RflagsBits::ZF != 0 {
        e |= ZF;
    }
    if r & RflagsBits::AF != 0 {
        e |= AF;
    }
    if r & RflagsBits::CF != 0 {
        e |= CF;
    }
    if r & RflagsBits::PF != 0 {
        e |= PF;
    }
    e
}

/// What to compare for a case.
pub struct Mask {
    /// Defined eflags bits.
    pub eflags: u32,
    /// Whether the destination is undefined (skip registers and memory).
    pub skip_result: bool,
}

pub fn mask_for(case: &Case) -> Mask {
    let code = case.code_bytes();
    let mut d = Decoder::with_ip(32, &code, INS as u64, DecoderOptions::NONE);
    let mut undefined = 0u32;
    let mut skip_result = false;
    while d.can_decode() {
        let i = d.decode();
        let (u, skip) = undefined_flags(&i, case);
        let written = if shift_count_zero(&i, case) {
            0
        } else {
            rflags_to_eflags(i.rflags_modified())
        };
        undefined = (undefined & !written) | u;
        skip_result |= skip;
    }
    Mask {
        eflags: (CF | PF | AF | ZF | SF | OF | DF) & !undefined,
        skip_result,
    }
}

/// Flags an instruction leaves undefined for this case's inputs, and
/// whether its result is undefined.
pub fn undefined_flags(i: &iced_x86::Instruction, case: &Case) -> (u32, bool) {
    let mut undefined = rflags_to_eflags(i.rflags_undefined());
    let mut skip_result = false;
    use Mnemonic as M;
    let m = i.mnemonic();
    if matches!(m, M::Shl | M::Sal | M::Shr | M::Sar | M::Rol | M::Ror | M::Rcl | M::Rcr | M::Shld | M::Shrd) {
        let w = match i.op0_kind() {
            OpKind::Register => i.op0_register().size() as u32 * 8,
            _ => i.memory_size().size() as u32 * 8,
        };
        let count_op = if matches!(m, M::Shld | M::Shrd) { 2 } else { 1 };
        let raw = if i.op_count() <= count_op {
            1
        } else {
            match i.op_kind(count_op) {
                OpKind::Immediate8 => i.immediate8() as u32,
                _ => case.regs[1] & 0xff,
            }
        };
        let c = raw & 31;
        if c != 1 {
            undefined |= OF;
        }
        if matches!(m, M::Shl | M::Sal | M::Shr) && c >= w {
            undefined |= CF;
        }
        if matches!(m, M::Shld | M::Shrd) && c > w {
            skip_result = true;
            undefined |= CF | OF | SF | ZF | PF | AF;
        }
        if c == 0 {
            // Flags unchanged.
            return (0, skip_result);
        }
    }
    (undefined, skip_result)
}

fn shift_count_zero(i: &iced_x86::Instruction, case: &Case) -> bool {
    use Mnemonic as M;
    let m = i.mnemonic();
    if !matches!(m, M::Shl | M::Sal | M::Shr | M::Sar | M::Rol | M::Ror | M::Rcl | M::Rcr | M::Shld | M::Shrd) {
        return false;
    }
    let count_op = if matches!(m, M::Shld | M::Shrd) { 2 } else { 1 };
    if i.op_count() <= count_op {
        return false;
    }
    let raw = match i.op_kind(count_op) {
        OpKind::Immediate8 => i.immediate8() as u32,
        _ => case.regs[1] & 0xff,
    };
    raw & 31 == 0
}

pub fn rflags_read(i: &iced_x86::Instruction) -> u32 {
    rflags_to_eflags(i.rflags_read())
}

/// Differences between an expected and actual outcome (empty when equal).
pub fn compare(case: &Case, want: &Outcome, got: &Outcome) -> Vec<String> {
    let mask = mask_for(case);
    let mut diffs = vec![];
    if want.fault != got.fault {
        diffs.push(format!("fault: want {:?} got {:?}", want.fault, got.fault));
        return diffs;
    }
    if !mask.skip_result {
        const NAMES: [&str; 8] = ["eax", "ecx", "edx", "ebx", "esp", "ebp", "esi", "edi"];
        for r in 0..8 {
            if want.regs[r] != got.regs[r] {
                diffs.push(format!(
                    "{}: want {:#010x} got {:#010x}",
                    NAMES[r], want.regs[r], got.regs[r]
                ));
            }
        }
        if want.mem != got.mem {
            diffs.push(format!("memory: want {:x?} got {:x?}", want.mem, got.mem));
        }
    }
    if !want.fault.is_empty() {
        return diffs;
    }
    if want.eip != got.eip {
        diffs.push(format!("eip: want {:#x} got {:#x}", want.eip, got.eip));
    }
    let (we, ge) = (want.eflags & mask.eflags, got.eflags & mask.eflags);
    if we != ge {
        diffs.push(format!(
            "eflags: want {we:#06x} got {ge:#06x} (diff {:#06x}, mask {:#06x})",
            we ^ ge,
            mask.eflags
        ));
    }
    if let (Some(w), Some(g)) = (&want.fx, &got.fx) {
        diffs.extend(crate::fpucmp::compare_fx(w, g, &case.form));
    }
    diffs
}
