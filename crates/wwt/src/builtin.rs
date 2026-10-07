//! Built-in bodies for well-known library functions.
//!
//! Programs spend much of their time in the C runtime's memory functions,
//! and translating `memmove` or `memset` instruction by instruction gives a
//! byte or word loop. WebAssembly has `memory.copy` and `memory.fill`, which
//! engines run at native speed. Functions exported under these names (by
//! Wine's msvcrt, ucrtbase and ntdll, or by any other DLL) get a body that
//! reads the arguments from the stack and does one `MemCopy` or `MemFill`,
//! with the same bounds and code-write checks as `rep movs`.
//!
//! Only exported names are matched: a C function in an image's own symbol
//! table is named with a leading underscore (`_memcpy`), so an application's
//! private `memcpy` keeps its translated body.

use crate::ir::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    /// `void *memmove(void *dst, const void *src, size_t n)`, cdecl; also
    /// `memcpy` (overlap is allowed to behave like memmove).
    MemMove,
    /// `void *memset(void *dst, int c, size_t n)`, cdecl.
    MemSet,
    /// `void RtlMoveMemory(void *dst, const void *src, SIZE_T n)`, stdcall.
    RtlMoveMemory,
    /// `void RtlFillMemory(void *dst, SIZE_T n, BYTE fill)`, stdcall.
    RtlFillMemory,
    /// `void RtlZeroMemory(void *dst, SIZE_T n)`, stdcall.
    RtlZeroMemory,
}

impl Builtin {
    /// The built-in for an exported function name.
    pub fn by_name(name: &str) -> Option<Builtin> {
        Some(match name {
            "memmove" | "memcpy" => Builtin::MemMove,
            "memset" => Builtin::MemSet,
            "RtlMoveMemory" => Builtin::RtlMoveMemory,
            "RtlFillMemory" => Builtin::RtlFillMemory,
            "RtlZeroMemory" => Builtin::RtlZeroMemory,
            _ => return None,
        })
    }

    /// Number of 4-byte arguments, and whether the callee pops them.
    fn args(self) -> (u32, bool) {
        match self {
            Builtin::MemMove | Builtin::MemSet => (3, false),
            Builtin::RtlMoveMemory | Builtin::RtlFillMemory => (3, true),
            Builtin::RtlZeroMemory => (2, true),
        }
    }
}

fn stack(offset: u32) -> Mem {
    Mem {
        size: 4,
        signed: false,
        space: Space::Trusted,
        offset,
        atomic: false,
    }
}

/// The IR of `b` as a function at `entry`.
pub fn body(entry: u32, b: Builtin) -> Function {
    let mut f = Function::new(entry);
    let b0 = f.new_block(entry);
    let copy = f.new_block(entry);
    let done = f.new_block(entry);
    let (nargs, pops) = b.args();
    let mut args = vec![];
    for i in 0..nargs {
        let v = f.new_vreg(Ty::I32);
        f.blocks[b0 as usize].insts.push(Inst {
            dst: Some(v),
            op: Op::Load {
                addr: ESP,
                mem: stack(4 + 4 * i),
            },
            eip: entry,
        });
        args.push(v);
    }
    let (dst, len) = match b {
        Builtin::MemMove | Builtin::MemSet | Builtin::RtlMoveMemory => (args[0], args[2]),
        Builtin::RtlFillMemory | Builtin::RtlZeroMemory => (args[0], args[1]),
    };
    // A zero length touches no memory, whatever the pointers are.
    let z = f.new_vreg(Ty::I32);
    f.blocks[b0 as usize].insts.push(Inst {
        dst: Some(z),
        op: Op::Un(UnOp::I32Eqz, len),
        eip: entry,
    });
    f.blocks[b0 as usize].term = Term::Branch {
        cond: z,
        t: done,
        f: copy,
    };
    let op = match b {
        Builtin::MemMove | Builtin::RtlMoveMemory => Op::MemCopy {
            dst,
            src: args[1],
            len,
        },
        Builtin::MemSet => Op::MemFill {
            dst,
            val: args[1],
            len,
        },
        Builtin::RtlFillMemory => Op::MemFill {
            dst,
            val: args[2],
            len,
        },
        Builtin::RtlZeroMemory => {
            let zero = f.new_vreg(Ty::I32);
            f.blocks[copy as usize].insts.push(Inst {
                dst: Some(zero),
                op: Op::Const(0),
                eip: entry,
            });
            Op::MemFill {
                dst,
                val: zero,
                len,
            }
        }
    };
    f.blocks[copy as usize].insts.push(Inst {
        dst: None,
        op,
        eip: entry,
    });
    f.blocks[copy as usize].term = Term::Jump(done);
    // Return: eax = dst for the C functions; pop the return address (and the
    // arguments for stdcall).
    let blk = &mut f.blocks[done as usize].insts;
    if matches!(b, Builtin::MemMove | Builtin::MemSet) {
        blk.push(Inst {
            dst: Some(EAX),
            op: Op::Copy(dst),
            eip: entry,
        });
    }
    let ra = f.new_vreg(Ty::I32);
    let k = f.new_vreg(Ty::I32);
    let sp = f.new_vreg(Ty::I32);
    let blk = &mut f.blocks[done as usize];
    blk.insts.push(Inst {
        dst: Some(ra),
        op: Op::Load {
            addr: ESP,
            mem: stack(0),
        },
        eip: entry,
    });
    blk.insts.push(Inst {
        dst: Some(k),
        op: Op::Const((4 + if pops { 4 * nargs } else { 0 }) as u64),
        eip: entry,
    });
    blk.insts.push(Inst {
        dst: Some(sp),
        op: Op::Bin(BinOp::I32Add, ESP, k),
        eip: entry,
    });
    blk.insts.push(Inst {
        dst: Some(ESP),
        op: Op::Copy(sp),
        eip: entry,
    });
    blk.term = Term::Ret(ra);
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(Builtin::by_name("memcpy"), Some(Builtin::MemMove));
        assert_eq!(Builtin::by_name("_memcpy"), None);
        assert_eq!(
            Builtin::by_name("RtlZeroMemory"),
            Some(Builtin::RtlZeroMemory)
        );
    }

    #[test]
    fn memmove_returns_dst_and_pops_nothing() {
        let f = body(0x1000, Builtin::MemMove);
        assert_eq!(f.blocks.len(), 3);
        assert!(f.blocks[1]
            .insts
            .iter()
            .any(|i| matches!(i.op, Op::MemCopy { .. })));
        assert!(f.blocks[2].insts.iter().any(|i| i.dst == Some(EAX)));
        assert!(matches!(f.blocks[2].term, Term::Ret(_)));
    }
}
