//! ntdll's heap (`RtlAllocateHeap` and friends) as native WebAssembly.
//!
//! Translated Wine runs ntdll's own heap as translated x86 code: every
//! `HeapAlloc` walks through handle checks, the LFH front end, critical
//! sections and free lists, with the register traffic of each translated
//! call. This module replaces the whole set of heap functions
//! (`wwt::builtin::NATIVE_HEAP`): when ntdll is translated with
//! `--native-heap`, each of them becomes a tail call into the export of the
//! same name here. An export receives the CPU state, reads its stdcall
//! arguments from the guest stack, sets eax, pops the return address and
//! arguments, and returns the address to continue at, like a translated
//! function.
//!
//! The heap is a segregated-fit allocator, single-threaded as the guest is:
//!
//! * Blocks have an 8-byte header before the pointer (requested size; the
//!   block's distance in 64 KB units from the region it was carved from;
//!   size class; state and user flags), so pointers are 8-aligned as on
//!   32-bit Windows, and `RtlSizeHeap` returns the exact requested size.
//! * Requests up to 512 KB round up to one of 75 size classes (8-byte steps
//!   to 256 bytes, then four per power of two). A class's free blocks form
//!   a LIFO list; new blocks are carved from the end of the newest segment.
//!   Blocks are never split or merged, so allocation and free are a few
//!   loads and stores.
//! * Segments come from the runtime's virtual memory manager
//!   (`heap_vm_alloc`), 1 MB first and doubling to 16 MB, as Wine grows its
//!   heaps; larger requests get a region each.
//! * A heap handle is the base of its first segment, which starts with the
//!   Windows-compatible header (`0xffeeffee` at 8, flags at 0x40, force
//!   flags at 0x44) and the heap's own state. Every region's header names
//!   its heap, so frees and size queries check that a block belongs to the
//!   heap they were given.
//!
//! Status and last-error behavior follow Wine's `dlls/ntdll/heap.c`.

#![cfg_attr(target_arch = "wasm32", no_std)]

mod mem;
use mem::*;

#[cfg(target_arch = "wasm32")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

// ---- The x86 side --------------------------------------------------------------

/// CPU struct offsets (`wwt::abi::cpu`).
const CPU_EAX: u32 = 0;
const CPU_ESP: u32 = 16;
const CPU_FS_BASE: u32 = 64;
/// TEB fields (i386).
const TEB_LAST_ERROR: u32 = 0x34;
const TEB_LAST_STATUS: u32 = 0xbf4;

/// Process-wide values, set by `wwt_heap_init` and `RtlCreateHeap`.
const CTX_PROCESS_HEAP: u32 = CTX;
/// ntdll's `RtlRaiseStatus`, for HEAP_GENERATE_EXCEPTIONS.
const CTX_RAISE_STATUS: u32 = CTX + 4;
/// The guest limit: no guest pointer is at or above it.
const CTX_LIMIT: u32 = CTX + 8;
/// The newest private heap (heaps other than the process heap).
const CTX_HEAPS: u32 = CTX + 12;

const GUEST_LO: u32 = 0x10000;

const STATUS_NO_MORE_ENTRIES: u32 = 0x8000_001a;
const STATUS_UNSUCCESSFUL: u32 = 0xc000_0001;
const STATUS_INVALID_INFO_CLASS: u32 = 0xc000_0003;
const STATUS_ACCESS_VIOLATION: u32 = 0xc000_0005;
const STATUS_INVALID_HANDLE: u32 = 0xc000_0008;
const STATUS_INVALID_PARAMETER: u32 = 0xc000_000d;
const STATUS_NO_MEMORY: u32 = 0xc000_0017;
const STATUS_BUFFER_TOO_SMALL: u32 = 0xc000_0023;

const HEAP_NO_SERIALIZE: u32 = 0x1;
const HEAP_GROWABLE: u32 = 0x2;
const HEAP_GENERATE_EXCEPTIONS: u32 = 0x4;
const HEAP_ZERO_MEMORY: u32 = 0x8;
const HEAP_REALLOC_IN_PLACE_ONLY: u32 = 0x10;
const HEAP_TAIL_CHECKING_ENABLED: u32 = 0x20;
const HEAP_FREE_CHECKING_ENABLED: u32 = 0x40;
const HEAP_DISABLE_COALESCE_ON_FREE: u32 = 0x80;
const HEAP_ADD_USER_INFO: u32 = 0x100;
const HEAP_USER_FLAGS_MASK: u32 = 0xf00;
const HEAP_PRIVATE: u32 = 0x1000;
const HEAP_CREATE_ENABLE_EXECUTE: u32 = 0x40000;
const HEAP_SHARED: u32 = 0x0400_0000;
const HEAP_CHECKING_ENABLED: u32 = 0x8000_0000;
/// Per-call flags that add to the heap's own (Wine's `heap_get_flags`).
const CALL_FLAGS: u32 = HEAP_GENERATE_EXCEPTIONS
    | HEAP_NO_SERIALIZE
    | HEAP_ZERO_MEMORY
    | HEAP_REALLOC_IN_PLACE_ONLY
    | HEAP_CHECKING_ENABLED
    | HEAP_USER_FLAGS_MASK;

const PAGE_READWRITE: u32 = 0x04;
const PAGE_EXECUTE_READWRITE: u32 = 0x40;

// ---- Layout ------------------------------------------------------------------------

/// Every region the heap gets from virtual memory (its first segment, which
/// holds the heap itself, later segments and large blocks) starts with its
/// size and its heap, where the Windows heap header has unused fields.
const C_SIZE: u32 = 0x00;
const C_HEAP: u32 = 0x04;
/// Segments: the next older one, and the end of the carved blocks (the
/// current segment's is the bump pointer).
const S_NEXT: u32 = 0x10;
const S_END: u32 = 0x14;
/// Large blocks: the heap's list, and the user value (HEAP_ADD_USER_INFO).
const L_NEXT: u32 = 0x08;
const L_PREV: u32 = 0x0c;
const L_VALUE: u32 = 0x10;

