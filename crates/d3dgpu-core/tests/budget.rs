//! Steady-state budget: once warmed up, a frame of game-shaped draws
//! (per-draw constants, a few textures and blend states, a dynamic vertex
//! buffer rewritten every frame) creates no pipelines, bind groups,
//! samplers or buffers, and costs one render pass and one submit.

mod common;

use d3dgpu_core::Options;
use d3dgpu_scenes::perf::{frame, setup};
use d3dgpu_scenes::*;

#[test]
fn steady_state_creates_nothing() {
    let Some((device, queue, _)) = common::device(wgpu::Features::empty()) else { return };
    let mut b = Builder::new(64, 64);
    let assets = setup(&mut b);
    for f in 0..3 {
        frame(&mut b, &assets, 150, f);
        b.split();
    }
    let warm = b.finish("warm-up");
    let mut core = d3dgpu_core::Core::with_options(device.clone(), queue.clone(), Options::for_device(&device));
    let mut shared = vec![0u8; 16];
    for batch in &warm.batches {
        core.execute(batch, &shared).unwrap();
    }
    core.wait(&mut shared);
    let before = core.stats();

    let mut b = Builder::new(64, 64);
    let frames = 4;
    for f in 0..frames {
        b.split();
        frame(&mut b, &assets, 150, f + 3);
    }
    // Run only the frames, not the builder's own setup batch.
    let built = b.finish("frames");
    for batch in &built.batches[1..] {
        core.execute(batch, &shared).unwrap();
    }
    core.wait(&mut shared);
    let after = core.stats();
    assert!(common::take_errors().is_empty());
    assert_eq!(after.draws - before.draws, 150 * frames as u64);
    assert_eq!(after.skipped_draws, 0, "{:?}", core.log());
    assert_eq!(after.pipelines_created, before.pipelines_created, "pipelines");
    assert_eq!(after.bind_groups_created, before.bind_groups_created, "bind groups");
    assert_eq!(after.samplers_created, before.samplers_created, "samplers");
    assert_eq!(after.buffers_created, before.buffers_created, "buffers");
    assert_eq!(after.textures_created, before.textures_created, "textures");
    assert_eq!(after.shader_translations, before.shader_translations, "translations");
    assert_eq!(after.submits - before.submits, frames as u64, "one submit per frame");
    assert_eq!(after.passes - before.passes, frames as u64, "one render pass per frame");
}
