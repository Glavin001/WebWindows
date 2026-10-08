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
//!
//! Native built-ins replace a function with an implementation the host
//! provides (`Term::Native`): ntdll's heap ([`NATIVE_HEAP`], see
//! `crates/wwt-heap`) when translating with `Config::native_heap`. Others
//! are tried first and may decline, leaving the call to the translated
//! body (`Term::NativeTry`): string and locale functions ([`NATIVE_TRY`],
//! see `crates/wwt-strings`) with `Config::native_strings`.

use crate::ir::*;

/// ntdll's heap functions, implemented natively as a set: every function
/// that reads or writes a heap's internals, so that a heap handle is never
/// seen by both implementations. `_heap_thread_detach` is ntdll's internal
/// hook (found by its COFF symbol) that walks the heaps at thread exit.
pub const NATIVE_HEAP: &[&str] = &[
    "RtlCreateHeap",
    "RtlDestroyHeap",
    "RtlAllocateHeap",
    "RtlFreeHeap",
    "RtlReAllocateHeap",
    "RtlSizeHeap",
    "RtlValidateHeap",
    "RtlLockHeap",
    "RtlUnlockHeap",
    "RtlCompactHeap",
    "RtlWalkHeap",
    "RtlGetProcessHeaps",
    "RtlQueryHeapInformation",
    "RtlSetHeapInformation",
    "RtlGetUserInfoHeap",
    "RtlSetUserValueHeap",
    "RtlSetUserFlagsHeap",
    "_heap_thread_detach",
];

/// The native heap function of this name, as the import name.
pub fn native_heap(name: &str) -> Option<&'static str> {
    NATIVE_HEAP.iter().copied().find(|n| *n == name)
}

/// A function whose whole body is the native implementation `name`.
pub fn native_body(entry: u32, name: &'static str) -> Function {
    let mut f = Function::new(entry);
    let b = f.new_block(entry);
    f.blocks[b as usize].term = Term::Native(name);
    f
}

/// A native implementation tried before a function's translated body. It
/// declines whatever it does not handle exactly as the x86 code would (bad
/// pointers, which must fault where the x86 code faults; flags or locales it
/// does not implement; stores to pages with translated code), and the
/// translated body runs instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeTry {
    /// The DLLs (lower case) whose export of this name it implements: Wine
    /// builds several from one source, with different behavior.
    pub dlls: &'static [&'static str],
    /// The export, or the COFF symbol (`_name`) of a function that is not
    /// exported (found when the translator discovers it as a function).
    pub name: &'static str,
    /// The import that implements it.
    pub import: &'static str,
    /// COFF symbols of static data it reads, passed as constant arguments;
    /// without them in the image, the function keeps its translated body.
    pub symbols: &'static [&'static str],
}

const CRT: &[&str] = &["ntdll.dll", "msvcrt.dll", "ucrtbase.dll"];

/// A C runtime function: its import is named `crt_<name>` (the module that
/// implements it has C runtime functions of its own under the plain names).
macro_rules! crt {
    ($name:literal) => {
        NativeTry {
            dlls: CRT,
            name: $name,
            import: concat!("crt_", $name),
            symbols: &[],
        }
    };
}

/// String and locale functions with native implementations
/// (`crates/wwt-strings`): kernelbase's `CompareStringEx` with the locale
/// tables it reads (`sort`, `current_locale_sort` in Wine's `locale.c`);
/// C string functions that behave the same in ntdll, msvcrt and ucrtbase;
/// and the thread-local storage lookups on their paths: `TlsGetValue`
/// (kernel32's import stub too) and the C runtime's per-thread data, which
/// its locale-aware functions fetch on every call.
pub const NATIVE_TRY: &[NativeTry] = &[
    NativeTry {
        dlls: &["kernelbase.dll"],
        name: "CompareStringEx",
        import: "CompareStringEx",
        symbols: &["_sort", "_current_locale_sort"],
    },
    NativeTry {
        dlls: &["kernel32.dll", "kernelbase.dll"],
        name: "TlsGetValue",
        import: "TlsGetValue",
        symbols: &[],
    },
    NativeTry {
        dlls: &["msvcrt.dll", "ucrtbase.dll"],
        name: "_msvcrt_get_thread_data",
        import: "msvcrt_get_thread_data",
        symbols: &["_msvcrt_tls_index"],
    },
    crt!("strlen"),
    crt!("wcslen"),
    crt!("memcmp"),
    crt!("strcmp"),
    crt!("strchr"),
    crt!("wcschr"),
    crt!("memchr"),
    crt!("strcspn"),
];

/// `f` with the native implementation `name` tried first: a new entry block
/// calls it with `args`, and the translated body runs when it declines.
pub fn with_native_try(mut f: Function, name: &'static str, args: Vec<u32>) -> Function {
    // Block 0 must stay the entry: move its code to a new block.
    let body = f.blocks.len() as BlockId;
    let b0 = f.blocks[0].clone();
    let addr = b0.addr;
    f.blocks.push(b0);
    for b in f.blocks.iter_mut() {
        for s in b.term.successors_mut() {
            if *s == 0 {
                *s = body;
            }
        }
    }
    f.blocks[0] = Block {
        addr,
        insts: vec![],
        term: Term::NativeTry {
            name,
            args,
            fallback: body,
        },
    };
    f
}

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
    fn native_heap_names() {
        assert_eq!(native_heap("RtlAllocateHeap"), Some("RtlAllocateHeap"));
        assert_eq!(native_heap("RtlAllocateHeap@12"), None);
        let f = native_body(0x1000, "RtlFreeHeap");
        assert_eq!(f.blocks.len(), 1);
        assert_eq!(f.blocks[0].term, Term::Native("RtlFreeHeap"));
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