/// The heap, at its handle.
const H_FFEEFFEE: u32 = 0x08;
const H_AUTO_FLAGS: u32 = 0x0c;
const H_FLAGS: u32 = 0x40;
const H_FORCE_FLAGS: u32 = 0x44;
const H_MAGIC: u32 = 0x48;
const H_COMPAT: u32 = 0x4c;
const H_NEXT_HEAP: u32 = 0x50;
const H_PREV_HEAP: u32 = 0x54;
const H_BUMP: u32 = 0x58;
const H_BUMP_END: u32 = 0x5c;
const H_CUR_SEG: u32 = 0x60;
/// Newest segment; segments link through `S_NEXT` down to the heap's own.
const H_SEGS: u32 = 0x64;
const H_LARGE: u32 = 0x68;
const H_GROW: u32 = 0x6c;
/// Blocks carved so far, until the heap reports itself as LFH.
const H_CARVED: u32 = 0x70;
/// Free list heads, one per size class.
const H_FREE: u32 = 0x80;
const HEAP_SIZE: u32 = 0x200;
const MAGIC: u32 = u32::from_le_bytes(*b"HEAP");

/// First block of a segment other than the heap's own.
const SEG_DATA: u32 = 0x40;
/// Block header of a large block: its pointer is 32 bytes into the region,
/// as on Windows.
const LARGE_HDR: u32 = 0x18;

const REGION: u32 = 0x10000;
const FIRST_GROW: u32 = 0x10_0000;
/// Segments double up to just under 16 MB, as on Windows (0xfd0000).
const MAX_GROW: u32 = 0xfd_0000;
/// No request this large can succeed in a 32-bit guest.
const MAX_REQUEST: u32 = 0x7ff0_0000;

/// Block header, before the pointer: requested size (u32), region distance
/// in 64 KB units (u16), size class (u8), state and user flags (u8).
const HDR: u32 = 8;
const STATE_MASK: u32 = 0x87;
const USED: u32 = 0x85;
const FREE: u32 = 0x02;
/// HEAP_ADD_USER_INFO: the block holds a user value (Wine's
/// BLOCK_FLAG_USER_INFO), and the user flags 0x200..0x800 as 0x10..0x40.
const USER_INFO: u32 = 0x08;
const USER_MASK: u32 = 0x78;
const LARGE: u32 = 0xff;
/// A block of exactly the size it needs (see [`exact_size`]): what is left
/// of a heap that cannot grow, when no size class fits in it.
const EXACT: u32 = 0xfe;

const NCLASS: u32 = 75;
const MAX_SMALL: u32 = 0x8_0000;

/// The size class of a block of `total` bytes (a multiple of 8, from 16 to
/// `MAX_SMALL`): the smallest class at least that large.
#[inline(always)]
fn class_of(total: u32) -> u32 {
    if total <= 256 {
        total / 8 - 2
    } else {
        // 2^k < total <= 2^(k+1); four classes per power of two.
        let k = 31 - (total - 1).leading_zeros();
        let step = 1 << (k - 2);
        let sub = (total - (1 << k)).div_ceil(step);
        31 + (k - 8) * 4 + sub - 1
    }
}

/// Bytes per block (header included) of class `c`.
#[inline(always)]
fn class_size(c: u32) -> u32 {
    if c < 31 {
        (c + 2) * 8
    } else {
        let k = 8 + (c - 31) / 4;
        (1 << k) + ((c - 31) % 4 + 1) * (1 << (k - 2))
    }
}

fn round_region(n: u32) -> u32 {
    (n + REGION - 1) & !(REGION - 1)
}

// ---- Heaps and blocks ---------------------------------------------------------------------

fn limit() -> u32 {
    ld(CTX_LIMIT)
}

/// Whether `h` is a live heap.
#[inline(always)]
fn heap_ok(h: u32) -> bool {
    h & (REGION - 1) == 0 && h >= GUEST_LO && h < limit() && ld(h + H_MAGIC) == MAGIC
}

/// The heap's flags with a call's flags added.
#[inline(always)]
fn eff_flags(h: u32, flags: u32) -> u32 {
    let mut f = flags;
    if f & (HEAP_TAIL_CHECKING_ENABLED | HEAP_FREE_CHECKING_ENABLED) != 0 {
        f |= HEAP_CHECKING_ENABLED;
    }
    ld(h + H_FLAGS) | f & CALL_FLAGS
}

fn protection(h: u32) -> u32 {
    if ld(h + H_FLAGS) & HEAP_CREATE_ENABLE_EXECUTE != 0 {
        PAGE_EXECUTE_READWRITE
    } else {
        PAGE_READWRITE
    }
}

/// The header of `p` if it is a block of heap `h` in use, else 0. Checks
/// the header and that the region it names belongs to `h`.
#[inline(always)]
fn block(h: u32, p: u32) -> u32 {
    if p & 7 != 0 || p < GUEST_LO + SEG_DATA + HDR || p >= limit() {
        return 0;
    }
    let hdr = p - HDR;
    let w = ld(hdr + 4);
    if w >> 24 & STATE_MASK != USED {
        return 0;
    }
    let base = (hdr & !(REGION - 1)).wrapping_sub((w & 0xffff) << 16);
    if base < GUEST_LO || base > hdr || ld(base + C_HEAP) != h {
        return 0;
    }
    let c = w >> 16 & 0xff;
    if c == LARGE {
        if hdr != base + LARGE_HDR {
            return 0;
        }
    } else if c >= NCLASS && c != EXACT {
        return 0;
    }
    hdr
}

fn hdr_class(hdr: u32) -> u32 {
    ld8(hdr + 6)
}

fn hdr_bits(hdr: u32) -> u32 {
    ld8(hdr + 7)
}

/// The region a block was carved from.
fn hdr_base(hdr: u32) -> u32 {
    (hdr & !(REGION - 1)) - (ld16(hdr + 4) << 16)
}

/// Bytes a block of `n` bytes needs: header, at least 8 bytes of data, and
/// `ui` bytes for a user value.
#[inline(always)]
fn exact_size(n: u32, ui: u32) -> u32 {
    (n.max(8) + HDR + ui + 7) & !7
}

/// Bytes the block at `hdr` takes in its segment.
fn block_size(hdr: u32) -> u32 {
    match hdr_class(hdr) {
        EXACT => exact_size(ld(hdr), (hdr_bits(hdr) & USER_INFO) >> 1),
        c => class_size(c),
    }
}

/// Bytes a block can hold, besides its user value if it has one.
fn capacity(hdr: u32, user_info: bool) -> u32 {
    let ui = if user_info { 4 } else { 0 };
    match hdr_class(hdr) {
        LARGE => ld(hdr - LARGE_HDR + C_SIZE) - LARGE_HDR - HDR,
        _ => block_size(hdr) - HDR - ui,
    }
}

