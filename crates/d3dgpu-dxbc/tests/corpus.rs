//! Stress test over fxc-compiled shaders from Wine's Direct3D 10/11 tests
//! (tools/dxbc/fetch-wine-shaders.sh puts them in target/dxbc-corpus; the
//! test passes trivially when they're absent). Every shader must decode;
//! every vertex, pixel and compute shader must translate to valid WGSL or
//! fail with `Unsupported` for a reason the summary lists.

use std::collections::BTreeMap;

use d3dgpu_dxbc::*;

#[test]
fn wine_corpus() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/dxbc-corpus");
    let Ok(entries) = std::fs::read_dir(dir) else {
        eprintln!("no corpus (run tools/dxbc/fetch-wine-shaders.sh)");
        return;
    };
    let mut files: Vec<_> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "dxbc"))
        .collect();
    files.sort();
    let caps = naga::valid::Capabilities::SHADER_FLOAT16_IN_FLOAT32
        | naga::valid::Capabilities::MULTISAMPLED_SHADING
        | naga::valid::Capabilities::CUBE_ARRAY_TEXTURES;
    let mut ok = 0;
    let mut unsupported: BTreeMap<String, usize> = BTreeMap::new();
    let mut failures = Vec::new();
    for path in &files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let bytes = std::fs::read(path).unwrap();
        let sh = match Shader::from_dxbc(&bytes) {
            Ok(s) => s,
            Err(Error::Unsupported(m)) => {
                *unsupported.entry(m).or_default() += 1;
                continue;
            }
            Err(e) => {
                failures.push(format!("{name}: decode: {e}"));
                continue;
            }
        };
        let t = match sh.translate(&Key { pos_fixup: true, ..Default::default() }) {
            Ok(t) => t,
            Err(Error::Unsupported(m)) => {
                *unsupported.entry(m).or_default() += 1;
                continue;
            }
            Err(e) => {
                failures.push(format!("{name}: translate: {e}"));
                continue;
            }
        };
        let module = match naga::front::wgsl::parse_str(&t.wgsl) {
            Ok(m) => m,
            Err(e) => {
                failures.push(format!("{name}: {}\n{}", e.emit_to_string(&t.wgsl), sh.disassemble()));
                continue;
            }
        };
        match naga::valid::Validator::new(naga::valid::ValidationFlags::all(), caps).validate(&module) {
            Ok(_) => ok += 1,
            Err(e) => failures.push(format!("{name}: {}\n{}", e.emit_to_string(&t.wgsl), sh.disassemble())),
        }
    }
    eprintln!("{} shaders: {ok} valid WGSL", files.len());
    for (m, n) in &unsupported {
        eprintln!("  unsupported ({n}): {m}");
    }
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n\n"));
}
