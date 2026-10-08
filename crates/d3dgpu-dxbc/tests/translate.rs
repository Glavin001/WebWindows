//! Every fixture (HLSL compiled to DXBC by vkd3d, see tools/dxbc/compile.sh)
//! must parse and translate to WGSL that Naga accepts with only the
//! capabilities browsers ship.

use d3dgpu_dxbc::*;

fn fixtures() -> Vec<(String, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "dxbc"))
        .map(|e| (e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).unwrap()))
        .collect();
    v.sort();
    v
}

fn numbered(src: &str) -> String {
    src.lines().enumerate().map(|(i, l)| format!("{:4} {l}\n", i + 1)).collect()
}

pub fn validate(name: &str, src: &str, caps: naga::valid::Capabilities) {
    let module = match naga::front::wgsl::parse_str(src) {
        Ok(m) => m,
        Err(e) => panic!("{name}: WGSL parse error:\n{}\n{}", e.emit_to_string(src), numbered(src)),
    };
    if let Err(e) = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), caps).validate(&module) {
        panic!("{name}: WGSL validation error:\n{}\n{}", e.emit_to_string(src), numbered(src));
    }
}

#[test]
fn fixtures_translate_and_validate() {
    let fx = fixtures();
    assert!(fx.len() >= 14, "fixtures missing");
    for (name, bytes) in &fx {
        let sh = Shader::from_dxbc(bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        let key = Key { pos_fixup: true, ..Default::default() };
        let t = sh.translate(&key).unwrap_or_else(|e| panic!("{name}: {e}\n{}", sh.disassemble()));
        validate(name, &t.wgsl, caps());
    }
}

#[test]
fn depth_and_typed_resource_keys() {
    for (name, bytes) in fixtures().iter().filter(|(n, _)| n.starts_with("textures.ps") || n.starts_with("compute")) {
        let sh = Shader::from_dxbc(bytes).unwrap();
        let key = Key {
            srvs: vec![(5, SrvKind::Texture { depth: true }), (7, SrvKind::Buffer(BufferFormat::Rgba8Unorm))],
            uavs: vec![(2, UavKind::Texture(StorageFormat::Rgba16Float)), (3, UavKind::Buffer(BufferFormat::R32Uint))],
            ..Default::default()
        };
        let t = sh.translate(&key).unwrap_or_else(|e| panic!("{name}: {e}"));
        validate(name, &t.wgsl, caps());
    }
}

#[test]
fn vertex_linkage_follows_the_pixel_shader() {
    let fx = fixtures();
    let get = |n: &str| Shader::from_dxbc(&fx.iter().find(|(f, _)| f == n).unwrap().1).unwrap();
    for (vs, ps) in
        [("basic.vs.vs_5_0.dxbc", "basic.ps.ps_5_0.dxbc"), ("outputs.vs.vs_5_0.dxbc", "outputs.ps.ps_5_0.dxbc")]
    {
        let (v, p) = (get(vs), get(ps));
        let link = p.linkage();
        assert!(!link.is_empty());
        let key = Key { outputs: Some(link.clone()), pos_fixup: true, ..Default::default() };
        let vt = v.translate(&key).unwrap();
        validate(vs, &vt.wgsl, caps());
        let pt = p.translate(&Key::default()).unwrap();
        assert_eq!(vt.varyings, pt.varyings, "{vs} -> {ps}");
        // Integer varyings are flat on both sides.
        for (k, slot) in link.iter().enumerate() {
            let attr =
                format!("@location({k}){}", if slot.interp == Interp::Flat { " @interpolate(flat)" } else { "" });
            assert!(vt.wgsl.contains(&attr) && pt.wgsl.contains(&attr), "{vs}/{ps}: {attr}");
        }
    }
}

#[test]
fn reflection_of_the_compute_fixture() {
    let fx = fixtures();
    let (_, bytes) = fx.iter().find(|(n, _)| n.starts_with("compute")).unwrap();
    let sh = Shader::from_dxbc(bytes).unwrap();
    let r = &sh.reflection;
    assert_eq!(r.thread_group, [64, 1, 1]);
    assert!(r.uav(0).unwrap().atomic);
    assert!(r.uav(2).unwrap().written);
    assert_eq!(r.tgsm.len(), 1);
    let t = sh.translate(&Key::default()).unwrap();
    assert_eq!(t.workgroup_size, [64, 1, 1]);
    assert!(t.bindings.iter().any(|b| b.binding == UAV_BASE_PUB + 2));
}

const UAV_BASE_PUB: u32 = d3dgpu_dxbc::wgsl::UAV_BASE;

/// Core WGSL as browsers implement it (Naga gates pack2x16float and friends
/// behind a capability).
fn caps() -> naga::valid::Capabilities {
    naga::valid::Capabilities::SHADER_FLOAT16_IN_FLOAT32
        | naga::valid::Capabilities::MULTISAMPLED_SHADING
        | naga::valid::Capabilities::CUBE_ARRAY_TEXTURES
}