/// Where a block keeps its user value (HEAP_ADD_USER_INFO).
fn user_value_addr(hdr: u32) -> u32 {
    match hdr_class(hdr) {
        LARGE => hdr - LARGE_HDR + L_VALUE,
        _ => hdr + block_size(hdr) - 4,
    }
}

fn data_start(h: u32, seg: u32) -> u32 {
    if seg == h {
        h + HEAP_SIZE
    } else {
        seg + SEG_DATA
    }
}

fn carved_end(h: u32, seg: u32) -> u32 {
    if seg == ld(h + H_CUR_SEG) {
        ld(h + H_BUMP)
    } else {
        ld(seg + S_END)
    }
}

/// Pushes a free block of class `c` at `hdr` on its list.
fn push_free(h: u32, hdr: u32, c: u32) {
    let head = h + H_FREE + 4 * c;
    st(hdr + HDR, ld(head));
    st(head, hdr + HDR);
}

/// Turns `[b, end)` in segment `seg` into free blocks, largest classes
/// first; returns where they end (`end` unless 8 bytes were all there was).
fn carve_free(h: u32, seg: u32, mut b: u32, end: u32) -> u32 {
    while end - b >= 16 {
        let rest = (end - b).min(MAX_SMALL);
        let mut c = class_of(rest);
        if class_size(c) > rest {
            c -= 1;
        }
        // Never leave 8 bytes, which hold no block.
        if end - b - class_size(c) == 8 {
            c -= 1;
        }
        st(b, 0);
        st(
            b + 4,
            ((b & !(REGION - 1)) - seg) >> 16 | c << 16 | FREE << 24,
        );
        push_free(h, b, c);
        b += class_size(c);
    }
    b
}

/// Turns the rest of the current segment into free blocks.
fn salvage(h: u32) {
    let b = carve_free(h, ld(h + H_CUR_SEG), ld(h + H_BUMP), ld(h + H_BUMP_END));
    st(h + H_BUMP, b);
}

/// Adds a segment with room for a block of `need` bytes.
fn grow(h: u32, need: u32) -> bool {
    if ld(h + H_FLAGS) & HEAP_GROWABLE == 0 {
        return false;
    }
    let min = round_region(SEG_DATA + need);
    let mut size = ld(h + H_GROW).max(min);
    let mut seg = vm_alloc(size, protection(h));
    // Running out of address space: try smaller segments.
    while seg == 0 && size > min {
        size = round_region(size / 2).max(min);
        seg = vm_alloc(size, protection(h));
    }
    if seg == 0 {
        return false;
    }
    salvage(h);
    let cur = ld(h + H_CUR_SEG);
    st(cur + S_END, ld(h + H_BUMP));
    st(h + H_GROW, (ld(h + H_GROW) * 2).min(MAX_GROW));
    st(seg + C_SIZE, size);
    st(seg + C_HEAP, h);
    st(seg + S_NEXT, ld(h + H_SEGS));
    st(h + H_SEGS, seg);
    st(h + H_CUR_SEG, seg);
    st(h + H_BUMP, seg + SEG_DATA);
    st(h + H_BUMP_END, seg + size);
    true
}

/// Allocates `n` bytes with flags `eff` (heap and call flags); 0 when out
/// of memory.
#[inline(always)]
fn alloc(h: u32, eff: u32, n: u32) -> u32 {
    if n > MAX_REQUEST {
        return 0;
    }
    let ui = if eff & HEAP_ADD_USER_INFO != 0 { 4 } else { 0 };
    let total = exact_size(n, ui);
    let bits = USED | eff >> 5 & USER_MASK;
    let p = if total <= MAX_SMALL {
        let c = class_of(total);
        let head = h + H_FREE + 4 * c;
        let f = ld(head);
        if f != 0 {
            st(head, ld(f));
            st(f - HDR, n);
            st8(f - 1, bits);
            f
        } else {
            let cs = class_size(c);
            let b = ld(h + H_BUMP);
            if cs > ld(h + H_BUMP_END) - b {
                let p = alloc_slow(h, c, total, n, bits);
                if p == 0 {
                    return 0;
                }
                p
            } else {
                carve(h, b, cs, c, n, bits)
            }
        }
    } else {
        let p = alloc_large(h, n, bits);
        if p == 0 {
            return 0;
        }
        p
    };
    if ui != 0 {
        st(user_value_addr(p - HDR), 0);
    }
    if eff & HEAP_ZERO_MEMORY != 0 {
        fill(p, 0, (n + 3) & !3);
    }
    p
}

/// A new block of `size` bytes and class `c` at the bump pointer `b`.
#[inline(always)]
fn carve(h: u32, b: u32, size: u32, c: u32, n: u32, bits: u32) -> u32 {
    st(h + H_BUMP, b + size);
    st(b, n);
    st(
        b + 4,
        ((b & !(REGION - 1)) - ld(h + H_CUR_SEG)) >> 16 | c << 16 | bits << 24,
    );
    // Wine turns on its low-fragmentation front end once a growable heap
    // has made enough blocks; HeapQueryInformation tells. Approximately so.
    let carved = ld(h + H_CARVED) + 1;
    st(h + H_CARVED, carved);
    let lfh_ok = HEAP_GROWABLE | HEAP_NO_SERIALIZE;
    if carved == 0x11 && ld(h + H_COMPAT) == 0 && ld(h + H_FLAGS) & lfh_ok == HEAP_GROWABLE {
        st(h + H_COMPAT, 2);
    }
    b + HDR
}

/// The current segment has no room for a block of class `c`: a new segment.
/// When the heap cannot grow: a block of just `total` bytes from what is
/// left of it, or part of a larger free block.
#[inline(never)]
fn alloc_slow(h: u32, c: u32, total: u32, n: u32, bits: u32) -> u32 {
    let cs = class_size(c);
    if grow(h, cs) {
        return carve(h, ld(h + H_BUMP), cs, c, n, bits);
    }
    let b = ld(h + H_BUMP);
    if total <= ld(h + H_BUMP_END) - b {
        return carve(h, b, total, EXACT, n, bits);
    }
    for big in c + 1..NCLASS {
        let head = h + H_FREE + 4 * big;
        let f = ld(head);
        if f == 0 {
            continue;
        }
        st(head, ld(f));
        let b = f - HDR;
        let size = class_size(big);
        // Split off the rest, unless that would leave 8 bytes.
        let c = if size - cs == 8 {
            big
        } else {
            carve_free(h, hdr_base(b), b + cs, b + size);
            c
        };
        st(b, n);
        st8(b + 6, c);
        st8(b + 7, bits);
        return f;
    }
    0
}

