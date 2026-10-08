//! The runtime kernel: a small hand-written module holding the dispatcher
//! loop and the function placed in table slot 0, which handles addresses
//! with no translation (host API thunks and code not yet translated).

use crate::abi::{addr, cpu, cpu64};

pub fn kernel_wat() -> String {
    kernel_wat_for(false)
}

/// The kernel for a 64-bit (memory64) memory: the CPU pointer, the lookup
/// tables and the host imports' CPU argument are i64, and the first-level
/// lookup table holds 8-byte pointers. x86 code addresses stay 32-bit.
pub fn kernel64_wat() -> String {
    kernel_wat_for(true)
}

/// The kernel for x86-64 code on a 64-bit memory, whose code addresses are
/// 64-bit: functions return an i64, eip lives in `cpu64::RIP`, and the
/// lookup clamps the page number to `code_pages` (the first-level table's
/// last entry points at an empty second-level table).
pub fn kernel_code64_wat() -> String {
    format!(
        r#"(module
  (type $fn (func (param i64) (result i64)))
  (import "env" "memory" (memory i64 1 {max_pages} shared))
  (import "env" "table" (table 1 funcref))
  (import "env" "lookup_l1" (global $l1 i64))
  (import "env" "code_pages" (global $pages i64))
  (import "env" "thunk_base" (global $tb i64))
  (import "env" "thunk_size" (global $ts i64))
  (import "env" "host_call" (func $host_call (param i64 i64) (result i64)))
  (import "env" "miss" (func $miss (param i64 i64) (result i32)))

  (func $lookup (export "lookup") (param $t i64) (result i32)
    (local $p i64)
    (local.set $p (i64.shr_u (local.get $t) (i64.const 12)))
    (i32.load
      (i64.add
        (i64.load (i64.add (global.get $l1)
                           (i64.shl (select (local.get $p) (global.get $pages)
                                            (i64.lt_u (local.get $p) (global.get $pages)))
                                    (i64.const 3))))
        (i64.shl (i64.and (local.get $t) (i64.const 0xfff)) (i64.const 2)))))

  (func $miss_entry (export "miss_entry") (type $fn) (param $cpu i64) (result i64)
    (local $t i64)
    (local.set $t (i64.load offset={rip} (local.get $cpu)))
    (if (i64.lt_u (i64.sub (local.get $t) (global.get $tb)) (global.get $ts))
      (then (return (call $host_call (local.get $cpu) (local.get $t)))))
    (return_call_indirect (type $fn) (local.get $cpu)
      (call $miss (local.get $cpu) (local.get $t))))

  (func $resume (export "resume") (type $fn) (param $cpu i64) (result i64)
    (i64.load offset={rip} (local.get $cpu)))

  (func (export "run") (param $cpu i64) (param $eip i64) (result i64)
    (loop $next
      (if (i32.or (i64.eq (local.get $eip) (i64.const {stop}))
                  (i64.eq (local.get $eip) (i64.const {yield_})))
        (then (return (local.get $eip))))
      (i64.store offset={rip} (local.get $cpu) (local.get $eip))
      (local.set $eip
        (call_indirect (type $fn) (local.get $cpu) (call $lookup (local.get $eip))))
      (br $next))
    unreachable)
)"#,
        rip = cpu64::RIP,
        stop = addr::STOP64,
        yield_ = addr::YIELD64,
        max_pages = crate::codegen::MAX_PAGES_64,
    )
}

pub fn kernel_code64_wasm() -> Vec<u8> {
    wat::parse_str(kernel_code64_wat()).expect("kernel WAT is valid")
}

