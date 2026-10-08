//! The heap's functions called as translated code calls them: arguments on
//! a guest stack, results in the CPU struct.

use super::*;

const CPU: u32 = 0xa000;
const TEB: u32 = 0xc000;
const STACK: u32 = 0xf000;
const RET: u32 = 0x1234_5678;
const RAISE: u32 = 0x7777_0000;

type Api = extern "C" fn(u32) -> u32;

fn setup() {
    init(RAISE, TEST_MEM);
    st(CPU + CPU_FS_BASE, TEB);
}

/// Calls `f` with stdcall arguments; checks it returns to the caller with
/// `pops` arguments popped, and returns eax.
fn call_pops(f: Api, pops: u32, args: &[u32]) -> u32 {
    let sp = STACK - 4 * (args.len() as u32 + 1);
    st(sp, RET);
    for (i, a) in args.iter().enumerate() {
        st(sp + 4 + 4 * i as u32, *a);
    }
    st(CPU + CPU_ESP, sp);
    assert_eq!(f(CPU), RET);
    assert_eq!(ld(CPU + CPU_ESP), sp + 4 + 4 * pops);
    ld(CPU + CPU_EAX)
}

fn call(f: Api, args: &[u32]) -> u32 {
    call_pops(f, args.len() as u32, args)
}

fn last_error() -> u32 {
    ld(TEB + TEB_LAST_ERROR)
}

fn create_heap(flags: u32, commit: u32, total: u32) -> u32 {
    call(rtl_create_heap, &[flags, 0, total, commit, 0, 0])
}

fn alloc_(h: u32, flags: u32, n: u32) -> u32 {
    call(rtl_allocate_heap, &[h, flags, n])
}

fn free_(h: u32, p: u32) -> u32 {
    call(rtl_free_heap, &[h, 0, p])
}

fn size_(h: u32, p: u32) -> u32 {
    call(rtl_size_heap, &[h, 0, p])
}

fn realloc_(h: u32, flags: u32, p: u32, n: u32) -> u32 {
    call(rtl_reallocate_heap, &[h, flags, p, n])
}

fn validate(h: u32, p: u32) -> u32 {
    call(rtl_validate_heap, &[h, 0, p])
}

#[test]
fn abi_matches_the_translator() {
    assert_eq!(CPU_EAX, wwt::abi::cpu::gpr(0));
    assert_eq!(CPU_ESP, wwt::abi::cpu::gpr(4));
    assert_eq!(CPU_FS_BASE, wwt::abi::cpu::FS_BASE);
    let names: Vec<&str> = wwt::builtin::NATIVE_HEAP.to_vec();
    assert_eq!(names.len(), 18);
}

#[test]
fn size_classes() {
    let mut total = 16;
    while total <= MAX_SMALL {
        let c = class_of(total);
        assert!(c < NCLASS);
        assert!(class_size(c) >= total, "{total}");
        assert!(c == 0 || class_size(c - 1) < total, "{total}");
        assert_eq!(class_size(c) % 8, 0);
        total += 8;
    }
    assert_eq!(class_size(NCLASS - 1), MAX_SMALL);
    assert_eq!(class_size(30), 256);
    assert_eq!(class_size(31), 320);
}

#[test]
fn process_heap_basics() {
    setup();
    let h = create_heap(HEAP_GROWABLE, 0, 0);
    assert_ne!(h, 0);
    assert_eq!(h & 0xffff, 0);
    assert_eq!(ld(CTX_PROCESS_HEAP), h);
    assert_eq!(ld(h + 8), 0xffee_ffee);
    assert_eq!(ld(h + 0x40) & HEAP_GROWABLE, HEAP_GROWABLE);

    let mut ptrs = vec![];
    for n in [
        0, 1, 7, 8, 9, 100, 255, 256, 1000, 5000, 70000, 600_000, 3_000_000,
    ] {
        let p = alloc_(h, 0, n);
        assert_ne!(p, 0, "{n}");
        assert_eq!(p % 8, 0);
        assert_eq!(size_(h, p), n);
        assert_eq!(validate(h, p), 1);
        fill(p, 0xab, n);
        ptrs.push((p, n));
    }
    assert_eq!(validate(h, 0), 1);
    for &(p, n) in &ptrs {
        assert_eq!(size_(h, p), n);
        assert_eq!(free_(h, p), 1);
        assert_eq!(validate(h, p), 0);
    }
    assert_eq!(validate(h, 0), 1);
    // Freed blocks come back first.
    let p = alloc_(h, 0, 100);
    assert_eq!(p, ptrs[5].0);
    // NULL frees succeed.
    assert_eq!(free_(h, 0), 1);
}

