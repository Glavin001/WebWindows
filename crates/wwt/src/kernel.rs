//! The runtime kernel: a small hand-written module holding the dispatcher
//! loop and the function placed in table slot 0, which handles addresses
//! with no translation (host API thunks and code not yet translated).

use crate::abi::{addr, cpu};

pub fn kernel_wat() -> String {
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

  ;; Runs guest code from `eip` until it returns to the stop address.
  (func (export "run") (param $cpu i32) (param $eip i32) (result i32)
    (loop $next
      (if (i32.eq (local.get $eip) (i32.const {stop}))
        (then (return (local.get $eip))))
      (i32.store offset={eip} (local.get $cpu) (local.get $eip))
      (local.set $eip
        (call_indirect (type $fn) (local.get $cpu) (call $lookup (local.get $eip))))
      (br $next))
    unreachable)
)"#,
        eip = cpu::EIP,
        stop = addr::STOP as i32,
    )
}

pub fn kernel_wasm() -> Vec<u8> {
    wat::parse_str(kernel_wat()).expect("kernel WAT is valid")
}
