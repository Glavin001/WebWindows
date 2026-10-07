//! The contract between translated code, the runtime kernel and the host:
//! the per-thread CPU state layout, flag-state encoding and fault codes.
//!
//! Everything here is shared with the JavaScript runtime, which reads the
//! values from [`abi_json`] instead of hard-coding them.

use serde::Serialize;

/// Byte offsets inside the per-thread CPU state struct. The struct lives in
/// the native region of the shared memory; translated functions receive its
/// address as their only parameter.
pub mod cpu {
    /// The eight general registers in x86 encoding order
    /// (eax, ecx, edx, ebx, esp, ebp, esi, edi), 4 bytes each.
    pub const GPR: u32 = 0;
    pub const EIP: u32 = 32;
    /// Lazy flag state: kind, result and operands (see [`super::flags`]).
    pub const FK: u32 = 36;
    pub const FR: u32 = 40;
    pub const FA: u32 = 44;
    pub const FB: u32 = 48;
    pub const FC: u32 = 52;
    /// Direction flag as 0 or 1.
    pub const DF: u32 = 56;
    /// System eflags bits (IF, TF, AC, ID...) that are not tracked lazily.
    pub const EFLAGS_SYS: u32 = 60;
    pub const FS_BASE: u32 = 64;
    pub const GS_BASE: u32 = 68;
    /// Segment selectors es, cs, ss, ds, fs, gs as u16.
    pub const SEG_SEL: u32 = 72;
    /// x87: top-of-stack index (0..7), control word, status word (without
    /// top), tag word.
    pub const FPU_TOP: u32 = 84;
    pub const FPU_CW: u32 = 88;
    pub const FPU_SW: u32 = 90;
    pub const FPU_TAG: u32 = 92;
    /// x87 physical registers st0..st7 stored as f64.
    pub const FPU_ST: u32 = 96;
    /// MMX registers mm0..mm7 (i64).
    pub const MMX: u32 = 160;
    /// SSE registers xmm0..xmm7 (16 bytes each).
    pub const XMM: u32 = 224;
    pub const MXCSR: u32 = 352;
    /// Information about the last fault raised by translated code.
    pub const FAULT_CODE: u32 = 356;
    pub const FAULT_ADDR: u32 = 360;
    /// Scratch space for the host and kernel.
    pub const SCRATCH: u32 = 384;
    pub const SIZE: u32 = 512;

    pub const fn gpr(i: u32) -> u32 {
        GPR + i * 4
    }
}

/// The x86-64 CPU struct. It keeps every field of [`cpu`] that does not
/// widen (EIP, FK, DF, segment selectors, x87, MMX, xmm0-7, MXCSR, fault
/// information, scratch) at the same offset, and adds the 64-bit fields in
/// an extension after the 32-bit struct. The 32-bit GPR, lazy-operand and
/// segment-base slots are unused in this layout.
pub mod cpu64 {
    /// The sixteen general registers (rax, rcx, rdx, rbx, rsp, rbp, rsi,
    /// rdi, r8..r15), 8 bytes each.
    pub const GPR: u32 = 512;
    /// Lazy flag result and operands, 8 bytes each.
    pub const FR: u32 = 640;
    pub const FA: u32 = 648;
    pub const FB: u32 = 656;
    pub const FC: u32 = 664;
    pub const FS_BASE: u32 = 672;
    pub const GS_BASE: u32 = 680;
    /// xmm8..xmm15 (16 bytes each); xmm0..7 stay at [`super::cpu::XMM`].
    pub const XMM8: u32 = 688;
    /// The instruction pointer of 64-bit code (8 bytes); [`super::cpu::EIP`]
    /// is unused in this layout.
    pub const RIP: u32 = 816;
    pub const SIZE: u32 = 832;

    pub const fn gpr(i: u32) -> u32 {
        GPR + i * 8
    }
}

/// Lazy flag encoding. `FK` holds `op | width_code << 8`; `FR`, `FA`, `FB`
/// and `FC` hold the result and operands needed to compute each flag.
pub mod flags {
    pub const CF: u32 = 1 << 0;
    pub const PF: u32 = 1 << 2;
    pub const AF: u32 = 1 << 4;
    pub const ZF: u32 = 1 << 6;
    pub const SF: u32 = 1 << 7;
    pub const TF: u32 = 1 << 8;
    pub const IF: u32 = 1 << 9;
    pub const DF: u32 = 1 << 10;
    pub const OF: u32 = 1 << 11;
    /// The six arithmetic flags tracked lazily.
    pub const ARITH: u32 = CF | PF | AF | ZF | SF | OF;