#[test]
fn zero_memory() {
    setup();
    let h = create_heap(HEAP_GROWABLE, 0, 0);
    let p = alloc_(h, 0, 64);
    fill(p, 0xff, 64);
    free_(h, p);
    let q = alloc_(h, HEAP_ZERO_MEMORY, 61);
    assert_eq!(q, p);
    for i in 0..64 {
        assert_eq!(ld8(q + i), 0);
    }
    // Growing in place zeroes the new part.
    fill(q, 0xee, 61);
    let r = realloc_(h, HEAP_ZERO_MEMORY, q, 64);
    assert_eq!(r, q);
    for i in 61..64 {
        assert_eq!(ld8(r + i), 0);
    }
    assert_eq!(ld8(r + 60), 0xee);
}

#[test]
fn bad_pointers() {
    setup();
    let h = create_heap(HEAP_GROWABLE, 0, 0);
    let h2 = create_heap(HEAP_GROWABLE, 0, 0);
    let p = alloc_(h, 0, 40);
    st(TEB + TEB_LAST_ERROR, 0);
    // Another heap's block.
    assert_eq!(free_(h2, p), 0);
    assert_eq!(last_error(), 87);
    assert_eq!(ld(TEB + TEB_LAST_STATUS), STATUS_INVALID_PARAMETER);
    assert_eq!(free_(h, p), 1);
    // Twice.
    assert_eq!(free_(h, p), 0);
    assert_eq!(size_(h, p), !0);
    // Unaligned, inside a block, outside the guest, not a heap.
    let q = alloc_(h, 0, 400);
    for bad in [q + 4, q + 16, 0x10, 0xffff_fff0, 0x4000_0000] {
        assert_eq!(free_(h, bad), 0, "{bad:#x}");
        assert_eq!(validate(h, bad), 0, "{bad:#x}");
    }
    assert_eq!(free_(0x1234, q), 0);
    assert_eq!(alloc_(0x1234, 0, 8), 0);
    assert_eq!(last_error(), 6);
    assert_eq!(validate(h, q), 1);
}

#[test]
fn realloc_moves_and_keeps_contents() {
    setup();
    let h = create_heap(HEAP_GROWABLE, 0, 0);
    let p = alloc_(h, 0, 100);
    for i in 0..100 {
        st8(p + i, i);
    }
    // Within the block's class: in place.
    assert_eq!(realloc_(h, 0, p, 104), p);
    assert_eq!(size_(h, p), 104);
    // Beyond: moves.
    let q = realloc_(h, 0, p, 5000);
    assert_ne!(q, p);
    assert_eq!(size_(h, q), 5000);
    for i in 0..100 {
        assert_eq!(ld8(q + i), i);
    }
    assert_eq!(validate(h, p), 0);
    // In place only.
    st(TEB + TEB_LAST_ERROR, 0);
    assert_eq!(realloc_(h, HEAP_REALLOC_IN_PLACE_ONLY, q, 50000), 0);
    assert_eq!(last_error(), 8);
    assert_eq!(validate(h, q), 1);
    // Shrinking stays in place, however far, as on Windows.
    let r = realloc_(h, 0, q, 10);
    assert_eq!(r, q);
    assert_eq!(size_(h, r), 10);
    for i in 0..10 {
        assert_eq!(ld8(r + i), i);
    }
    let s = alloc_(h, 0, 5000);
    assert_eq!(realloc_(h, HEAP_REALLOC_IN_PLACE_ONLY, s, 10), s);
    assert_eq!(size_(h, s), 10);
    // Large blocks.
    let big = alloc_(h, 0, 2 << 20);
    st8(big + (2 << 20) - 1, 0x5a);
    let big2 = realloc_(h, 0, big, 3 << 20);
    assert_eq!(ld8(big2 + (2 << 20) - 1), 0x5a);
    assert_eq!(size_(h, big2), 3 << 20);
    assert_eq!(realloc_(h, 0, big2, 1 << 20), big2);
    assert_eq!(realloc_(h, 0, 0, 10), 0);
    assert_eq!(validate(h, 0), 1);
}

#[test]
fn large_blocks_release_their_region() {
    setup();
    let h = create_heap(HEAP_GROWABLE, 0, 0);
    let before = vm_used();
    let p = alloc_(h, HEAP_ZERO_MEMORY, 1 << 20);
    assert_eq!(
        vm_used() - before,
        round_region((1 << 20) + LARGE_HDR + HDR)
    );
    assert_eq!(ld(p + 12345), 0);
    assert_eq!(free_(h, p), 1);
    assert_eq!(vm_used(), before);
    assert_eq!(free_(h, p), 0);
}

