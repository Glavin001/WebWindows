//! Wine's hot string and locale functions as native WebAssembly.
//!
//! Translated Wine runs `CompareStringW` as translated x86: per character a
//! table lookup and a handful of byte appends to sort keys, each a
//! translated call with its register traffic. String-heavy programs spend
//! half their time there. This module implements such functions natively
//! (`wwt::builtin::NATIVE_TRY`): `CompareStringEx`, the C string functions
//! of ntdll, msvcrt and ucrtbase, and the thread-local storage lookups on
//! their paths. When Wine's DLLs are translated with `--native-strings`,
//! each of these functions first calls its import here, with the CPU state
//! written back. An import either does
//! the whole x86 function (reads the arguments from the guest stack, sets
//! eax, pops the return address, and for stdcall the arguments) and returns
//! the address to continue at, or declines by returning 0 without changing
//! anything, and the translated body runs instead.
//!
//! Declining keeps the behavior exactly Wine's wherever the native code
//! would differ:
//!
//! * **Faults.** Translated code faults on accesses to the null region (the
//!   first 64 KB) and at or above the guest limit, and nowhere else. Before
//!   reading, the functions check that everything they would read is outside
//!   those (conservatively: below the guest limit's last 64 KB); when not,
//!   they decline and the translated code faults, or not, exactly where the
//!   x86 code would, with the same instruction and address. (A scan for a
//!   terminator declines when it reaches the end of the readable range.)
//! * **Cases not implemented**: `CompareStringEx` with a named locale, flags
//!   it rejects, or strings too long for the scratch memory; TLS expansion
//!   slots; the C runtime's first use of a thread.
//!
//! None of these functions write guest memory, except `TlsGetValue` the
//! thread's last error in its TEB (which holds no code), so the store map
//! (pages with translated code) does not concern them.

#![cfg_attr(target_arch = "wasm32", no_std)]

mod compare;
mod mem;
#[cfg(test)]
mod tests;

use mem::*;

#[cfg(target_arch = "wasm32")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

/// CPU struct offsets (`wwt::abi::cpu`).
const CPU_EAX: u32 = 0;
const CPU_ESP: u32 = 16;
const CPU_FS_BASE: u32 = 64;
/// TEB fields (i386).
const TEB_LAST_ERROR: u32 = 0x34;
const TEB_TLS_SLOTS: u32 = 0xe10;
/// TLS_MINIMUM_AVAILABLE: slots in the TEB itself.
const TLS_SLOTS: u32 = 64;

/// Process-wide values, in the guest's null region (set by `init`).
const CTX: u32 = 0x8100;
/// End of the range guest pointers are read from without a check.
const CTX_READ_END: u32 = CTX;
/// Scratch memory for sort keys (above the guest limit) and its size.
const CTX_SCRATCH: u32 = CTX + 4;
const CTX_SCRATCH_SIZE: u32 = CTX + 8;

/// The null region: guest accesses below it fault.
const NULL_END: u32 = 0x10000;

/// Sets up the process-wide state: the guest limit, and scratch memory
/// outside guest memory. Called once, before Wine runs.
#[export_name = "wwt_strings_init"]
pub extern "C" fn init(guest_limit: u32, scratch: u32, scratch_size: u32) {
    st(CTX_READ_END, guest_limit - 0x10000);
    st(CTX_SCRATCH, scratch);
    st(CTX_SCRATCH_SIZE, scratch_size);
}

/// Whether the x86 code could read `n` bytes at `p` without faulting.
#[inline(always)]
fn readable(p: u32, n: u32) -> bool {
    let end = ld(CTX_READ_END);
    p >= NULL_END && p < end && n <= end - p
}

/// Bytes readable from `p` on (0 when `p` itself is not).
#[inline(always)]
fn readable_from(p: u32) -> u32 {
    let end = ld(CTX_READ_END);
    if p >= NULL_END && p < end {
        end - p
    } else {
        0
    }
}

/// A call in progress: the CPU state and the stack at entry.
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

    /// Returns `v` in eax, popping the return address and `n` arguments
    /// (0 for cdecl).
    #[inline(always)]
    fn ret(&self, n: u32, v: u32) -> u32 {
        st(self.cpu + CPU_EAX, v);
        st(self.cpu + CPU_ESP, self.esp + 4 + 4 * n);
        ld(self.esp)
    }
}

/// Declines the call: the translated body runs.
const DECLINE: u32 = 0;