/// A block in a region of its own.
#[inline(never)]
fn alloc_large(h: u32, n: u32, bits: u32) -> u32 {
    if ld(h + H_FLAGS) & HEAP_GROWABLE == 0 {
        return 0;
    }
    let size = round_region(LARGE_HDR + HDR + n);
    let r = vm_alloc(size, protection(h));
    if r == 0 {
        return 0;
    }
    st(r + C_SIZE, size);
    st(r + C_HEAP, h);
    st(r + L_VALUE, 0);
    let first = ld(h + H_LARGE);
    st(r + L_NEXT, first);
    st(r + L_PREV, 0);
    if first != 0 {
        st(first + L_PREV, r);
    }
    st(h + H_LARGE, r);
    let hdr = r + LARGE_HDR;
    st(hdr, n);
    st(hdr + 4, LARGE << 16 | bits << 24);
    hdr + HDR
}

/// Frees a block in use (`hdr` from [`block`]).
#[inline(always)]
fn free_block(h: u32, hdr: u32) {
    let c = hdr_class(hdr);
    if c == LARGE {
        free_large(h, hdr - LARGE_HDR);
    } else if c == EXACT {
        let size = block_size(hdr);
        carve_free(h, hdr_base(hdr), hdr, hdr + size);
    } else {
        st8(hdr + 7, FREE);
        push_free(h, hdr, c);
    }
}

#[inline(never)]
fn free_large(h: u32, r: u32) {
    let (prev, next) = (ld(r + L_PREV), ld(r + L_NEXT));
    if prev != 0 {
        st(prev + L_NEXT, next);
    } else {
        st(h + H_LARGE, next);
    }
    if next != 0 {
        st(next + L_PREV, prev);
    }
    st(r + LARGE_HDR + 4, 0);
    vm_free(r);
}

/// Resizes the block at `p`: in place when it fits (unless that would
/// waste more than half the block), else by moving it. Returns the pointer
/// and status.
fn realloc(h: u32, flags: u32, p: u32, n: u32) -> (u32, u32) {
    let eff = eff_flags(h, flags);
    if n > MAX_REQUEST {
        return (0, STATUS_NO_MEMORY);
    }
    let hdr = block(h, p);
    if hdr == 0 {
        return (0, STATUS_INVALID_PARAMETER);
    }
    let bits = hdr_bits(hdr);
    let c = hdr_class(hdr);
    let ui = bits & USER_INFO != 0 || eff & HEAP_ADD_USER_INFO != 0;
    let old = ld(hdr);
    // An exact block's size follows from its data size: it can only take
    // sizes that need as many bytes.
    let fits = if c == EXACT {
        exact_size(n, if ui { 4 } else { 0 }) == block_size(hdr)
    } else {
        n <= capacity(hdr, ui)
    };
    // A block that fits stays where it is, however much smaller it gets:
    // Windows (and Wine) shrink in place, and programs that ignore what
    // a shrinking HeapReAlloc returns keep working.
    if fits {
        st(hdr, n);
        let keep = bits & (STATE_MASK | USER_INFO);
        st8(hdr + 7, keep | eff >> 5 & USER_MASK);
        if eff & HEAP_ADD_USER_INFO != 0 {
            st(user_value_addr(hdr), 0);
        }
        if eff & HEAP_ZERO_MEMORY != 0 && n > old {
            fill(p + old, 0, ((n + 3) & !3) - old);
        }
        return (p, 0);
    }
    if flags & HEAP_REALLOC_IN_PLACE_ONLY != 0 {
        return (0, STATUS_NO_MEMORY);
    }
    let q = alloc(h, eff, n);
    if q == 0 {
        return (0, STATUS_NO_MEMORY);
    }
    copy(q, p, n.min(old));
    free_block(h, hdr);
    (q, 0)
}

/// The segment of `h` holding the block at `hdr`, or 0.
fn segment_of(h: u32, hdr: u32) -> u32 {
    let mut seg = ld(h + H_SEGS);
    while seg != 0 {
        if hdr >= data_start(h, seg) && hdr < carved_end(h, seg) {
            return seg;
        }
        seg = ld(seg + S_NEXT);
    }
    0
}

/// Whether the large block region `r` belongs to `h`.
fn is_large_of(h: u32, r: u32) -> bool {
    let mut l = ld(h + H_LARGE);
    while l != 0 {
        if l == r {
            return true;
        }
        l = ld(l + L_NEXT);
    }
    false
}

/// The thorough check of HeapValidate: a block in use of `h`, inside one
/// of its segments' carved blocks or one of its large blocks.
fn validate_ptr(h: u32, p: u32) -> bool {
    let hdr = block(h, p);
    if hdr == 0 {
        return false;
    }
    let c = hdr_class(hdr);
    if c == LARGE {
        return is_large_of(h, hdr - LARGE_HDR);
    }
    let seg = segment_of(h, hdr);
    seg != 0 && seg == hdr_base(hdr) && hdr + block_size(hdr) <= carved_end(h, seg)
}

/// HeapValidate of a whole heap: every block header, and every free list.
fn validate_heap(h: u32) -> bool {
    let mut blocks = 0u32;
    let mut seg = ld(h + H_SEGS);
    while seg != 0 {
        let (mut b, end) = (data_start(h, seg), carved_end(h, seg));
        if ld(seg + C_HEAP) != h || end > seg + ld(seg + C_SIZE) {
            return false;
        }
        while b < end {
            let c = hdr_class(b);
            let state = hdr_bits(b) & STATE_MASK;
            let class_ok = c < NCLASS || c == EXACT && state == USED;
            if !class_ok || (state != USED && state != FREE) || hdr_base(b) != seg {
                return false;
            }
            b += block_size(b);
            blocks += 1;
        }
        if b != end {
            return false;
        }
        seg = ld(seg + S_NEXT);
    }
    for c in 0..NCLASS {
        let mut p = ld(h + H_FREE + 4 * c);
        let mut n = 0;
        while p != 0 {
            let hdr = p - HDR;
            n += 1;
            if n > blocks
                || p & 7 != 0
                || p >= limit()
                || hdr_class(hdr) != c
                || hdr_bits(hdr) & STATE_MASK != FREE
                || segment_of(h, hdr) == 0
            {
                return false;
            }
            p = ld(p);
        }
    }
    let mut l = ld(h + H_LARGE);
    while l != 0 {
        if ld(l + C_HEAP) != h || hdr_class(l + LARGE_HDR) != LARGE {
            return false;
        }
        l = ld(l + L_NEXT);
    }
    true
}