pub fn kernel_wat_for(mem64: bool) -> String {
    if mem64 {
        return format!(
            r#"(module
  (type $fn (func (param i64) (result i32)))
  (import "env" "memory" (memory i64 1 {max_pages} shared))
  (import "env" "table" (table 1 funcref))
  (import "env" "lookup_l1" (global $l1 i64))
  (import "env" "thunk_base" (global $tb i32))
  (import "env" "thunk_size" (global $ts i32))
  (import "env" "host_call" (func $host_call (param i64 i32) (result i32)))
  (import "env" "miss" (func $miss (param i64 i32) (result i32)))

  (func $lookup (export "lookup") (param $t i32) (result i32)
    (i32.load
      (i64.add
        (i64.load (i64.add (global.get $l1)
                           (i64.shl (i64.extend_i32_u (i32.shr_u (local.get $t) (i32.const 12)))
                                    (i64.const 3))))
        (i64.extend_i32_u (i32.shl (i32.and (local.get $t) (i32.const 0xfff)) (i32.const 2))))))

  (func $miss_entry (export "miss_entry") (type $fn) (param $cpu i64) (result i32)
    (local $t i32)
    (local.set $t (i32.load offset={eip} (local.get $cpu)))
    (if (i32.lt_u (i32.sub (local.get $t) (global.get $tb)) (global.get $ts))
      (then (return (call $host_call (local.get $cpu) (local.get $t)))))
    (return_call_indirect (type $fn) (local.get $cpu)
      (call $miss (local.get $cpu) (local.get $t))))

  (func $resume (export "resume") (type $fn) (param $cpu i64) (result i32)
    (i32.load offset={eip} (local.get $cpu)))

  (func (export "run") (param $cpu i64) (param $eip i32) (result i32)
    (loop $next
      (if (i32.or (i32.eq (local.get $eip) (i32.const {stop}))
                  (i32.eq (local.get $eip) (i32.const {yield_})))
        (then (return (local.get $eip))))
      (i32.store offset={eip} (local.get $cpu) (local.get $eip))
      (local.set $eip
        (call_indirect (type $fn) (local.get $cpu) (call $lookup (local.get $eip))))
      (br $next))
    unreachable)
)"#,
            eip = cpu::EIP,
            stop = addr::STOP as i32,
            yield_ = addr::YIELD as i32,
            max_pages = crate::codegen::MAX_PAGES_64,
        );
    }
    format!(
        r#"(module
  (type $fn (func (param i32) (result i32)))
  (import "env" "memory" (memory 1 65536 shared))
  (import "env" "table" (table 1 funcref))
  (import "env" "lookup_l1" (global $l1 i32))
  (import "env" "thunk_base" (global $tb i32))
  (import "env" "thunk_size" (global $ts i32))
  ;; Runs a host (JavaScript) implementation of a Windows API function.
  (import "env" "host_call" (func $host_call (param i32 i32) (result i32)))
  ;; Translates code at an address with no translation; returns its table
  ;; index (the host raises an error when it cannot).
  (import "env" "miss" (func $miss (param i32 i32) (result i32)))

  (func $lookup (export "lookup") (param $t i32) (result i32)
    (i32.load
      (i32.add
        (i32.load (i32.add (global.get $l1)
                           (i32.shl (i32.shr_u (local.get $t) (i32.const 12)) (i32.const 2))))
        (i32.shl (i32.and (local.get $t) (i32.const 0xfff)) (i32.const 2)))))

  ;; Table slot 0. Translated code stores the target in cpu.eip before an
  ;; indirect call, so a lookup miss lands here with the address available.
  (func $miss_entry (export "miss_entry") (type $fn) (param $cpu i32) (result i32)
    (local $t i32)
    (local.set $t (i32.load offset={eip} (local.get $cpu)))
    (if (i32.lt_u (i32.sub (local.get $t) (global.get $tb)) (global.get $ts))
      (then (return (call $host_call (local.get $cpu) (local.get $t)))))
    (return_call_indirect (type $fn) (local.get $cpu)
      (call $miss (local.get $cpu) (local.get $t))))

  ;; Table slot 1: continues at cpu.eip. The host returns this slot from a
  ;; miss it turns into an exception, after pointing cpu.eip at the
  ;; exception dispatcher.
  (func $resume (export "resume") (type $fn) (param $cpu i32) (result i32)
    (i32.load offset={eip} (local.get $cpu)))

  ;; Runs guest code from `eip` until it returns to the stop address, or
  ;; the host hands back the yield address to switch threads.
  (func (export "run") (param $cpu i32) (param $eip i32) (result i32)
    (loop $next
      (if (i32.or (i32.eq (local.get $eip) (i32.const {stop}))
                  (i32.eq (local.get $eip) (i32.const {yield_})))
        (then (return (local.get $eip))))
      (i32.store offset={eip} (local.get $cpu) (local.get $eip))
      (local.set $eip
        (call_indirect (type $fn) (local.get $cpu) (call $lookup (local.get $eip))))
      (br $next))
    unreachable)
)"#,
        eip = cpu::EIP,
        stop = addr::STOP as i32,
        yield_ = addr::YIELD as i32,
    )
}

pub fn kernel_wasm() -> Vec<u8> {
    wat::parse_str(kernel_wat()).expect("kernel WAT is valid")
}

pub fn kernel64_wasm() -> Vec<u8> {
    wat::parse_str(kernel64_wat()).expect("kernel WAT is valid")
}