/// Length of the NUL-terminated string of `size`-byte units at `p`, or
/// `None` when it runs out of readable memory first.
#[inline(always)]
fn str_len(p: u32, size: u32) -> Option<u32> {
    let avail = readable_from(p) / size;
    let mut i = 0;
    if size == 1 {
        // Four bytes at a time from an aligned address: an aligned word never
        // crosses the end of the readable range, which is page-aligned.
        while i < avail && (p + i) & 3 != 0 {
            if ld8(p + i) == 0 {
                return Some(i);
            }
            i += 1;
        }
        while i + 4 <= avail {
            let w = ld(p + i);
            if (w.wrapping_sub(0x0101_0101) & !w & 0x8080_8080) != 0 {
                break;
            }
            i += 4;
        }
        while i < avail {
            if ld8(p + i) == 0 {
                return Some(i);
            }
            i += 1;
        }
    } else {
        while i < avail {
            if ld16(p + 2 * i) == 0 {
                return Some(i);
            }
            i += 1;
        }
    }
    None
}

/// `size_t strlen(const char *str)`
#[export_name = "crt_strlen"]
pub extern "C" fn strlen(cpu: u32) -> u32 {
    let f = Frame::new(cpu);
    match str_len(f.arg(0), 1) {
        Some(n) => f.ret(0, n),
        None => DECLINE,
    }
}

/// `size_t wcslen(const WCHAR *str)`
#[export_name = "crt_wcslen"]
pub extern "C" fn wcslen(cpu: u32) -> u32 {
    let f = Frame::new(cpu);
    match str_len(f.arg(0), 2) {
        Some(n) => f.ret(0, n),
        None => DECLINE,
    }
}

/// `int memcmp(const void *p1, const void *p2, size_t n)`: -1, 0 or 1, in
/// ntdll, msvcrt and ucrtbase alike.
#[export_name = "crt_memcmp"]
pub extern "C" fn memcmp(cpu: u32) -> u32 {
    let f = Frame::new(cpu);
    let (p1, p2, n) = (f.arg(0), f.arg(1), f.arg(2));
    if n == 0 {
        return f.ret(0, 0);
    }
    if !readable(p1, n) || !readable(p2, n) {
        return DECLINE;
    }
    f.ret(0, mem_cmp(p1, p2, n) as u32)
}

/// -1, 0 or 1 as the first `n` bytes at `p1` and `p2` compare.
#[inline(always)]
fn mem_cmp(p1: u32, p2: u32, n: u32) -> i32 {
    let mut i = 0;
    while i + 8 <= n && ld64(p1 + i) == ld64(p2 + i) {
        i += 8;
    }
    while i < n {
        let (a, b) = (ld8(p1 + i), ld8(p2 + i));
        if a != b {
            return if a < b { -1 } else { 1 };
        }
        i += 1;
    }
    0
}

/// `int strcmp(const char *s1, const char *s2)`: -1, 0 or 1.
#[export_name = "crt_strcmp"]
pub extern "C" fn strcmp(cpu: u32) -> u32 {
    let f = Frame::new(cpu);
    let (s1, s2) = (f.arg(0), f.arg(1));
    let avail = readable_from(s1).min(readable_from(s2));
    let mut i = 0;
    while i < avail {
        let (a, b) = (ld8(s1 + i), ld8(s2 + i));
        if a == 0 || a != b {
            let r = match a.cmp(&b) {
                core::cmp::Ordering::Less => -1i32,
                core::cmp::Ordering::Equal => 0,
                core::cmp::Ordering::Greater => 1,
            };
            return f.ret(0, r as u32);
        }
        i += 1;
    }
    DECLINE
}

/// `char *strchr(const char *str, int c)`: also finds the terminator.
#[export_name = "crt_strchr"]
pub extern "C" fn strchr(cpu: u32) -> u32 {
    let f = Frame::new(cpu);
    let (s, c) = (f.arg(0), f.arg(1) & 0xff);
    let avail = readable_from(s);
    let mut i = 0;
    while i < avail {
        let a = ld8(s + i);
        if a == c {
            return f.ret(0, s + i);
        }
        if a == 0 {
            return f.ret(0, 0);
        }
        i += 1;
    }
    DECLINE
}

/// `WCHAR *wcschr(const WCHAR *str, WCHAR ch)`: also finds the terminator.
#[export_name = "crt_wcschr"]
pub extern "C" fn wcschr(cpu: u32) -> u32 {
    let f = Frame::new(cpu);
    let (s, c) = (f.arg(0), f.arg(1) & 0xffff);
    let avail = readable_from(s) / 2;
    let mut i = 0;
    while i < avail {
        let a = ld16(s + 2 * i);
        if a == c {
            return f.ret(0, s + 2 * i);
        }
        if a == 0 {
            return f.ret(0, 0);
        }
        i += 1;
    }
    DECLINE
}