#[test]
fn user_info() {
    setup();
    let h = create_heap(HEAP_GROWABLE, 0, 0);
    let (value, uflags) = (0xe000, 0xe004);
    for n in [10, 600_000] {
        let p = alloc_(h, HEAP_ADD_USER_INFO | 0x200, n);
        fill(p, 0xff, n);
        assert_eq!(call(rtl_get_user_info_heap, &[h, 0, p, value, uflags]), 1);
        assert_eq!((ld(value), ld(uflags)), (0, 0x200));
        assert_eq!(call(rtl_set_user_value_heap, &[h, 0, p, 0xbeef]), 1);
        assert_eq!(call(rtl_set_user_flags_heap, &[h, 0, p, 0x200, 0x400]), 1);
        call(rtl_get_user_info_heap, &[h, 0, p, value, uflags]);
        assert_eq!((ld(value), ld(uflags)), (0xbeef, 0x400));
        assert_eq!(size_(h, p), n);
        // Kept when resized in place.
        assert_eq!(realloc_(h, 0, p, n - 2), p);
        call(rtl_get_user_info_heap, &[h, 0, p, value, uflags]);
        assert_eq!(ld(value), 0xbeef);
        assert_eq!(call(rtl_set_user_flags_heap, &[h, 0, p, 0x100, 0]), 0);
    }
    let q = alloc_(h, 0, 10);
    assert_eq!(call(rtl_set_user_value_heap, &[h, 0, q, 1]), 0);
    assert_eq!(call(rtl_get_user_info_heap, &[h, 0, q, value, uflags]), 1);
    assert_eq!(ld(uflags), 0);
}

#[test]
fn heap_list_and_destroy() {
    setup();
    let process = create_heap(HEAP_GROWABLE, 0, 0);
    let a = create_heap(0, 0x1000, 0);
    let b = create_heap(0, 0x1000, 0x10000);
    assert_eq!(ld(b + 0x40) & HEAP_GROWABLE, 0);
    assert_eq!(
        ld(a + 0x40) & (HEAP_GROWABLE | HEAP_PRIVATE),
        HEAP_GROWABLE | HEAP_PRIVATE
    );
    let out = 0xe000;
    assert_eq!(call(rtl_get_process_heaps, &[1, out]), 3);
    assert_eq!(call(rtl_get_process_heaps, &[3, out]), 3);
    assert_eq!((ld(out), ld(out + 4), ld(out + 8)), (process, b, a));
    for _ in 0..1000 {
        alloc_(a, 0, 3000);
    }
    alloc_(a, 0, 1 << 20);
    let before = vm_used();
    assert_eq!(call(rtl_destroy_heap, &[a]), 0);
    assert!(vm_used() < before - (3 << 20));
    assert_eq!(call(rtl_destroy_heap, &[a]), a);
    assert_eq!(call(rtl_destroy_heap, &[process]), process);
    assert_eq!(call(rtl_get_process_heaps, &[3, out]), 2);
    assert_eq!((ld(out), ld(out + 4)), (process, b));
    assert_eq!(call(rtl_lock_heap, &[b]), 1);
    assert_eq!(call(rtl_unlock_heap, &[a]), 0);
}

#[test]
fn fixed_size_heap_runs_out() {
    setup();
    create_heap(HEAP_GROWABLE, 0, 0);
    let h = create_heap(0, 0, 0x10000);
    let mut n = 0;
    while alloc_(h, 0, 1000) != 0 {
        n += 1;
    }
    assert!((50..64).contains(&n), "{n}");
    assert_eq!(last_error(), 8);
    assert_eq!(alloc_(h, 0, 1 << 20), 0);
    assert_eq!(validate(h, 0), 1);
}

#[test]
fn generate_exceptions() {
    setup();
    let h = create_heap(HEAP_GROWABLE, 0, 0);
    let sp = STACK - 16;
    st(sp, RET);
    st(sp + 4, h);
    st(sp + 8, HEAP_GENERATE_EXCEPTIONS);
    st(sp + 12, 0xf000_0000);
    st(CPU + CPU_ESP, sp);
    // Continues in RtlRaiseStatus(STATUS_NO_MEMORY), called from the caller.
    assert_eq!(rtl_allocate_heap(CPU), RAISE);
    let esp = ld(CPU + CPU_ESP);
    assert_eq!(esp, sp + 8);
    assert_eq!((ld(esp), ld(esp + 4)), (RET, STATUS_NO_MEMORY));
}

