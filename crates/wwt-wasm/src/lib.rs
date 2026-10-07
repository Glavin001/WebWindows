//! The translator as a WebAssembly module with a minimal C ABI, so the
//! runtime can translate code it discovers while running (fast mode).
//!
//! Exports:
//! * `wwt_alloc(len) -> ptr` / `wwt_free(ptr, len)`
//! * `wwt_translate(code, code_len, base, entries, n_entries, known,
//!   n_known, opt_level, flags) -> ptr` — returns a buffer `[len: u32][module bytes]`; free it
//!   with `wwt_free(ptr, len + 4)`.
//! * `wwt_translate_pe(file, len, profile, n_profile, opt_level, flags) ->
//!   ptr` — ahead-of-time translation of a whole .exe/.dll.
//! * `wwt_kernel() -> ptr` — the runtime kernel module, same format;
//!   `wwt_kernel64()` for a 64-bit (memory64) memory.
//! * `wwt_abi() -> ptr` — the ABI JSON, same format.

use wwt::discover::FlatCode;

/// Bit 0: disable memory checks; bit 1: disable code-write checks; bit 2:
/// the code is x86-64 (for `wwt_translate`; PE files carry their own mode);
/// bit 3: target a 64-bit (memory64) memory.
const FLAG_NO_MEM_CHECKS: u32 = 1;
const FLAG_NO_SMC_CHECKS: u32 = 2;
const FLAG_X64: u32 = 4;
const FLAG_MEM64: u32 = 8;

fn apply_flags(cfg: wwt::Config, flags: u32) -> wwt::Config {
    let mut cfg = cfg;
    let mc = flags & FLAG_NO_MEM_CHECKS == 0;
    let sc = flags & FLAG_NO_SMC_CHECKS == 0;
    cfg.lift.mem_checks = mc;
    cfg.codegen.mem_checks = mc;
    cfg.lift.smc_checks = sc;
    cfg.codegen.smc_checks = sc;
    let mode = if flags & FLAG_X64 != 0 {
        wwt::ir::Mode::X64
    } else {
        wwt::ir::Mode::X86
    };
    cfg.with_mode(mode).with_mem64(flags & FLAG_MEM64 != 0)
}

#[no_mangle]
pub extern "C" fn wwt_alloc(len: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(len.max(1));
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

/// # Safety
/// `ptr` must come from `wwt_alloc` (or a result) with the same length.
#[no_mangle]
pub unsafe extern "C" fn wwt_free(ptr: *mut u8, len: usize) {
    drop(Vec::from_raw_parts(ptr, 0, len.max(1)));
}

fn result(bytes: Vec<u8>) -> *mut u8 {
    let mut out = Vec::with_capacity(bytes.len() + 4);
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&bytes);
    let mut out = std::mem::ManuallyDrop::new(out);
    out.shrink_to_fit();
    out.as_mut_ptr()
}

/// # Safety
/// The pointers must describe valid buffers in this module's memory.
#[no_mangle]
pub unsafe extern "C" fn wwt_translate(
    code: *const u8,
    code_len: usize,
    base: u32,
    entries: *const u32,
    n_entries: usize,
    known: *const u32,
    n_known: usize,
    opt_level: u32,
    flags: u32,
) -> *mut u8 {
    let code = std::slice::from_raw_parts(code, code_len).to_vec();
    let entries = std::slice::from_raw_parts(entries, n_entries).to_vec();
    let known = if n_known == 0 {
        vec![]
    } else {
        std::slice::from_raw_parts(known, n_known).to_vec()
    };
    let cfg = apply_flags(
        if opt_level == 0 {
            wwt::Config::fast()
        } else {
            wwt::Config::default()
        },
        flags,
    );
    let src = FlatCode { base, bytes: code };
    match wwt::translate::translate_region_with_known(&src, &entries, &known, &cfg) {
        Ok(t) => result(t.wasm),
        Err(_) => result(vec![]),
    }
}

/// Translates a whole PE file ahead of time, with `profile` addresses (code
/// found at run time on earlier launches) as extra entry points. Returns an
/// empty buffer on failure.
///
/// # Safety
/// The pointers must describe valid buffers in this module's memory.
#[no_mangle]
pub unsafe extern "C" fn wwt_translate_pe(
    file: *const u8,
    file_len: usize,
    profile: *const u32,
    n_profile: usize,
    opt_level: u32,
    flags: u32,
) -> *mut u8 {
    let data = std::slice::from_raw_parts(file, file_len).to_vec();
    let profile = if n_profile == 0 {
        vec![]
    } else {
        std::slice::from_raw_parts(profile, n_profile).to_vec()
    };
    let cfg = apply_flags(
        if opt_level == 0 {
            wwt::Config::fast()
        } else {
            wwt::Config::default()
        },
        flags,
    );
    let Ok(pe) = wwt::pe::PeFile::parse(data) else {
        return result(vec![]);
    };
    match wwt::translate_pe(&pe, &cfg, &profile) {
        Ok(t) => result(t.wasm),
        Err(_) => result(vec![]),
    }
}

#[no_mangle]
pub extern "C" fn wwt_kernel() -> *mut u8 {
    result(wwt::kernel::kernel_wasm())
}

#[no_mangle]
pub extern "C" fn wwt_kernel64() -> *mut u8 {
    result(wwt::kernel::kernel64_wasm())
}

#[no_mangle]
pub extern "C" fn wwt_abi() -> *mut u8 {
    result(wwt::abi::abi_json().into_bytes())
}