/// `void *memchr(const void *p, int c, size_t n)`
#[export_name = "crt_memchr"]
pub extern "C" fn memchr(cpu: u32) -> u32 {
    let f = Frame::new(cpu);
    let (p, c, n) = (f.arg(0), f.arg(1) & 0xff, f.arg(2));
    if n == 0 {
        return f.ret(0, 0);
    }
    if !readable(p, n) {
        return DECLINE;
    }
    for i in 0..n {
        if ld8(p + i) == c {
            return f.ret(0, p + i);
        }
    }
    f.ret(0, 0)
}

/// `size_t strcspn(const char *str, const char *reject)`
#[export_name = "crt_strcspn"]
pub extern "C" fn strcspn(cpu: u32) -> u32 {
    let f = Frame::new(cpu);
    let (s, reject) = (f.arg(0), f.arg(1));
    let Some(nr) = str_len(reject, 1) else {
        return DECLINE;
    };
    let mut set = [0u32; 8];
    for i in 0..nr {
        let c = ld8(reject + i);
        set[(c >> 5) as usize] |= 1 << (c & 31);
    }
    // The terminator ends the scan too.
    set[0] |= 1;
    let avail = readable_from(s);
    let mut i = 0;
    while i < avail {
        let c = ld8(s + i);
        if set[(c >> 5) as usize] & (1 << (c & 31)) != 0 {
            return f.ret(0, i);
        }
        i += 1;
    }
    DECLINE
}

/// `INT CompareStringEx(const WCHAR *locale, DWORD flags, const WCHAR *str1,
/// int len1, const WCHAR *str2, int len2, NLSVERSIONINFO *version,
/// void *reserved, LPARAM handle)`, with the addresses of kernelbase's
/// `sort` tables and `current_locale_sort`: the user's default locale
/// (`locale` NULL) only.
#[export_name = "CompareStringEx"]
pub extern "C" fn compare_string_ex(cpu: u32, sort: u32, current_sort: u32) -> u32 {
    let f = Frame::new(cpu);
    let (locale, flags, s1, len1, s2, len2) = (
        f.arg(0),
        f.arg(1),
        f.arg(2),
        f.arg(3) as i32,
        f.arg(4),
        f.arg(5) as i32,
    );
    if locale != 0 || f.arg(6) | f.arg(7) | f.arg(8) != 0 || flags & !compare::SUPPORTED_FLAGS != 0
    {
        return DECLINE;
    }
    if s1 == 0 || s2 == 0 || ld(f.esp) == 0 {
        return DECLINE;
    }
    let len = |s: u32, len: i32| -> Option<u32> {
        let n = if len < 0 { str_len(s, 2)? } else { len as u32 };
        readable(s, n.checked_mul(2)?).then_some(n)
    };
    let (Some(n1), Some(n2)) = (len(s1, len1), len(s2, len2)) else {
        return DECLINE;
    };
    let scratch = (ld(CTX_SCRATCH), ld(CTX_SCRATCH_SIZE));
    match compare::compare(sort, ld(current_sort), flags, s1, n1, s2, n2, scratch) {
        Some(r) => f.ret(9, (r.signum() + 2) as u32),
        None => DECLINE,
    }
}

/// `void *TlsGetValue(DWORD index)` (kernelbase, and kernel32's stub that
/// jumps there): the TEB's own slots; expansion slots are declined.
#[export_name = "TlsGetValue"]
pub extern "C" fn tls_get_value(cpu: u32) -> u32 {
    let f = Frame::new(cpu);
    let (teb, index) = (ld(cpu + CPU_FS_BASE), f.arg(0));
    if index >= TLS_SLOTS || teb == 0 {
        return DECLINE;
    }
    st(teb + TEB_LAST_ERROR, 0);
    f.ret(1, ld(teb + TEB_TLS_SLOTS + 4 * index))
}

/// msvcrt's and ucrtbase's `thread_data_t *msvcrt_get_thread_data(void)`,
/// with the address of `msvcrt_tls_index`: the thread's data once it has
/// some (`TlsGetValue`, the last error kept); its creation is declined.
#[export_name = "msvcrt_get_thread_data"]
pub extern "C" fn msvcrt_get_thread_data(cpu: u32, tls_index: u32) -> u32 {
    let f = Frame::new(cpu);
    let (teb, index) = (ld(cpu + CPU_FS_BASE), ld(tls_index));
    if index >= TLS_SLOTS || teb == 0 {
        return DECLINE;
    }
    match ld(teb + TEB_TLS_SLOTS + 4 * index) {
        0 => DECLINE,
        data => f.ret(0, data),
    }
}