#[test]
fn information() {
    setup();
    let h = create_heap(HEAP_GROWABLE, 0, 0);
    let (info, out) = (0xe000, 0xe004);
    assert_eq!(call(rtl_query_heap_information, &[h, 0, info, 4, out]), 0);
    assert_eq!((ld(info), ld(out)), (0, 4));
    assert_eq!(
        call(rtl_query_heap_information, &[h, 0, info, 2, out]),
        STATUS_BUFFER_TOO_SMALL
    );
    st(info, 2);
    assert_eq!(call(rtl_set_heap_information, &[h, 0, info, 4]), 0);
    assert_eq!(
        call(rtl_set_heap_information, &[h, 0, info, 4]),
        STATUS_UNSUCCESSFUL
    );
    call(rtl_query_heap_information, &[h, 0, info, 4, 0]);
    assert_eq!(ld(info), 2);
    assert_eq!(call(rtl_compact_heap, &[h, 0]), 0);
    assert_eq!(call_pops(heap_thread_detach, 0, &[]), 0);
}

/// Walks the heap; returns the busy blocks (pointer, size) and checks the
/// entries are consistent.
fn walk_busy(h: u32) -> Vec<(u32, u32)> {
    let e = 0xe000;
    fill(e, 0, 28);
    let mut busy = vec![];
    let mut regions = 0;
    loop {
        let status = call(rtl_walk_heap, &[h, e]);
        if status == STATUS_NO_MORE_ENTRIES {
            break;
        }
        assert_eq!(status, 0);
        let flags = ld16(e + E_FLAGS);
        if flags & ENTRY_REGION != 0 {
            regions += 1;
        } else if flags & ENTRY_BUSY != 0 {
            busy.push((ld(e), ld(e + 4)));
        }
        assert!(busy.len() < 100_000);
    }
    assert!(regions >= 1);
    busy.sort();
    busy
}

#[test]
fn walk_sees_every_block() {
    setup();
    let h = create_heap(HEAP_GROWABLE, 0, 0);
    let mut live = vec![];
    for i in 0..3000u32 {
        let n = i * 37 % 4000 + if i % 50 == 1 { 200_000 } else { 0 };
        let p = alloc_(h, 0, n);
        if i % 3 == 0 {
            free_(h, p);
        } else {
            live.push((p, n));
        }
    }
    for i in 0..3 {
        let n = (i + 1) << 20;
        live.push((alloc_(h, 0, n), n));
    }
    live.sort();
    assert_eq!(walk_busy(h), live);
    assert_eq!(validate(h, 0), 1);
}

/// Random allocations, resizes and frees against a model of the blocks'
/// contents.
#[test]
fn random_against_model() {
    setup();
    let h = create_heap(HEAP_GROWABLE, 0, 0);
    let mut rng = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let mut live: Vec<(u32, u32, u8)> = vec![];
    for step in 0..20_000u32 {
        let r = next();
        let n = match r % 20 {
            0 | 1 => (r >> 8) as u32 % 100_000,
            2 => (r >> 8) as u32 % 700_000,
            _ => (r >> 8) as u32 % 300,
        };
        match (r >> 40) % 5 {
            0 | 1 => {
                let p = alloc_(
                    h,
                    if r & 1 << 50 != 0 {
                        HEAP_ZERO_MEMORY
                    } else {
                        0
                    },
                    n,
                );
                assert_ne!(p, 0);
                if r & 1 << 50 != 0 {
                    for i in (0..n).step_by(97) {
                        assert_eq!(ld8(p + i), 0);
                    }
                }
                let tag = step as u8;
                fill(p, tag, n);
                live.push((p, n, tag));
            }
            2 if !live.is_empty() => {
                let i = (r >> 20) as usize % live.len();
                let (p, old, tag) = live[i];
                let q = realloc_(h, 0, p, n);
                assert_ne!(q, 0);
                for k in (0..old.min(n)).step_by(13) {
                    assert_eq!(ld8(q + k), tag as u32);
                }
                fill(q, tag, n);
                live[i] = (q, n, tag);
            }
            _ if !live.is_empty() => {
                let i = (r >> 20) as usize % live.len();
                let (p, n, tag) = live.swap_remove(i);
                assert_eq!(size_(h, p), n);
                for k in (0..n).step_by(29) {
                    assert_eq!(ld8(p + k), tag as u32);
                }
                assert_eq!(free_(h, p), 1);
            }
            _ => {}
        }
        if step % 5000 == 0 {
            assert_eq!(validate(h, 0), 1);
        }
    }
    let mut want: Vec<(u32, u32)> = live.iter().map(|&(p, n, _)| (p, n)).collect();
    want.sort();
    assert_eq!(walk_busy(h), want);
}

