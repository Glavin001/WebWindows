//! Every scene in d3dgpu-scenes, on a native adapter held to browser
//! limits, checked against its expected pixels.

mod common;

use d3dgpu_core::Options;
use d3dgpu_scenes::ALL;

fn run_all(features: wgpu::Features, adjust: impl Fn(&mut Options), filter: impl Fn(&str) -> bool) {
    let Some((device, queue, _)) = common::device(features) else { return };
    let out_dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("scenes");
    let _ = std::fs::create_dir_all(&out_dir);
    let mut failures = Vec::new();
    for scene in ALL.iter().filter(|s| filter(s.name)) {
        let built = scene.build(64, 64);
        let mut opts = Options::for_device(&device);
        adjust(&mut opts);
        let r = common::run(&device, &queue, opts, &built);
        common::save_ppm(&out_dir.join(format!("{}.ppm", scene.name)), &r);
        for f in common::check(&built, &r) {
            failures.push(format!("{}: {f}", scene.name));
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures (frames in {}):\n{}",
        failures.len(),
        out_dir.display(),
        failures.join("\n")
    );
}

/// Core WebGPU only: clip planes in the pixel shader, BC decoded on the CPU.
#[test]
fn scenes_core_features() {
    run_all(wgpu::Features::empty(), |_| {}, |_| true);
}

/// With the optional features browsers ship (clip distances, BC), where
/// the adapter has them.
#[test]
fn scenes_optional_features() {
    let f = wgpu::Features::CLIP_DISTANCES | wgpu::Features::TEXTURE_COMPRESSION_BC;
    run_all(f, |_| {}, |n| matches!(n, "clip_plane" | "texture_formats"));
}
