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

/// Wine's kernel32 tests: data decoded as code produced `jmp far [m]`, whose
/// 6-byte far pointer reached code generation as an ordinary load. Far
/// pointers now raise a general protection fault (no unsupported
/// instruction), and the module must still be valid.
#[test]
fn far_pointer_operands_fault() {
    let cfg = wwt::Config::default();
    // jmp far [eax]; call far [eax]; lfs eax, [ecx]
    for code in [
        &[0xff, 0x28][..],
        &[0xff, 0x18, 0xc3],
        &[0x0f, 0xb4, 0x01, 0xc3],
    ] {
        let (f, unsupported) = translate_snippet(code, 0x401000, &cfg);
        assert!(unsupported.is_empty(), "{code:x?}: {unsupported:?}");
        let wasm = build_module(&[f], &cfg);
        wasmparser::Validator::new_with_features(wasmparser::WasmFeatures::all())
            .validate_all(&wasm)
            .expect("valid module");
    }
}
