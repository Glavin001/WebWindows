//! The render core in the browser: a `Renderer` for the render worker (the
//! only thread that touches WebGPU) and the scene library for the producer
//! and the test page. Built with `wasm-bindgen --target web`; see
//! `runtime/d3dgpu/`.
//!
//! Everything here returns to the event loop between calls: WebGPU
//! presents and resolves `mapAsync` only there, so readbacks are started by
//! one call and collected by later polls.

#![cfg(target_arch = "wasm32")]

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use d3dgpu_core::{Core, HeadlessPresenter, Options, SurfacePresenter};
use wasm_bindgen::prelude::*;

/// Present target window id used by the scenes.
const WINDOW: u32 = d3dgpu_scenes::WINDOW;

#[wasm_bindgen]
pub struct Renderer {
    core: Core,
    device: wgpu::Device,
    queue: wgpu::Queue,
    shared: Vec<u8>,
    errors: Arc<Mutex<Vec<String>>>,
    read: Option<(wgpu::Buffer, Arc<AtomicU8>, u32, u32, u32)>,
    adapter: String,
}

#[wasm_bindgen]
impl Renderer {
    /// Creates the device with WebGPU's default limits. `optional` also
    /// requests the optional features the core uses (clip distances, BC,
    /// float32-filterable) where the adapter has them. With a canvas,
    /// window 1 presents to it; otherwise frames stay in a headless front
    /// buffer that [`Renderer::start_frame_read`] can read.
    pub async fn create(canvas: Option<web_sys::OffscreenCanvas>, optional: bool) -> Result<Renderer, JsError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::BROWSER_WEBGPU,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let surface = match &canvas {
            Some(c) => Some(instance.create_surface(wgpu::SurfaceTarget::OffscreenCanvas(c.clone()))?),
            None => None,
        };
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: surface.as_ref(),
                ..Default::default()
            })
            .await
            .map_err(|e| JsError::new(&format!("no WebGPU adapter: {e}")))?;
        let wanted = wgpu::Features::CLIP_DISTANCES
            | wgpu::Features::TEXTURE_COMPRESSION_BC
            | wgpu::Features::FLOAT32_FILTERABLE;
        let features = if optional { adapter.features() & wanted } else { wgpu::Features::empty() };
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("d3dgpu"),
                required_features: features,
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await
            .map_err(|e| JsError::new(&format!("requestDevice: {e}")))?;
        let errors = Arc::new(Mutex::new(Vec::new()));
        let sink = errors.clone();
        device.on_uncaptured_error(Arc::new(move |e: wgpu::Error| sink.lock().unwrap().push(e.to_string())));
        let info = adapter.get_info();
        let adapter_name = format!("{} {:?}, features: {:?}", info.name, info.backend, features.features_webgpu);
        let mut core = Core::with_options(device.clone(), queue, Options::for_device(&device));
        if let (Some(surface), Some(c)) = (surface, canvas) {
            let mut config = surface
                .get_default_config(&adapter, c.width().max(1), c.height().max(1))
                .ok_or_else(|| JsError::new("canvas surface is not supported by the adapter"))?;
            config.format = config.format.remove_srgb_suffix();
            core.set_presenter(WINDOW, Box::new(SurfacePresenter::new(&device, surface, config)));
        }
        let queue = core.queue().clone();
        Ok(Renderer { core, device, queue, shared: Vec::new(), errors, read: None, adapter: adapter_name })
    }

    /// Starts over with a fresh core (objects, state, fences) on the same
    /// device, presenting headlessly. Used between test scenes.
    pub fn reset(&mut self) {
        self.core = Core::with_options(self.device.clone(), self.queue.clone(), Options::for_device(&self.device));
        self.shared.iter_mut().for_each(|b| *b = 0);
        self.read = None;
    }

    pub fn adapter(&self) -> String {
        self.adapter.clone()
    }

    /// Resizes the shared memory region readbacks write into.
    pub fn set_shared_size(&mut self, size: usize) {
        self.shared.resize(size, 0);
    }

    /// Bytes of the shared memory region.
    pub fn shared_bytes(&self, offset: usize, len: usize) -> Vec<u8> {
        self.shared.get(offset..offset + len).map(<[u8]>::to_vec).unwrap_or_default()
    }

    /// Executes one batch.
    pub fn execute(&mut self, batch: &[u8]) -> Result<(), JsError> {
        self.core.execute(batch, &self.shared).map_err(|e| JsError::new(&e.to_string()))
    }

    /// Retires finished readbacks into the shared region; returns the
    /// completed fence.
    pub fn poll(&mut self) -> f64 {
        self.core.poll(&mut self.shared) as f64
    }

    /// Whether readbacks or fences are still in flight.
    pub fn completed_fence(&self) -> f64 {
        self.core.completed_fence() as f64
    }

    /// Counters as JSON.
    pub fn stats_json(&self) -> String {
        let s = self.core.stats();
        format!(
            "{{\"batches\":{},\"commands\":{},\"draws\":{},\"skipped_draws\":{},\"submits\":{},\"passes\":{},\"pipelines\":{},\"bind_groups\":{},\"samplers\":{},\"buffers\":{},\"textures\":{},\"translations\":{},\"bytes_uploaded\":{},\"load_op_clears\":{},\"quad_clears\":{},\"presents\":{},\"errors\":{}}}",
            s.batches, s.commands, s.draws, s.skipped_draws, s.submits, s.passes, s.pipelines_created, s.bind_groups_created,
            s.samplers_created, s.buffers_created, s.textures_created, s.shader_translations, s.bytes_uploaded,
            s.load_op_clears, s.quad_clears, s.presents, s.errors
        )
    }

    /// The core's log and WebGPU validation errors since the last call.
    pub fn take_messages(&mut self) -> Vec<String> {
        let mut out: Vec<String> = self.core.log().to_vec();
        out.extend(std::mem::take(&mut *self.errors.lock().unwrap()).into_iter().map(|e| format!("WebGPU: {e}")));
        out
    }

    /// Starts reading the headless front buffer of window 1. Returns false
    /// when nothing was presented headlessly.
    pub fn start_frame_read(&mut self) -> bool {
        let device = self.core.device().clone();
        let queue = self.core.queue().clone();
        let Some(p) = self.core.presenter(WINDOW) else { return false };
        let Some(h) = p.as_any().downcast_mut::<HeadlessPresenter>() else { return false };
        let Some((tex, (w, h))) = h.front_buffer() else { return false };
        let row = (w * 4).next_multiple_of(256);
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("d3dgpu frame read"),
            size: (row * h) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            tex.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        queue.submit([enc.finish()]);
        let state = Arc::new(AtomicU8::new(0));
        let s = state.clone();
        buf.slice(..)
            .map_async(wgpu::MapMode::Read, move |r| s.store(if r.is_ok() { 1 } else { 2 }, Ordering::Release));
        self.read = Some((buf, state, w, h, row));
        true
    }

    /// The frame as tightly packed RGBA8 once the read has finished
    /// (undefined while it is pending).
    pub fn frame_ready(&mut self) -> Option<Vec<u8>> {
        let (buf, state, w, h, row) = self.read.as_ref()?;
        match state.load(Ordering::Acquire) {
            0 => return None,
            2 => {
                self.read = None;
                return Some(Vec::new());
            }
            _ => {}
        }
        let data = buf.slice(..).get_mapped_range().map(|v| v.to_vec()).unwrap_or_default();
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..*h as usize {
            out.extend_from_slice(&data[y * *row as usize..y * *row as usize + *w as usize * 4]);
        }
        buf.unmap();
        self.read = None;
        Some(out)
    }

    pub fn frame_width(&mut self) -> u32 {
        self.core
            .presenter(WINDOW)
            .and_then(|p| {
                p.as_any().downcast_mut::<HeadlessPresenter>().and_then(|h| h.front_buffer().map(|(_, s)| s.0))
            })
            .unwrap_or(0)
    }
}

