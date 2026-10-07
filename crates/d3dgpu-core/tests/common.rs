//! Shared helpers for the native GPU tests: a device held to WebGPU's
//! default limits, and a scene runner.

#![allow(dead_code)]

use d3dgpu_core::{Core, HeadlessPresenter, Options};
use d3dgpu_scenes::{Built, Expect, WINDOW};

/// A device on the first native adapter with WebGPU's default limits and
/// only `features` (those the adapter has). `None` when there is no GPU
/// (set `D3DGPU_REQUIRE_GPU=1` to make that an error).
pub fn device(features: wgpu::Features) -> Option<(wgpu::Device, wgpu::Queue, wgpu::Features)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())) {
        Ok(a) => a,
        Err(e) => {
            assert!(std::env::var("D3DGPU_REQUIRE_GPU").is_err(), "no GPU adapter: {e}");
            eprintln!("skipping: no GPU adapter ({e})");
            return None;
        }
    };
    let features = features & adapter.features();
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("d3dgpu test"),
        required_features: features,
        // Browsers give WebGPU's defaults; native adapters offer more, and
        // tests that use more would pass things Chrome rejects.
        required_limits: wgpu::Limits::default(),
        ..Default::default()
    }))
    .expect("device");
    // Collect validation errors instead of panicking, so a failing scene
    // is reported by name.
    let errors = ERRORS.get_or_init(Default::default).clone();
    device.on_uncaptured_error(std::sync::Arc::new(move |e: wgpu::Error| errors.lock().unwrap().push(e.to_string())));
    Some((device, queue, features))
}

static ERRORS: std::sync::OnceLock<std::sync::Arc<std::sync::Mutex<Vec<String>>>> = std::sync::OnceLock::new();

/// WebGPU validation errors since the last call.
pub fn take_errors() -> Vec<String> {
    ERRORS.get().map(|e| std::mem::take(&mut *e.lock().unwrap())).unwrap_or_default()
}

pub struct Run {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    pub shared: Vec<u8>,
    pub core: Core,
}

/// Executes a built scene and returns the presented frame.
pub fn run(device: &wgpu::Device, queue: &wgpu::Queue, opts: Options, built: &Built) -> Run {
    let mut core = Core::with_options(device.clone(), queue.clone(), opts);
    let mut shared = vec![0u8; built.shared_size.max(16)];
    for batch in &built.batches {
        core.execute(batch, &shared).expect("batch decodes");
        core.poll(&mut shared);
    }
    core.wait(&mut shared);
    let p = core.presenter(WINDOW).expect("scene presented");
    let headless = p.as_any().downcast_mut::<HeadlessPresenter>().unwrap();
    let (width, height, pixels) = headless.read_pixels(device, queue).unwrap_or((
        built.width,
        built.height,
        vec![0; (built.width * built.height * 4) as usize],
    ));
    Run { width, height, pixels, shared, core }
}

/// Checks a run against the scene's expectations; returns the failures.
pub fn check(built: &Built, r: &Run) -> Vec<String> {
    let mut fails = Vec::new();
    let px = |x: i32, y: i32| -> [u8; 4] {
        let i = ((y as u32 * r.width + x as u32) * 4) as usize;
        [r.pixels[i], r.pixels[i + 1], r.pixels[i + 2], r.pixels[i + 3]]
    };
    let close = |a: [u8; 4], b: [u8; 4], tol: u8| a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= tol);
    for e in &built.expect {
        match e {
            Expect::Pixels { rect, rgba, tolerance } => {
                'outer: for y in rect.y1..rect.y2 {
                    for x in rect.x1..rect.x2 {
                        let got = px(x, y);
                        if !close(got, *rgba, *tolerance) {
                            fails.push(format!("({x}, {y}) is {got:?}, want {rgba:?}"));
                            break 'outer;
                        }
                    }
                }
            }
            Expect::AnyPixel { rect, rgba, tolerance } => {
                let found = (rect.y1..rect.y2).any(|y| (rect.x1..rect.x2).any(|x| close(px(x, y), *rgba, *tolerance)));
                if !found {
                    fails.push(format!("no pixel of {rect:?} is {rgba:?}"));
                }
            }
            Expect::Shared { offset, bytes } => {
                let got = &r.shared[*offset as usize..*offset as usize + bytes.len()];
                if got != bytes.as_slice() {
                    fails.push(format!("shared memory at {offset} is {got:02x?}, want {bytes:02x?}"));
                }
            }
        }
    }
    for e in take_errors() {
        fails.push(format!("WebGPU error: {e}"));
    }
    let stats = r.core.stats();
    if stats.errors != 0 || stats.skipped_draws != 0 {
        fails.push(format!("core reported problems: {:?}", r.core.log()));
    }
    fails
}

/// Writes the frame as a binary PPM (for looking at failures).
pub fn save_ppm(path: &std::path::Path, r: &Run) {
    let mut out = format!("P6\n{} {}\n255\n", r.width, r.height).into_bytes();
    for p in r.pixels.as_chunks::<4>().0 {
        out.extend_from_slice(&p[..3]);
    }
    let _ = std::fs::write(path, out);
}
