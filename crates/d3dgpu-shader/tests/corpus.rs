//! Stress test over the shader model 1-3 bytecode embedded in Wine's
//! Direct3D 8/9 tests (tools/dxbc/fetch-wine-d3d9-shaders.sh puts it in
//! target/d3d9-corpus; the test passes trivially when it's absent). Some of
//! those shaders are deliberately invalid, so parse errors are counted, not
//! failures; every shader that parses must translate to WGSL Naga accepts
//! with browser capabilities, or be `Unsupported` for a reason listed.

use std::collections::BTreeMap;

use d3dgpu_shader::*;

#[test]
fn wine_d3d9_corpus() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/d3d9-corpus");
    let Ok(entries) = std::fs::read_dir(dir) else {
        eprintln!("no corpus (run tools/dxbc/fetch-wine-d3d9-shaders.sh)");
        return;
    };
    let mut files: Vec<_> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "d3dbc"))
        .collect();
    files.sort();
    let mut ok = 0;
    let mut rejected = 0;
    let mut unsupported: BTreeMap<String, usize> = BTreeMap::new();
    let mut failures = Vec::new();
    for path in &files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let bytes = std::fs::read(path).unwrap();
        let m = match ShaderModule::from_bytes(&bytes) {
            Ok(m) => m,
            Err(Error::Unsupported(r)) => {
                *unsupported.entry(r).or_default() += 1;
                continue;
            }
            Err(_) => {
                rejected += 1;
                continue;
            }
        };
        let out = match m.stage() {
            Stage::Vertex => {
                let mut key = VertexKey { pos_fixup: true, ..Default::default() };
                key.outputs = m
                    .reflection
                    .vs_outputs
                    .iter()
                    .filter(|s| s.usage != bytecode::usage::POSITION)
                    .map(|s| Varying { semantic: *s, interp: Interp::Perspective })
                    .collect();
                m.vertex(&key)
            }
            Stage::Pixel => m.pixel(&PixelKey::default()),
        };
        let t = match out {
            Ok(t) => t,
            Err(Error::Unsupported(r)) => {
                *unsupported.entry(r).or_default() += 1;
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
                failures.push(format!("{name}: {}", e.emit_to_string(&t.wgsl)));
                continue;
            }
        };
        let caps = naga::valid::Capabilities::empty();
        if let Err(e) = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), caps).validate(&module) {
            failures.push(format!("{name}: {}", e.emit_to_string(&t.wgsl)));
            continue;
        }
        ok += 1;
    }
    eprintln!(
        "{} shaders: {ok} translate and validate, {rejected} rejected by the parser, unsupported: {unsupported:?}",
        files.len()
    );
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n\n"));
}