/// A built scene: command batches and expectations.
#[wasm_bindgen]
pub struct SceneData {
    built: d3dgpu_scenes::Built,
}

#[wasm_bindgen]
impl SceneData {
    pub fn name(&self) -> String {
        self.built.name.into()
    }
    pub fn batch_count(&self) -> usize {
        self.built.batches.len()
    }
    pub fn batch(&self, i: usize) -> Vec<u8> {
        self.built.batches[i].clone()
    }
    pub fn shared_size(&self) -> usize {
        self.built.shared_size
    }
    /// Expected shared-memory contents as (offset, bytes) pairs flattened
    /// into [offset, len, bytes...] runs.
    pub fn expected_shared(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for e in &self.built.expect {
            if let d3dgpu_scenes::Expect::Shared { offset, bytes } = e {
                out.extend_from_slice(&offset.to_le_bytes());
                out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                out.extend_from_slice(bytes);
            }
        }
        out
    }
    /// Checks a frame (RGBA8 rows `width` wide) and the shared memory;
    /// returns the failures, one per line.
    pub fn check(&self, width: u32, pixels: &[u8], shared: &[u8]) -> Vec<String> {
        self.built.check(width, pixels, shared)
    }
}

pub fn scene_names() -> Vec<String> {
    d3dgpu_scenes::ALL.iter().map(|s| s.name.to_string()).collect()
}

#[wasm_bindgen(js_name = sceneNames)]
pub fn scene_names_js() -> Vec<String> {
    scene_names()
}

#[wasm_bindgen(js_name = buildScene)]
pub fn build_scene(name: &str, width: u32, height: u32) -> Option<SceneData> {
    let s = d3dgpu_scenes::ALL.iter().find(|s| s.name == name)?;
    Some(SceneData { built: s.build(width, height) })
}

/// The game-shaped performance scene: a setup batch, then `frames` frames
/// of `draws` draws each, one batch per frame.
#[wasm_bindgen(js_name = buildPerf)]
pub fn build_perf(draws: u32, frames: u32, width: u32, height: u32) -> SceneData {
    let mut b = d3dgpu_scenes::Builder::new(width, height);
    let assets = d3dgpu_scenes::perf::setup(&mut b);
    for f in 0..frames {
        b.split();
        d3dgpu_scenes::perf::frame(&mut b, &assets, draws, f);
    }
    SceneData { built: b.finish("perf") }
}