fn create(flags: u32, total: u32, commit: u32) -> u32 {
    let process_heap = ld(CTX_PROCESS_HEAP);
    let mut flags = flags & !(HEAP_TAIL_CHECKING_ENABLED | HEAP_FREE_CHECKING_ENABLED);
    if process_heap != 0 {
        flags |= HEAP_PRIVATE;
    }
    if process_heap == 0 || total == 0 || flags & HEAP_SHARED != 0 {
        flags |= HEAP_GROWABLE;
    }
    let commit = (commit.min(0xffff_0000) + 0xfff) & !0xfff;
    let total = if total == 0 {
        round_region(commit + 1)
    } else {
        total
    };
    let size = round_region(total.max(commit).clamp(HEAP_SIZE + 0x1000, 0xffff_0000));
    let prot = if flags & HEAP_CREATE_ENABLE_EXECUTE != 0 {
        PAGE_EXECUTE_READWRITE
    } else {
        PAGE_READWRITE
    };
    let h = vm_alloc(size, prot);
    if h == 0 {
        return 0;
    }
    fill(h, 0, HEAP_SIZE);
    st(h + H_FFEEFFEE, 0xffee_ffee);
    st(h + H_AUTO_FLAGS, flags & HEAP_GROWABLE);
    st(h + H_FLAGS, flags & !HEAP_SHARED);
    // As Wine's heap_set_debug_flags leaves them without debugging flags.
    st(
        h + H_FORCE_FLAGS,
        flags & !(HEAP_SHARED | HEAP_DISABLE_COALESCE_ON_FREE | HEAP_GROWABLE | HEAP_PRIVATE),
    );
    st(h + H_MAGIC, MAGIC);
    st(h + C_SIZE, size);
    st(h + C_HEAP, h);
    st(h + H_SEGS, h);
    st(h + H_CUR_SEG, h);
    st(h + H_BUMP, h + HEAP_SIZE);
    st(h + H_BUMP_END, h + size);
    st(h + H_GROW, FIRST_GROW);
    if process_heap == 0 {
        st(CTX_PROCESS_HEAP, h);
    } else {
        let first = ld(CTX_HEAPS);
        st(h + H_NEXT_HEAP, first);
        if first != 0 {
            st(first + H_PREV_HEAP, h);
        }
        st(CTX_HEAPS, h);
    }
    h
}

/// Returns 0 when the heap was destroyed, else `h`.
fn destroy(h: u32) -> u32 {
    if !heap_ok(h) || h == ld(CTX_PROCESS_HEAP) {
        return h;
    }
    let (prev, next) = (ld(h + H_PREV_HEAP), ld(h + H_NEXT_HEAP));
    if prev != 0 {
        st(prev + H_NEXT_HEAP, next);
    } else {
        st(CTX_HEAPS, next);
    }
    if next != 0 {
        st(next + H_PREV_HEAP, prev);
    }
    let mut l = ld(h + H_LARGE);
    while l != 0 {
        let n = ld(l + L_NEXT);
        st(l + LARGE_HDR + 4, 0);
        vm_free(l);
        l = n;
    }
    let mut seg = ld(h + H_SEGS);
    while seg != h {
        let n = ld(seg + S_NEXT);
        st(seg + C_HEAP, 0);
        vm_free(seg);
        seg = n;
    }
    st(h + H_MAGIC, 0);
    st(h + C_HEAP, 0);
    vm_free(h);
    0
}

// ---- HeapWalk ------------------------------------------------------------------------------
//
// Entries come as Wine lists them: each segment as a region, its blocks (a
// free block's data starts after its free-list links), its unused rest as
// one free block up to the "committed" end and the rest of the region as
// an uncommitted range; then the large blocks. Segments are committed
// whole, so the committed end reported is the unused rest's first page
// boundary.

/// Wine's `struct rtl_heap_entry`.
const E_DATA: u32 = 0;
const E_SIZE: u32 = 4;
const E_OVERHEAD: u32 = 8;
const E_REGION_INDEX: u32 = 9;
const E_FLAGS: u32 = 10;
const E_COMMITTED: u32 = 12;
const E_UNCOMMITTED: u32 = 16;
const E_FIRST: u32 = 20;
const E_LAST: u32 = 24;
const ENTRY_BUSY: u32 = 0x1;
const ENTRY_REGION: u32 = 0x2;
const ENTRY_BLOCK: u32 = 0x10;
const ENTRY_UNCOMMITTED: u32 = 0x1000;
const ENTRY_COMMITTED: u32 = 0x4000;
/// A free block's entry: header and free-list links before its data.
const FREE_OVERHEAD: u32 = 2 * HDR;
/// The smallest unused rest listed as a free block: Wine's entry ends that
/// much before the committed end.
const TAIL_MIN: u32 = 3 * FREE_OVERHEAD;

fn walk_entry(e: u32, data: u32, size: u32, overhead: u32, index: u32, flags: u32) {
    st(e + E_DATA, data);
    st(e + E_SIZE, size);
    st8(e + E_OVERHEAD, overhead);
    st8(e + E_REGION_INDEX, index);
    st16(e + E_FLAGS, flags);
}

fn seg_end(seg: u32) -> u32 {
    seg + ld(seg + C_SIZE)
}

/// The end of what HeapWalk reports as committed in `seg`.
fn commit_end(h: u32, seg: u32) -> u32 {
    let end = carved_end(h, seg);
    if seg_end(seg) - end < TAIL_MIN {
        return seg_end(seg);
    }
    ((end + TAIL_MIN + 0xfff) & !0xfff).min(seg_end(seg))
}

fn walk_region(h: u32, e: u32, seg: u32) -> u32 {
    let first = data_start(h, seg);
    let committed = commit_end(h, seg) - seg;
    walk_entry(e, seg, first - seg, 0, 0, ENTRY_REGION);
    st(e + E_COMMITTED, committed);
    st(e + E_UNCOMMITTED, ld(seg + C_SIZE) - committed);
    st(e + E_FIRST, first);
    st(e + E_LAST, seg_end(seg));
    0
}

