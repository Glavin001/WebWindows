//! Guest memory.
//!
//! In WebAssembly the module imports the machine's memory, so a guest
//! address is a plain pointer. On the host (unit tests) a thread-local
//! buffer stands in for it.

#[cfg(target_arch = "wasm32")]
mod imp {
    #[inline(always)]
    pub fn ld(a: u32) -> u32 {
        unsafe { (a as usize as *const u32).read_unaligned() }
    }
    #[inline(always)]
    pub fn st(a: u32, v: u32) {
        unsafe { (a as usize as *mut u32).write_unaligned(v) }
    }
    #[inline(always)]
    pub fn ld8(a: u32) -> u32 {
        unsafe { (a as usize as *const u8).read() as u32 }
    }
    #[inline(always)]
    pub fn st8(a: u32, v: u32) {
        unsafe { (a as usize as *mut u8).write(v as u8) }
    }
    #[inline(always)]
    pub fn ld16(a: u32) -> u32 {
        unsafe { (a as usize as *const u16).read_unaligned() as u32 }
    }
    #[inline(always)]
    pub fn ld64(a: u32) -> u64 {
        unsafe { (a as usize as *const u64).read_unaligned() }
    }
    pub fn copy(dst: u32, src: u32, n: u32) {
        unsafe {
            core::ptr::copy(
                src as usize as *const u8,
                dst as usize as *mut u8,
                n as usize,
            )
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use std::cell::RefCell;

    /// Size of the test guest memory.
    pub const TEST_MEM: u32 = 64 << 20;

    thread_local! {
        static MEM: RefCell<Vec<u8>> = RefCell::new(vec![0; TEST_MEM as usize]);
    }

    fn with<R>(f: impl FnOnce(&mut Vec<u8>) -> R) -> R {
        MEM.with(|m| f(&mut m.borrow_mut()))
    }

    pub fn ld(a: u32) -> u32 {
        with(|m| u32::from_le_bytes(m[a as usize..a as usize + 4].try_into().unwrap()))
    }
    pub fn st(a: u32, v: u32) {
        with(|m| m[a as usize..a as usize + 4].copy_from_slice(&v.to_le_bytes()))
    }
    pub fn ld8(a: u32) -> u32 {
        with(|m| m[a as usize] as u32)
    }
    pub fn st8(a: u32, v: u32) {
        with(|m| m[a as usize] = v as u8)
    }
    pub fn ld16(a: u32) -> u32 {
        ld8(a) | ld8(a + 1) << 8
    }
    pub fn ld64(a: u32) -> u64 {
        ld(a) as u64 | (ld(a + 4) as u64) << 32
    }
    pub fn copy(dst: u32, src: u32, n: u32) {
        with(|m| m.copy_within(src as usize..(src + n) as usize, dst as usize))
    }
    /// Copies host bytes into test memory.
    #[cfg(test)]
    pub fn put(a: u32, bytes: &[u8]) {
        with(|m| m[a as usize..a as usize + bytes.len()].copy_from_slice(bytes))
    }
}

pub use imp::*;
