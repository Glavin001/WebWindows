//! Fixed addresses shared with `tools/oracle/oracle.c`.

/// Data window holding memory operands and the stack.
pub const MEM_BASE: u32 = 0x0020_0000;
pub const MEM_SIZE: usize = 512;
/// Address of the instruction under test.
pub const INS: u32 = 0x0030_0100;
/// Branch target (marker 1).
pub const TGT: u32 = INS + 0x40;
/// Return target placed on the stack for `ret` (marker 2).
pub const RET_TGT: u32 = INS + 0x60;
/// Initial esp: near the top of the window.
pub const STACK: u32 = MEM_BASE + 0x1c0;

/// Where the translated test runs put the CPU struct and runtime tables.
pub const NATIVE_BASE: u32 = 0x0100_0000;
pub const L1: u32 = NATIVE_BASE;
pub const ZERO_L2: u32 = NATIVE_BASE + 0x40_0000;
pub const CODE_BITMAP: u32 = ZERO_L2 + 0x4000;
pub const CPU: u32 = CODE_BITMAP + 0x2_0000;
pub const MEMORY_PAGES: u64 = (CPU as u64 + 0x1_0000) / 65536 + 1;