#[test]
fn fixed_size_heap_fills_up_exactly() {
    setup();
    create_heap(HEAP_GROWABLE, 0, 0);
    let h = create_heap(0, 0x20000, 0x20000);
    // No size class fits what is left: the block takes just what it needs.
    let p = alloc_(h, 0, 0x1f800);
    assert_ne!(p, 0);
    assert_eq!(size_(h, p), 0x1f800);
    assert_eq!(validate(h, p), 1);
    assert_eq!(realloc_(h, HEAP_REALLOC_IN_PLACE_ONLY, p, 0x1f7fc), p);
    assert_eq!(realloc_(h, HEAP_REALLOC_IN_PLACE_ONLY, p, 0x1f900), 0);
    let q = alloc_(h, 0, 100);
    assert_ne!(q, 0);
    assert_eq!(walk_busy(h), vec![(p, 0x1f7fc), (q, 100)]);
    assert_eq!(validate(h, 0), 1);
    // Freed, it becomes blocks of the size classes.
    assert_eq!(free_(h, p), 1);
    assert_eq!(validate(h, 0), 1);
    assert_eq!(walk_busy(h), vec![(q, 100)]);
    let mut n = 0;
    while alloc_(h, 0, 0x1000) != 0 {
        n += 1;
    }
    assert!(n >= 15, "{n}");

    assert_eq!(validate(h, 0), 1);
}

#[test]
fn large_blocks_are_aligned_as_on_windows() {
    setup();
    let h = create_heap(HEAP_GROWABLE, 0, 0);
    for _ in 0..4 {
        assert_eq!(alloc_(h, 0, 0x80000) % 64, 32);
    }
}

#[test]
fn heaps_report_lfh_once_used() {
    setup();
    create_heap(HEAP_GROWABLE, 0, 0);
    let h = create_heap(0, 0, 0);
    let (info, out) = (0xe000, 0xe004);
    for _ in 0..0x11 {
        call(rtl_query_heap_information, &[h, 0, info, 4, out]);
        assert_eq!(ld(info), 0);
        alloc_(h, 0, 0);
    }
    call(rtl_query_heap_information, &[h, 0, info, 4, out]);
    assert_eq!(ld(info), 2);
}

/// A heap that cannot grow, filled and emptied at random: exact blocks and
/// split free blocks keep the heap walkable and the contents intact.
#[test]
fn random_fixed_heap() {
    setup();
    create_heap(HEAP_GROWABLE, 0, 0);
    let h = create_heap(0, 0, 0x40000);
    let mut rng = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let mut live: Vec<(u32, u32, u8)> = vec![];
    let mut failed = 0;
    for step in 0..20_000u32 {
        let r = next();
        let n = if r % 8 == 0 {
            (r >> 8) as u32 % 0x30000
        } else {
            (r >> 8) as u32 % 2000
        };
        if (r >> 40) % 3 == 0 && !live.is_empty() {
            let i = (r >> 20) as usize % live.len();
            let (p, n, tag) = live.swap_remove(i);
            for k in (0..n).step_by(31) {
                assert_eq!(ld8(p + k), tag as u32);
            }
            assert_eq!(free_(h, p), 1);
        } else if (r >> 40) % 3 == 1 && !live.is_empty() {
            let i = (r >> 20) as usize % live.len();
            let (p, old, tag) = live[i];
            let q = realloc_(h, 0, p, n);
            if q == 0 {
                failed += 1;
                continue;
            }
            for k in (0..old.min(n)).step_by(17) {
                assert_eq!(ld8(q + k), tag as u32);
            }
            fill(q, tag, n);
            live[i] = (q, n, tag);
        } else {
            let p = alloc_(h, 0, n);
            if p == 0 {
                failed += 1;
                continue;
            }
            fill(p, step as u8, n);
            live.push((p, n, step as u8));
        }
        if step % 500 == 0 {
            assert_eq!(validate(h, 0), 1);
            let mut want: Vec<(u32, u32)> = live.iter().map(|&(p, n, _)| (p, n)).collect();
            want.sort();
            assert_eq!(walk_busy(h), want);
        }
    }
    assert!(failed > 0);
}
