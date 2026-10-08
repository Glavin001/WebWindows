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

/// Fast mode (`mem_traps`): the module, and its `wwt.traps` section as
/// (module offset, x86 address, flags, displacement).
fn trap_module(code: &[u8]) -> (Vec<u8>, Vec<(u32, u32, u32, u32)>) {
    let mut cfg = wwt::Config::default();
    cfg.codegen.mem_traps = true;
    let (f, unsupported) = translate_snippet(code, 0x401000, &cfg);
    assert!(unsupported.is_empty(), "{unsupported:?}");
    let src = wwt::discover::FlatCode {
        base: 0x401000,
        bytes: code.to_vec(),
    };
    let funcs = [f];
    let wasm = wwt::codegen::ModuleGen::new(&cfg.codegen, &funcs)
        .without_direct_calls()
        .with_code(&src)
        .build(&funcs, "{}");
    wasmparser::Validator::new_with_features(wasmparser::WasmFeatures::all())
        .validate_all(&wasm)
        .expect("valid module");
    let mut sites = Vec::new();
    for payload in wasmparser::Parser::new(0).parse_all(&wasm) {
        if let Ok(wasmparser::Payload::CustomSection(c)) = payload {
            if c.name() == wwt::abi::TRAPS_SECTION {
                let d = c.data();
                let u = |k: usize| u32::from_le_bytes(d[k..k + 4].try_into().unwrap());
                for i in 0..u(0) as usize {
                    let e = 4 + i * 16;
                    sites.push((u(e), u(e + 4), u(e + 8), u(e + 12)));
                }
            }
        }
    }
    (wasm, sites)
}

#[test]
fn trapping_accesses_replace_checks_and_are_mapped() {
    use wwt::abi::trap::*;
    // mov eax, [ecx]; add eax, [ecx+4]; mov [edx+4], eax;
    // mov eax, [ecx+esi*4+0x9000]; ret
    let code = [
        0x8b, 0x01, 0x03, 0x41, 0x04, 0x89, 0x42, 0x04, 0x8b, 0x84, 0xb1, 0x00, 0x90, 0x00, 0x00,
        0xc3,
    ];
    let (wasm, sites) = trap_module(&code);
    let text = wasmprinter::print_bytes(&wasm).unwrap();
    // Only the store's store-map path keeps a precise check.
    assert_eq!(
        text.lines().filter(|l| l.trim() == "global.get 2").count(),
        1
    );
    // eax = 1, ecx = 2, edx = 3, esi = 7.
    let operand = |base: u32, index: u32, scale: u32| {
        OPERAND | base << BASE_SHIFT | index << INDEX_SHIFT | scale << SCALE_SHIFT
    };
    assert_eq!(
        sites.iter().map(|s| (s.1, s.2, s.3)).collect::<Vec<_>>(),
        [
            (0x401000, operand(2, 0, 0), 0),
            (0x401002, operand(2, 0, 0), 4),
            (0x401005, WRITE | operand(3, 0, 0), 4),
            (0x401008, operand(2, 7, 2), 0x9000),
        ]
    );
    // Each offset is a load or store: with the null region's offset, but
    // for the access the first one covers (which traps only above the top
    // of memory).
    let mut found = 0;
    for p in wasmparser::Parser::new(0).parse_all(&wasm) {
        if let Ok(wasmparser::Payload::CodeSectionEntry(body)) = p {
            let mut r = body.get_operators_reader().unwrap();
            while !r.eof() {
                let (op, at) = r.read_with_offset().unwrap();
                if sites.iter().any(|s| s.0 as u64 == at as u64) {
                    let s = format!("{op:?}");
                    assert!(s.contains("Load") || s.contains("Store"), "{s}");
                    assert_eq!(s.contains("offset: 65536"), found != 1, "{s}");
                    found += 1;
                }
            }
        }
    }
    assert_eq!(found, sites.len());
}
