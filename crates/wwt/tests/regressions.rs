//! Minimal reproductions of translator bugs found by the test layers.

use wwt::translate::{build_module, translate_snippet};

fn translates(code: &[u8]) {
    let cfg = wwt::Config::default();
    let (f, unsupported) = translate_snippet(code, 0x401000, &cfg);
    assert!(unsupported.is_empty(), "{unsupported:?}");
    let wasm = build_module(&[f], &cfg);
    wasmparser::Validator::new_with_features(wasmparser::WasmFeatures::all())
        .validate_all(&wasm)
        .expect("valid module");
}

/// Csmith seed 1066 at -O0: `jne` to the next instruction made both arms of
/// the branch the same block, which the structurizer emitted twice.
#[test]
fn conditional_jump_to_next_instruction() {
    // cmp eax, ebx; jne $+2; inc eax; ret
    translates(&[0x39, 0xd8, 0x75, 0x00, 0x40, 0xc3]);
}

/// `rep movsd` in the middle of a block: the rest of the block must stay in
/// the same function rather than exit to an address with no entry.
#[test]
fn rep_movs_mid_block() {
    // mov ecx, 4; rep movsd; mov eax, 1; ret
    translates(&[
        0xb9, 0x04, 0x00, 0x00, 0x00, 0xf3, 0xa5, 0xb8, 0x01, 0x00, 0x00, 0x00, 0xc3,
    ]);
}

/// `cmpxchg` whose destination is the accumulator itself.
#[test]
fn cmpxchg_accumulator_destination() {
    // cmpxchg eax, ecx; ret
    translates(&[0x0f, 0xb1, 0xc8, 0xc3]);
}