/// The entry for the block at `b` in `seg`, or (past the carved blocks)
/// what follows them.
fn walk_from(h: u32, e: u32, seg: u32, b: u32) -> u32 {
    let end = carved_end(h, seg);
    if b < end {
        let size = block_size(b);
        if hdr_bits(b) & STATE_MASK == USED {
            // Overhead as Wine counts it: the header and the unused tail.
            let busy = ENTRY_BUSY | ENTRY_BLOCK | ENTRY_COMMITTED;
            walk_entry(e, b + HDR, ld(b), (size - ld(b)).min(0xff), 0, busy);
        } else {
            walk_entry(
                e,
                b + FREE_OVERHEAD,
                size - FREE_OVERHEAD,
                FREE_OVERHEAD,
                0,
                0,
            );
        }
        return 0;
    }
    let ce = commit_end(h, seg);
    if b == end && ce - end >= TAIL_MIN {
        let data = end + FREE_OVERHEAD;
        walk_entry(e, data, ce - 2 * FREE_OVERHEAD - data, FREE_OVERHEAD, 0, 0);
        return 0;
    }
    walk_uncommitted(h, e, seg)
}

/// The uncommitted range of `seg`, or what follows it.
fn walk_uncommitted(h: u32, e: u32, seg: u32) -> u32 {
    let ce = commit_end(h, seg);
    if ce < seg_end(seg) {
        walk_entry(e, ce, seg_end(seg) - ce, 0, 0, ENTRY_UNCOMMITTED);
        return 0;
    }
    let next = ld(seg + S_NEXT);
    if next != 0 {
        return walk_region(h, e, next);
    }
    walk_large(e, ld(h + H_LARGE))
}

fn walk_large(e: u32, r: u32) -> u32 {
    if r == 0 {
        return STATUS_NO_MORE_ENTRIES;
    }
    let hdr = r + LARGE_HDR;
    let busy = ENTRY_BUSY | ENTRY_BLOCK | ENTRY_COMMITTED;
    walk_entry(e, hdr + HDR, ld(hdr), 0, 64, busy);
    0
}

fn walk(h: u32, e: u32) -> u32 {
    let data = ld(e + E_DATA);
    let flags = ld16(e + E_FLAGS);
    if data == 0 {
        return walk_region(h, e, ld(h + H_SEGS));
    }
    let mut seg = ld(h + H_SEGS);
    if flags & (ENTRY_REGION | ENTRY_UNCOMMITTED) != 0 {
        while seg != 0 {
            if flags & ENTRY_REGION != 0 && seg == data {
                return walk_from(h, e, seg, data_start(h, seg));
            }
            if flags & ENTRY_UNCOMMITTED != 0 && data >= seg && data < seg_end(seg) {
                let next = ld(seg + S_NEXT);
                if next != 0 {
                    return walk_region(h, e, next);
                }
                return walk_large(e, ld(h + H_LARGE));
            }
            seg = ld(seg + S_NEXT);
        }
        return STATUS_INVALID_PARAMETER;
    }
    let busy = flags & ENTRY_BUSY != 0;
    let hdr = data.wrapping_sub(if busy { HDR } else { FREE_OVERHEAD });
    let mut l = ld(h + H_LARGE);
    while l != 0 {
        if hdr == l + LARGE_HDR {
            return walk_large(e, ld(l + L_NEXT));
        }
        l = ld(l + L_NEXT);
    }
    while seg != 0 {
        let end = carved_end(h, seg);
        if hdr >= data_start(h, seg) && hdr < end {
            let c = hdr_class(hdr);
            if c >= NCLASS && c != EXACT {
                return STATUS_INVALID_PARAMETER;
            }
            return walk_from(h, e, seg, hdr + block_size(hdr));
        }
        if hdr == end && !busy {
            return walk_uncommitted(h, e, seg);
        }
        seg = ld(seg + S_NEXT);
    }
    STATUS_INVALID_PARAMETER
}

// ---- Calls from translated code ------------------------------------------------------

/// A stdcall call in progress: the CPU state and the stack at entry.
struct Frame {
    cpu: u32,
    esp: u32,
}

impl Frame {
    #[inline(always)]
    fn new(cpu: u32) -> Frame {
        Frame {
            cpu,
            esp: ld(cpu + CPU_ESP),
        }
    }

    #[inline(always)]
    fn arg(&self, i: u32) -> u32 {
        ld(self.esp + 4 + 4 * i)
    }

    /// Returns `v` in eax, popping the return address and `n` arguments.
    #[inline(always)]
    fn ret(&self, n: u32, v: u32) -> u32 {
        st(self.cpu + CPU_EAX, v);
        st(self.cpu + CPU_ESP, self.esp + 4 + 4 * n);
        ld(self.esp)
    }

    /// RtlSetLastWin32ErrorAndNtStatusFromNtStatus.
    fn set_status(&self, status: u32) {
        let teb = ld(self.cpu + CPU_FS_BASE);
        if teb == 0 {
            return;
        }
        let error = match status {
            STATUS_NO_MEMORY => 8,
            STATUS_INVALID_HANDLE => 6,
            _ => 87,
        };
        st(teb + TEB_LAST_ERROR, error);
        st(teb + TEB_LAST_STATUS, status);
    }

    /// Wine's heap_set_status, then the return: STATUS_NO_MEMORY with
    /// HEAP_GENERATE_EXCEPTIONS continues in `RtlRaiseStatus(status)`, as if
    /// the caller had called it instead (`n` >= 2).
    #[inline(always)]
    fn finish(&self, n: u32, v: u32, flags: u32, status: u32) -> u32 {
        if status != 0 {
            return self.fail(n, v, flags, status);
        }
        self.ret(n, v)
    }

    #[inline(never)]
    fn fail(&self, n: u32, v: u32, flags: u32, status: u32) -> u32 {
        let raise = ld(CTX_RAISE_STATUS);
        if status == STATUS_NO_MEMORY && flags & HEAP_GENERATE_EXCEPTIONS != 0 && raise != 0 {
            let ra = ld(self.esp);
            let sp = self.esp + 4 * n - 4;
            st(sp, ra);
            st(sp + 4, status);
            st(self.cpu + CPU_ESP, sp);
            return raise;
        }
        self.set_status(status);
        self.ret(n, v)
    }
}