    /// `FR` already holds the arithmetic flags in eflags bit positions.
    pub const EXPLICIT: u32 = 0;
    /// res = a + b
    pub const ADD: u32 = 1;
    /// res = a + b + c
    pub const ADC: u32 = 2;
    /// res = a - b
    pub const SUB: u32 = 3;
    /// res = a - b - c
    pub const SBB: u32 = 4;
    /// res = a op b with CF = OF = 0
    pub const LOGIC: u32 = 5;
    /// res = a + 1, c = carry flag before the instruction
    pub const INC: u32 = 6;
    /// res = a - 1, c = carry flag before the instruction
    pub const DEC: u32 = 7;
    /// res = -a
    pub const NEG: u32 = 8;
    /// res = a << b (1 <= b <= 31)
    pub const SHL: u32 = 9;
    /// res = a >>> b
    pub const SHR: u32 = 10;
    /// res = a >> b (arithmetic)
    pub const SAR: u32 = 11;
    /// Multiplication: res = low part, b = 1 if the high part is significant.
    pub const MUL: u32 = 12;
    pub const NUM_OPS: u32 = 13;

    pub const fn kind(op: u32, width_bits: u32) -> u32 {
        let code = match width_bits {
            8 => 0,
            16 => 1,
            64 => 3,
            _ => 2,
        };
        op | code << 8
    }
    pub const fn op_of(kind: u32) -> u32 {
        kind & 0xff
    }
    pub const fn width_of(kind: u32) -> u32 {
        8 << (kind >> 8 & 3)
    }
}

/// Fault codes passed to the host's `fault` import. Values are the Windows
/// exception codes the runtime will eventually raise.
pub mod fault {
    pub const ACCESS_VIOLATION: u32 = 0xC000_0005;
    pub const INTEGER_DIVIDE_BY_ZERO: u32 = 0xC000_0094;
    pub const INTEGER_OVERFLOW: u32 = 0xC000_0095;
    pub const ILLEGAL_INSTRUCTION: u32 = 0xC000_001D;
    pub const PRIVILEGED_INSTRUCTION: u32 = 0xC000_0096;
    pub const BREAKPOINT: u32 = 0x8000_0003;
    pub const SINGLE_STEP: u32 = 0x8000_0004;
    /// Not a Windows code: the translator could not translate an instruction.
    pub const UNSUPPORTED: u32 = 0xE057_0001;
    /// Not a Windows code: `int n` (software interrupt).
    pub const SOFTWARE_INTERRUPT: u32 = 0xE057_0002;
}

/// Addresses with special meaning to the dispatcher.
pub mod addr {
    /// Lowest valid guest address; everything below is the null region.
    pub const NULL_LIMIT: u32 = 0x0001_0000;
    /// Returning to this address stops the dispatcher loop (used as the
    /// return address of thread entry points).
    pub const STOP: u32 = 0xFFFF_FFF0;
    /// The stop address of 64-bit code, beyond any guest address (and exact
    /// as a JavaScript number).
    pub const STOP64: u64 = 1 << 52;
}

/// Names of the module imports every translated module expects.
pub mod imports {
    pub const MODULE: &str = "env";
    pub const MEMORY: &str = "memory";
    pub const TABLE: &str = "table";
    pub const TABLE_BASE: &str = "table_base";
    pub const LOOKUP_L1: &str = "lookup_l1";
    pub const GUEST_LIMIT: &str = "guest_limit";
    pub const CODE_BITMAP: &str = "code_bitmap";
    /// 64-bit code only: the number of 4 KB pages the first-level lookup
    /// table covers. Targets at or above go through its last entry, which
    /// points at an empty second-level table.
    pub const CODE_PAGES: &str = "code_pages";
    pub const FAULT: &str = "fault";
    pub const CODE_WRITE: &str = "code_write";
    pub const MATH: &str = "math";
}

