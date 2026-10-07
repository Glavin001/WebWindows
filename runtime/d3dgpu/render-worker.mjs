// The render worker: the only thread that touches WebGPU. It waits for
// batches with Atomics.waitAsync (never Atomics.wait), so promises,
// mapAsync and canvas presentation keep running between batches.

import init, { Renderer, buildScene, sceneNames, demoList } from './pkg/d3dgpu_web.js';
import * as P from './protocol.mjs';

let renderer, ctrl, bytes, testMode;
// Per-frame samples since the last report: [interval since the previous
// frame, execute time] in ms.
let samples = [];
let lastFrameEnd = 0;
let lastReport = performance.now();
let paced = false;
// Frames the GPU may lag behind; more would queue work without bound in
// GPU-bound scenes and measure submission speed instead of throughput.
const MAX_IN_FLIGHT = 2;
const nextAnimationFrame = () =>
  typeof requestAnimationFrame === 'function'
    ? new Promise((r) => requestAnimationFrame(r))
    : new Promise((r) => setTimeout(r, 16));

/// Retires readbacks: copies the core's shared region into the shared
/// memory and wakes a producer blocked on the fence.
function pump() {
  if (renderer) {
    const done = renderer.poll();
    if (done > Atomics.load(ctrl, P.FENCE)) {
      const size = Atomics.load(ctrl, P.SHARED_SIZE);
      bytes.set(renderer.shared_bytes(0, size), P.SHARED);
      Atomics.store(ctrl, P.FENCE, done);
      Atomics.notify(ctrl, P.FENCE);
    }
  }
  setTimeout(pump, 1);
}

const tick = () => new Promise((r) => setTimeout(r, 1));

/// A demo's last frame must have real content (many distinct colours)
/// and no errors.
async function finishDemo(name) {
  if (!renderer.start_frame_read()) {
    postMessage({ type: 'result', name, failures: ['nothing was presented'], messages: renderer.take_messages() });
    return;
  }
  let pixels;
  for (let i = 0; i < 5000 && !(pixels = renderer.frame_ready()); i++) await tick();
  const failures = [];
  if (!pixels) failures.push('frame read did not finish');
  else {
    const colours = new Set();
    const words = new Uint32Array(pixels.buffer, pixels.byteOffset, pixels.byteLength / 4);
    for (let i = 0; i < words.length && colours.size < 64; i += 7) colours.add(words[i]);
    const min = name.startsWith('perf') ? 4 : 16;
    if (colours.size < min) failures.push(`only ${colours.size} distinct colours`);
  }
  const stats = JSON.parse(renderer.stats_json());
  if (stats.skipped_draws) failures.push(`${stats.skipped_draws} skipped draws`);
  postMessage({ type: 'result', name, failures, messages: renderer.take_messages(), stats });
}

async function finishScene() {
  // Collect the frame from the headless front buffer and check it here,
  // with the same scene description the producer used.
  const name = currentScene;
  if (name.startsWith('demo:')) return finishDemo(name.slice(5));
  const data = buildScene(name, 64, 64);
  if (!renderer.start_frame_read()) {
    postMessage({ type: 'result', name, failures: ['nothing was presented'], messages: renderer.take_messages() });
    return;
  }
  let pixels;
  for (let i = 0; i < 5000 && !(pixels = renderer.frame_ready()); i++) await tick();
  const expected = data.expected_shared();
  const view = new DataView(expected.buffer, expected.byteOffset, expected.byteLength);
  let fences = 0;
  for (let p = 0; p < expected.length; p += 8 + view.getUint32(p + 4, true)) fences++;
  for (let i = 0; i < 5000 && renderer.completed_fence() < fences; i++) await tick();
  const shared = renderer.shared_bytes(0, data.shared_size());
  const failures = pixels ? data.check(renderer.frame_width(), pixels, shared) : ['frame read did not finish'];
  postMessage({ type: 'result', name, failures, messages: renderer.take_messages(), stats: JSON.parse(renderer.stats_json()) });
}

let currentScene = null;

async function loop() {
  for (;;) {
    const seen = Atomics.load(ctrl, P.CONSUMED);
    if (Atomics.load(ctrl, P.PRODUCED) === seen) {
      const w = Atomics.waitAsync(ctrl, P.PRODUCED, seen);
      if (w.async) await w.value;
      continue;
    }
    const len = Atomics.load(ctrl, P.LEN);
    const flags = Atomics.load(ctrl, P.FLAGS);
    // Copy the batch out of shared memory (writeBuffer and friends copy at
    // call time anyway); the slot is free once CONSUMED moves.
    const batch = bytes.slice(P.SLOT, P.SLOT + len);
    Atomics.store(ctrl, P.CONSUMED, seen + 1);
    Atomics.notify(ctrl, P.CONSUMED);
    if (flags & P.FIRST) {
      if (testMode) {
        renderer.reset();
        const index = Atomics.load(ctrl, P.SCENE);
        currentScene = index >= P.DEMO_BASE
          ? 'demo:' + JSON.parse(demoList())[index - P.DEMO_BASE].name
          : sceneNames()[index];
      } else {
        renderer.restart();
        lastFrameEnd = 0;
      }
      renderer.set_shared_size(Atomics.load(ctrl, P.SHARED_SIZE));
      Atomics.store(ctrl, P.FENCE, 0);
    }
    if (!testMode) {
      if (paced) await nextAnimationFrame();
      while (renderer.gpu_in_flight() >= MAX_IN_FLIGHT) await tick();
    }
    const t0 = performance.now();
    try {
      renderer.execute(batch);
    } catch (err) {
      postMessage({ type: 'error', message: String(err) });
    }
    const t1 = performance.now();
    if (testMode && flags & P.LAST) {
      await finishScene();
    } else if (!testMode) {
      renderer.track_gpu();
      if (!(flags & P.FIRST)) samples.push([lastFrameEnd ? t1 - lastFrameEnd : 0, t1 - t0]);
      lastFrameEnd = t1;
      // Return to the event loop so the canvas presents this frame.
      await new Promise((r) => setTimeout(r, 0));
      const now = performance.now();
      if (now - lastReport > 250) {
        postMessage({
          type: 'stats',
          samples,
          gpu: renderer.take_gpu_latencies(),
          stats: JSON.parse(renderer.stats_json()),
          messages: renderer.take_messages(),
        });
        samples = [];
        lastReport = now;
      }
    }
  }
}

onmessage = async (e) => {
  const m = e.data;
  if (m.type === 'pace') {
    paced = m.on;
  } else if (m.type === 'restart') {
    lastFrameEnd = 0;
    samples = [];
  } else if (m.type === 'init') {
    await init();
    ctrl = new Int32Array(m.sab, 0, P.CTRL_BYTES / 4);
    bytes = new Uint8Array(m.sab);
    testMode = !m.canvas;
    try {
      renderer = await Renderer.create(m.canvas ?? undefined, m.optional ?? true);
    } catch (err) {
      postMessage({ type: 'error', message: String(err) });
      return;
    }
    postMessage({ type: 'ready', adapter: renderer.adapter(), info: JSON.parse(renderer.adapter_info_json()) });
    pump();
    loop();
  }
};
