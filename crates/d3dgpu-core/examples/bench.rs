//! CPU cost per draw of game-shaped frames (500, 2,000 and 5,000 draws),
//! natively. Native wgpu is the ceiling; the browser number is the one
//! that counts.
//!
//! cargo run --release -p d3dgpu-core --example bench

use std::time::Instant;

use d3dgpu_core::{Core, Options};
use d3dgpu_scenes::perf::{frame, frame11, setup, setup11};
use d3dgpu_scenes::Builder;

fn main() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&Default::default())).expect("adapter");
    println!("{:?} ({:?})", adapter.get_info().name, adapter.get_info().backend);
    let (device, queue) = pollster::block_on(adapter.request_device(&Default::default())).expect("device");
    for (api, draws) in [500u32, 2000, 5000].iter().flat_map(|d| [("d3d9", *d), ("d3d11", *d)]) {
        let frames = 20;
        let mut b = if api == "d3d9" { Builder::new(640, 480) } else { Builder::new_d3d11(640, 480) };
        if api == "d3d9" {
            let assets = setup(&mut b);
            for f in 0..frames {
                b.split();
                frame(&mut b, &assets, draws, f);
            }
        } else {
            let assets = setup11(&mut b);
            for f in 0..frames {
                b.split();
                frame11(&mut b, &assets, draws, f);
            }
        }
        let built = b.finish("bench");
        let mut core = Core::with_options(device.clone(), queue.clone(), Options::for_device(&device));
        core.set_profiling(true);
        let mut shared = vec![0u8; 16];
        // Warm up on the setup batch and the first frame.
        core.execute(&built.batches[0], &shared).unwrap();
        core.execute(&built.batches[1], &shared).unwrap();
        core.wait(&mut shared);
        let mut cpu = std::time::Duration::ZERO;
        let s0 = core.stats();
        let t_all = Instant::now();
        for batch in &built.batches[2..] {
            let t = Instant::now();
            core.execute(batch, &shared).unwrap();
            cpu += t.elapsed();
            core.wait(&mut shared);
        }
        let n = (frames - 1) as f64;
        let s1 = core.stats();
        let draw_us = (s1.draw_ns - s0.draw_ns) as f64 / 1e3 / (n * draws as f64);
        let submit_ms = (s1.submit_ns - s0.submit_ns) as f64 / 1e6 / n;
        println!(
            "{api:5} {draws:5} draws/frame: {draw_us:5.2} us per draw recorded by the core, {submit_ms:6.2} ms finish+submit per frame, \
             {:6.2} ms execute per frame, {:6.1} ms wall per frame incl. GPU; {} pipelines, {} bind groups in total; {:.1} pass commands per draw ({:.1} skipped)",
            cpu.as_secs_f64() * 1e3 / n,
            t_all.elapsed().as_secs_f64() * 1e3 / n,
            s1.pipelines_created,
            s1.bind_groups_created,
            (s1.pass_commands - s0.pass_commands) as f64 / (n * draws as f64),
            (s1.pass_commands_skipped - s0.pass_commands_skipped) as f64 / (n * draws as f64),
        );
    }
}