/// Custom section listing the x86 address of each translated function, in
/// table order: a little-endian u32 count followed by that many u32s.
pub const FUNCS_SECTION: &str = "wwt.funcs";
/// The same for 64-bit code (x86-64 on a 64-bit memory): a u32 count
/// followed by that many u64 addresses.
pub const FUNCS64_SECTION: &str = "wwt.funcs64";
/// Custom section with JSON metadata about the translation.
pub const META_SECTION: &str = "wwt.meta";
/// Bumped whenever generated code changes incompatibly, to invalidate caches.
pub const ABI_VERSION: u32 = 3;

#[derive(Serialize)]
struct AbiJson {
    version: u32,
    cpu: std::collections::BTreeMap<&'static str, u32>,
    cpu64: std::collections::BTreeMap<&'static str, u32>,
    flags: std::collections::BTreeMap<&'static str, u32>,
    fault: std::collections::BTreeMap<&'static str, u32>,
    stop_address: u32,
    stop_address64: u64,
    null_limit: u32,
    funcs_section: &'static str,
    funcs64_section: &'static str,
    meta_section: &'static str,
}

/// The ABI as JSON for the JavaScript runtime.
pub fn abi_json() -> String {
    use cpu::*;
    let cpu = [
        ("GPR", GPR),
        ("EIP", EIP),
        ("FK", FK),
        ("FR", FR),
        ("FA", FA),
        ("FB", FB),
        ("FC", FC),
        ("DF", DF),
        ("EFLAGS_SYS", EFLAGS_SYS),
        ("FS_BASE", FS_BASE),
        ("GS_BASE", GS_BASE),
        ("SEG_SEL", SEG_SEL),
        ("FPU_TOP", FPU_TOP),
        ("FPU_CW", FPU_CW),
        ("FPU_SW", FPU_SW),
        ("FPU_TAG", FPU_TAG),
        ("FPU_ST", FPU_ST),
        ("MMX", MMX),
        ("XMM", XMM),
        ("MXCSR", MXCSR),
        ("FAULT_CODE", FAULT_CODE),
        ("FAULT_ADDR", FAULT_ADDR),
        ("SCRATCH", SCRATCH),
        ("SIZE", SIZE),
    ]
    .into_iter()
    .collect();
    let cpu64 = [
        ("GPR", cpu64::GPR),
        ("FR", cpu64::FR),
        ("FA", cpu64::FA),
        ("FB", cpu64::FB),
        ("FC", cpu64::FC),
        ("FS_BASE", cpu64::FS_BASE),
        ("GS_BASE", cpu64::GS_BASE),
        ("XMM8", cpu64::XMM8),
        ("RIP", cpu64::RIP),
        ("SIZE", cpu64::SIZE),
    ]
    .into_iter()
    .collect();
    let fl = [
        ("CF", flags::CF),
        ("PF", flags::PF),
        ("AF", flags::AF),
        ("ZF", flags::ZF),
        ("SF", flags::SF),
        ("TF", flags::TF),
        ("IF", flags::IF),
        ("DF", flags::DF),
        ("OF", flags::OF),
        ("EXPLICIT", flags::EXPLICIT),
    ]
    .into_iter()
    .collect();
    let fault = [
        ("ACCESS_VIOLATION", fault::ACCESS_VIOLATION),
        ("INTEGER_DIVIDE_BY_ZERO", fault::INTEGER_DIVIDE_BY_ZERO),
        ("INTEGER_OVERFLOW", fault::INTEGER_OVERFLOW),
        ("ILLEGAL_INSTRUCTION", fault::ILLEGAL_INSTRUCTION),
        ("PRIVILEGED_INSTRUCTION", fault::PRIVILEGED_INSTRUCTION),
        ("BREAKPOINT", fault::BREAKPOINT),
        ("SINGLE_STEP", fault::SINGLE_STEP),
        ("UNSUPPORTED", fault::UNSUPPORTED),
        ("SOFTWARE_INTERRUPT", fault::SOFTWARE_INTERRUPT),
    ]
    .into_iter()
    .collect();
    serde_json::to_string_pretty(&AbiJson {
        version: ABI_VERSION,
        cpu,
        cpu64,
        flags: fl,
        fault,
        stop_address: addr::STOP,
        stop_address64: addr::STOP64,
        null_limit: addr::NULL_LIMIT,
        funcs_section: FUNCS_SECTION,
        funcs64_section: FUNCS64_SECTION,
        meta_section: META_SECTION,
    })
    .unwrap()
}