/// Sets up the process-wide state: ntdll's `RtlRaiseStatus` and the guest
/// limit. Called once, before ntdll runs.
#[export_name = "wwt_heap_init"]
pub extern "C" fn init(raise_status: u32, guest_limit: u32) {
    st(CTX_PROCESS_HEAP, 0);
    st(CTX_RAISE_STATUS, raise_status);
    st(CTX_LIMIT, guest_limit);
    st(CTX_HEAPS, 0);
}

/// `HANDLE RtlCreateHeap(ULONG flags, void *addr, SIZE_T total, SIZE_T commit,
/// void *lock, RTL_HEAP_PARAMETERS *params)`. The heap's memory always
/// comes from virtual memory, also when the caller offers some (`addr`).
#[export_name = "RtlCreateHeap"]
pub extern "C" fn rtl_create_heap(cpu: u32) -> u32 {
    let f = Frame::new(cpu);
    let (flags, total, commit) = (f.arg(0), f.arg(2), f.arg(3));
    f.ret(6, create(flags, total, commit))
}

/// `HANDLE RtlDestroyHeap(HANDLE heap)`: 0 when destroyed; the process heap
/// and invalid handles are returned unchanged.
#[export_name = "RtlDestroyHeap"]
pub extern "C" fn rtl_destroy_heap(cpu: u32) -> u32 {
    let f = Frame::new(cpu);
    let h = f.arg(0);
    f.ret(1, destroy(h))
}

/// `void *RtlAllocateHeap(HANDLE heap, ULONG flags, SIZE_T size)`
#[export_name = "RtlAllocateHeap"]
pub extern "C" fn rtl_allocate_heap(cpu: u32) -> u32 {
    let f = Frame::new(cpu);
    let (h, flags, n) = (f.arg(0), f.arg(1), f.arg(2));
    if !heap_ok(h) {
        return f.finish(3, 0, flags, STATUS_INVALID_HANDLE);
    }
    let p = alloc(h, eff_flags(h, flags), n);
    f.finish(3, p, flags, if p == 0 { STATUS_NO_MEMORY } else { 0 })
}

/// `BOOLEAN RtlFreeHeap(HANDLE heap, ULONG flags, void *ptr)`
#[export_name = "RtlFreeHeap"]
pub extern "C" fn rtl_free_heap(cpu: u32) -> u32 {
    let f = Frame::new(cpu);
    let (h, flags, p) = (f.arg(0), f.arg(1), f.arg(2));
    if p == 0 {
        return f.ret(3, 1);
    }
    let hdr = if heap_ok(h) { block(h, p) } else { 0 };
    if hdr == 0 {
        return f.finish(3, 0, flags, STATUS_INVALID_PARAMETER);
    }
    free_block(h, hdr);
    f.ret(3, 1)
}

/// `void *RtlReAllocateHeap(HANDLE heap, ULONG flags, void *ptr, SIZE_T size)`
#[export_name = "RtlReAllocateHeap"]
pub extern "C" fn rtl_reallocate_heap(cpu: u32) -> u32 {
    let f = Frame::new(cpu);
    let (h, flags, p, n) = (f.arg(0), f.arg(1), f.arg(2), f.arg(3));
    if p == 0 {
        return f.ret(4, 0);
    }
    if !heap_ok(h) {
        return f.finish(4, 0, flags, STATUS_INVALID_HANDLE);
    }
    let (q, status) = realloc(h, flags, p, n);
    f.finish(4, q, flags, status)
}

/// `SIZE_T RtlSizeHeap(HANDLE heap, ULONG flags, const void *ptr)`: the
/// requested size, or ~0.
#[export_name = "RtlSizeHeap"]
pub extern "C" fn rtl_size_heap(cpu: u32) -> u32 {
    let f = Frame::new(cpu);
    let (h, flags, p) = (f.arg(0), f.arg(1), f.arg(2));
    let hdr = if heap_ok(h) { block(h, p) } else { 0 };
    if hdr == 0 {
        return f.finish(3, !0, flags, STATUS_INVALID_PARAMETER);
    }
    f.ret(3, ld(hdr))
}

/// `BOOLEAN RtlValidateHeap(HANDLE heap, ULONG flags, const void *ptr)`
#[export_name = "RtlValidateHeap"]
pub extern "C" fn rtl_validate_heap(cpu: u32) -> u32 {
    let f = Frame::new(cpu);
    let (h, p) = (f.arg(0), f.arg(2));
    let ok = heap_ok(h)
        && if p != 0 {
            validate_ptr(h, p)
        } else {
            validate_heap(h)
        };
    f.ret(3, ok as u32)
}

/// `BOOLEAN RtlLockHeap(HANDLE heap)`: the guest has one thread, so locking
/// only checks the handle.
#[export_name = "RtlLockHeap"]
pub extern "C" fn rtl_lock_heap(cpu: u32) -> u32 {
    let f = Frame::new(cpu);
    let h = f.arg(0);
    f.ret(1, heap_ok(h) as u32)
}

/// `BOOLEAN RtlUnlockHeap(HANDLE heap)`
#[export_name = "RtlUnlockHeap"]
pub extern "C" fn rtl_unlock_heap(cpu: u32) -> u32 {
    let f = Frame::new(cpu);
    let h = f.arg(0);
    f.ret(1, heap_ok(h) as u32)
}

/// `ULONG RtlCompactHeap(HANDLE heap, ULONG flags)`: 0, as in Wine.
#[export_name = "RtlCompactHeap"]
pub extern "C" fn rtl_compact_heap(cpu: u32) -> u32 {
    Frame::new(cpu).ret(2, 0)
}

/// `NTSTATUS RtlWalkHeap(HANDLE heap, void *entry)`: the heap's segments,
/// each followed by its blocks (in use and free) and its uncarved rest,
/// then the large blocks.
#[export_name = "RtlWalkHeap"]
pub extern "C" fn rtl_walk_heap(cpu: u32) -> u32 {
    let f = Frame::new(cpu);
    let (h, e) = (f.arg(0), f.arg(1));
    let status = if e == 0 {
        STATUS_INVALID_PARAMETER
    } else if !heap_ok(h) {
        STATUS_INVALID_HANDLE
    } else {
        walk(h, e)
    };
    f.ret(2, status)
}

