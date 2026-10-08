//! Guest memory and the runtime's virtual memory.
//!
//! In WebAssembly the module imports the machine's memory, so a guest
//! address is a plain pointer. Everything the heap keeps lives in guest
//! memory, in the heaps themselves, except a few process-wide values at
//! [`CTX`], in the guest's null region: guest code faults there, Wine maps
//! nothing there, and the module's own stack sits below it (`build.rs`).
//!
//! On the host (unit tests) a thread-local buffer stands in for guest
//! memory and a bump allocator for the runtime's virtual memory.

/// Process-wide values (see `lib.rs`).
pub const CTX: u32 = 0x8000;

#[cfg(target_arch = "wasm32")]
mod imp {
    #[link(wasm_import_module = "env")]
    extern "C" {
        /// Reserves and commits `size` bytes (a multiple of 64 KB) of guest
        /// address space through the runtime's virtual memory manager;
        /// returns the base, or 0.
        #[link_name = "heap_vm_alloc"]
        fn vm_alloc_import(size: u32, prot: u32) -> u32;
        /// Releases a region `vm_alloc` returned.
        #[link_name = "heap_vm_free"]
        fn vm_free_import(base: u32);
    }

    #[inline(always)]
    pub fn ld(a: u32) -> u32 {
        unsafe { (a as usize as *const u32).read() }
    }
    #[inline(always)]
    pub fn st(a: u32, v: u32) {
        unsafe { (a as usize as *mut u32).write(v) }
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
        unsafe { (a as usize as *const u16).read() as u32 }
    }
    #[inline(always)]
    pub fn st16(a: u32, v: u32) {
        unsafe { (a as usize as *mut u16).write(v as u16) }
    }
    pub fn fill(a: u32, v: u8, n: u32) {
        unsafe { core::ptr::write_bytes(a as usize as *mut u8, v, n as usize) }
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
    pub fn vm_alloc(size: u32, prot: u32) -> u32 {
        unsafe { vm_alloc_import(size, prot) }
    }
    pub fn vm_free(base: u32) {
        unsafe { vm_free_import(base) }
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use std::cell::RefCell;

    /// Size of the test guest memory.
    pub const TEST_MEM: u32 = 256 << 20;
    /// Where test virtual memory starts.
    const VM_BASE: u32 = 0x10_0000;

    struct Mem {
        bytes: Vec<u8>,
        next: u32,
        /// Live regions: base -> size.
        regions: std::collections::BTreeMap<u32, u32>,
    }

    thread_local! {
        static MEM: RefCell<Mem> = RefCell::new(Mem {
            bytes: vec![0; TEST_MEM as usize],
            next: VM_BASE,
            regions: Default::default(),
        });
    }

    fn with<R>(f: impl FnOnce(&mut Mem) -> R) -> R {
        MEM.with(|m| f(&mut m.borrow_mut()))
    }

    pub fn ld(a: u32) -> u32 {
        with(|m| u32::from_le_bytes(m.bytes[a as usize..a as usize + 4].try_into().unwrap()))
    }
    pub fn st(a: u32, v: u32) {
        with(|m| m.bytes[a as usize..a as usize + 4].copy_from_slice(&v.to_le_bytes()))
    }
    pub fn ld8(a: u32) -> u32 {
        with(|m| m.bytes[a as usize] as u32)
    }
    pub fn st8(a: u32, v: u32) {
        with(|m| m.bytes[a as usize] = v as u8)
    }
    pub fn ld16(a: u32) -> u32 {
        ld8(a) | ld8(a + 1) << 8
    }
    pub fn st16(a: u32, v: u32) {
        st8(a, v & 0xff);
        st8(a + 1, v >> 8 & 0xff);
    }
    pub fn fill(a: u32, v: u8, n: u32) {
        with(|m| m.bytes[a as usize..(a + n) as usize].fill(v))
    }
    pub fn copy(dst: u32, src: u32, n: u32) {
        with(|m| {
            m.bytes
                .copy_within(src as usize..(src + n) as usize, dst as usize)
        })
    }
    pub fn vm_alloc(size: u32, _prot: u32) -> u32 {
        assert_eq!(size % 0x10000, 0, "regions are whole 64 KB units");
        with(|m| {
            if m.next + size > TEST_MEM {
                return 0;
            }
            let base = m.next;
            m.next += size;
            m.regions.insert(base, size);
            m.bytes[base as usize..(base + size) as usize].fill(0);
            base
        })
    }
    pub fn vm_free(base: u32) {
        with(|m| {
            let size = m
                .regions
                .remove(&base)
                .expect("freeing a region vm_alloc returned");
            // Catch use after release.
            m.bytes[base as usize..(base + size) as usize].fill(0xcc);
        })
    }

    /// Bytes of live regions.
    #[cfg(test)]
    pub fn vm_used() -> u32 {
        with(|m| m.regions.values().sum())
    }
}

pub use imp::*;
