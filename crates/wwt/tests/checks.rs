//! Which guest-memory checks code generation emits: accesses near an
//! address that already passed a check skip theirs (stores only outside
//! their store-map path), and nothing else does.

use wwt::translate::{build_module, translate_snippet};

/// Number of guest-limit checks (reads of the `guest_limit` global) in the
/// module translated from `code`.
fn guest_checks(code: &[u8]) -> usize {
    let cfg = wwt::Config::default();
    let (f, unsupported) = translate_snippet(code, 0x401000, &cfg);
    assert!(unsupported.is_empty(), "{unsupported:?}");
    let wasm = build_module(&[f], &cfg);
    wasmparser::Validator::new_with_features(wasmparser::WasmFeatures::all())
        .validate_all(&wasm)
        .expect("valid module");
    let text = wasmprinter::print_bytes(&wasm).unwrap();
    text.lines().filter(|l| l.trim() == "global.get 2").count()
}

#[test]
fn nearby_loads_share_a_check() {
    // mov eax, [ecx]; add eax, [ecx+4]; add eax, [ecx+8]; ret
    assert_eq!(
        guest_checks(&[0x8b, 0x01, 0x03, 0x41, 0x04, 0x03, 0x41, 0x08, 0xc3]),
        1
    );
}

#[test]
fn redefined_base_is_checked_again() {
    // mov eax, [ecx]; mov ecx, [edx]; add eax, [ecx+4]; ret
    assert_eq!(
        guest_checks(&[0x8b, 0x01, 0x8b, 0x0a, 0x03, 0x41, 0x04, 0xc3]),
        3
    );
}

#[test]
fn distant_load_is_checked() {
    // mov eax, [ecx]; add eax, [ecx+0x9000]; ret
    assert_eq!(
        guest_checks(&[0x8b, 0x01, 0x03, 0x81, 0x00, 0x90, 0x00, 0x00, 0xc3]),
        2
    );
}

#[test]
fn bounded_index_shares_the_base_check() {
    // mov eax, [ecx]; movzx edx, al; add eax, [ecx+edx*4]; ret
    // edx*4 is at most 1020: within the window of the first check.
    assert_eq!(
        guest_checks(&[0x8b, 0x01, 0x0f, 0xb6, 0xd0, 0x03, 0x04, 0x91, 0xc3]),
        1
    );
}

#[test]
fn unbounded_index_is_checked() {
    // mov eax, [ecx]; mov edx, eax; add eax, [ecx+edx*4]; ret
    assert_eq!(
        guest_checks(&[0x8b, 0x01, 0x89, 0xc2, 0x03, 0x04, 0x91, 0xc3]),
        2
    );
}

#[test]
fn negative_displacements_share_a_check() {
    // mov eax, [ecx-8]; add eax, [ecx-4]; ret
    assert_eq!(guest_checks(&[0x8b, 0x41, 0xf8, 0x03, 0x41, 0xfc, 0xc3]), 1);
}

#[test]
fn store_check_covers_later_loads_and_stores() {
    // mov [ecx], eax; mov eax, [ecx+4]; mov [ecx+8], eax; ret
    // Each store keeps the precise check on its store-map path (taken only
    // while code is writable); otherwise the first store's check covers the
    // load and the second store: 2 + 1.
    assert_eq!(
        guest_checks(&[0x89, 0x01, 0x8b, 0x41, 0x04, 0x89, 0x41, 0x08, 0xc3]),
        3
    );
}