/// `ULONG RtlGetProcessHeaps(ULONG count, HANDLE *heaps)`: the number of
/// heaps; they are stored when `count` is enough, the process heap first.
#[export_name = "RtlGetProcessHeaps"]
pub extern "C" fn rtl_get_process_heaps(cpu: u32) -> u32 {
    let f = Frame::new(cpu);
    let (count, out) = (f.arg(0), f.arg(1));
    let mut total = 1;
    let mut h = ld(CTX_HEAPS);
    while h != 0 {
        total += 1;
        h = ld(h + H_NEXT_HEAP);
    }
    if total <= count {
        st(out, ld(CTX_PROCESS_HEAP));
        let (mut h, mut i) = (ld(CTX_HEAPS), 1);
        while h != 0 {
            st(out + 4 * i, h);
            i += 1;
            h = ld(h + H_NEXT_HEAP);
        }
    }
    f.ret(2, total)
}

/// `NTSTATUS RtlQueryHeapInformation(HANDLE heap, HEAP_INFORMATION_CLASS class,
/// void *info, SIZE_T size_in, SIZE_T *size_out)`
#[export_name = "RtlQueryHeapInformation"]
pub extern "C" fn rtl_query_heap_information(cpu: u32) -> u32 {
    let f = Frame::new(cpu);
    let (h, class, info, size_in, size_out) = (f.arg(0), f.arg(1), f.arg(2), f.arg(3), f.arg(4));
    let status = if class != 0 {
        STATUS_INVALID_INFO_CLASS
    } else if !heap_ok(h) {
        STATUS_ACCESS_VIOLATION
    } else {
        if size_out != 0 {
            st(size_out, 4);
        }
        if size_in < 4 {
            STATUS_BUFFER_TOO_SMALL
        } else {
            st(info, ld(h + H_COMPAT));
            0
        }
    };
    f.ret(5, status)
}

/// `NTSTATUS RtlSetHeapInformation(HANDLE heap, HEAP_INFORMATION_CLASS class,
/// void *info, SIZE_T size)`: HeapCompatibilityInformation can be set once,
/// to the standard heap (0) or the low-fragmentation heap (2), which this
/// heap already is.
#[export_name = "RtlSetHeapInformation"]
pub extern "C" fn rtl_set_heap_information(cpu: u32) -> u32 {
    let f = Frame::new(cpu);
    let (h, class, info, size) = (f.arg(0), f.arg(1), f.arg(2), f.arg(3));
    let status = if class != 0 {
        0
    } else if size < 4 {
        STATUS_BUFFER_TOO_SMALL
    } else if !heap_ok(h) {
        STATUS_INVALID_HANDLE
    } else if ld(h + H_FLAGS) & HEAP_NO_SERIALIZE != 0 {
        STATUS_INVALID_PARAMETER
    } else {
        let v = ld(info);
        if (v != 0 && v != 2) || ld(h + H_COMPAT) != 0 {
            STATUS_UNSUCCESSFUL
        } else {
            st(h + H_COMPAT, v);
            0
        }
    };
    f.ret(4, status)
}

/// `BOOLEAN RtlGetUserInfoHeap(HANDLE heap, ULONG flags, void *ptr,
/// void **user_value, ULONG *user_flags)`
#[export_name = "RtlGetUserInfoHeap"]
pub extern "C" fn rtl_get_user_info_heap(cpu: u32) -> u32 {
    let f = Frame::new(cpu);
    let (h, flags, p, value, uflags) = (f.arg(0), f.arg(1), f.arg(2), f.arg(3), f.arg(4));
    st(uflags, 0);
    // An invalid heap succeeds without touching the value, as in Wine.
    if !heap_ok(h) {
        return f.ret(5, 1);
    }
    let hdr = block(h, p);
    if hdr == 0 {
        st(value, 0);
        return f.finish(5, 0, flags, STATUS_INVALID_PARAMETER);
    }
    let bits = hdr_bits(hdr);
    let user = (bits & USER_MASK) << 5;
    if user != 0 {
        st(uflags, user & !HEAP_ADD_USER_INFO);
        st(
            value,
            if bits & USER_INFO != 0 {
                ld(user_value_addr(hdr))
            } else {
                0
            },
        );
    }
    f.ret(5, 1)
}

/// `BOOLEAN RtlSetUserValueHeap(HANDLE heap, ULONG flags, void *ptr,
/// void *user_value)`: only blocks allocated with HEAP_ADD_USER_INFO.
#[export_name = "RtlSetUserValueHeap"]
pub extern "C" fn rtl_set_user_value_heap(cpu: u32) -> u32 {
    let f = Frame::new(cpu);
    let (h, p, value) = (f.arg(0), f.arg(2), f.arg(3));
    if !heap_ok(h) {
        return f.ret(4, 1);
    }
    let hdr = block(h, p);
    if hdr == 0 || hdr_bits(hdr) & USER_INFO == 0 {
        return f.ret(4, 0);
    }
    st(user_value_addr(hdr), value);
    f.ret(4, 1)
}

/// `BOOLEAN RtlSetUserFlagsHeap(HANDLE heap, ULONG flags, void *ptr,
/// ULONG clear, ULONG set)`: the user flags 0x200..0x800.
#[export_name = "RtlSetUserFlagsHeap"]
pub extern "C" fn rtl_set_user_flags_heap(cpu: u32) -> u32 {
    let f = Frame::new(cpu);
    let (h, p, clear, set) = (f.arg(0), f.arg(2), f.arg(3), f.arg(4));
    if (clear | set) & !0xe00 != 0 {
        f.set_status(STATUS_INVALID_PARAMETER);
        return f.ret(5, 0);
    }
    if !heap_ok(h) {
        return f.ret(5, 1);
    }
    let hdr = block(h, p);
    if hdr == 0 || hdr_bits(hdr) & USER_INFO == 0 {
        return f.ret(5, 0);
    }
    let bits = hdr_bits(hdr) & !(clear >> 5 & USER_MASK) | set >> 5 & USER_MASK;
    st8(hdr + 7, bits);
    f.ret(5, 1)
}

/// ntdll's internal `heap_thread_detach` (cdecl, no arguments): Wine
/// releases the exiting thread's LFH groups; this heap keeps none.
#[export_name = "_heap_thread_detach"]
pub extern "C" fn heap_thread_detach(cpu: u32) -> u32 {
    Frame::new(cpu).ret(0, 0)
}

#[cfg(test)]
mod tests;
