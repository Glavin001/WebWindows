//! Every animated demo for a few frames on a native adapter held to
//! browser limits: no validation errors, no skipped draws, and a frame
//! with real content (many distinct colours). Frames are saved as PPM.

mod common;

use d3dgpu_core::{Core, HeadlessPresenter, Options};
use d3dgpu_scenes::demos::DEMOS;
use d3dgpu_scenes::WINDOW;

#[test]
fn demos_render() {
    let Some((device, queue, _)) = common::device(wgpu::Features::empty()) else { return };
    let out_dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("demos");
    let _ = std::fs::create_dir_all(&out_dir);
    let mut failures = Vec::new();
    for demo in DEMOS {
        // Small sizes: lavapipe renders on the CPU.
        let param = demo.param.map(|p| {
            p.default.min(match p.label {
                "particles" => 8192,
                "instances" => 2000,
                _ => 200,
            })
        });
        let (mut b, mut d) = demo.start(320, 240, param);
        let mut core = Core::with_options(device.clone(), queue.clone(), Options::for_device(&device));
        let mut shared = vec![0u8; 16];
        core.execute(&b.take_batch(), &shared).unwrap();
        for f in 0..4 {
            d.frame(&mut b, f as f32 * 0.4);
            core.execute(&b.take_batch(), &shared).unwrap();
        }
        core.wait(&mut shared);
        let p = core.presenter(WINDOW).expect("demo presented");
        let headless = p.as_any().downcast_mut::<HeadlessPresenter>().unwrap();
        let (w, h, pixels) = headless.read_pixels(&device, &queue).expect("frame");
        let run = common::Run { width: w, height: h, pixels, shared, core };
        common::save_ppm(&out_dir.join(format!("{}.ppm", demo.name)), &run);
        for e in common::take_errors() {
            failures.push(format!("{}: WebGPU error: {e}", demo.name));
        }
        let stats = run.core.stats();
        if stats.errors != 0 || stats.skipped_draws != 0 {
            failures.push(format!("{}: core reported problems: {:?}", demo.name, run.core.log()));
        }
        let mut colors: Vec<[u8; 4]> = run.pixels.as_chunks::<4>().0.to_vec();
        colors.sort();
        colors.dedup();
        let min = if demo.name.starts_with("perf") { 4 } else { 16 };
        if colors.len() < min {
            failures.push(format!("{}: only {} distinct colours", demo.name, colors.len()));
        }
    }
    assert!(failures.is_empty(), "frames in {}:\n{}", out_dir.display(), failures.join("\n"));
}
