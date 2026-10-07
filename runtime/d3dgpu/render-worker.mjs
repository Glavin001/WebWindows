// The render worker: the only thread that touches WebGPU. It waits for
// batches with Atomics.waitAsync (never Atomics.wait), so promises,
// mapAsync and canvas presentation keep running between batches.

import init, { Renderer, buildScene, sceneNames } from './pkg/d3dgpu_web.js';
import * as P from './protocol.mjs';

let renderer, ctrl, bytes, testMode;
const timing = { frames: 0, executeMs: 0, draws: 0, last: performance.now() };

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

async function finishScene() {
  // Collect the frame from the headless front buffer and check it here,
  // with the same scene description the producer used.
  const name = currentScene;
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
        currentScene = sceneNames()[Atomics.load(ctrl, P.SCENE)];
      }
      renderer.set_shared_size(Atomics.load(ctrl, P.SHARED_SIZE));
      Atomics.store(ctrl, P.FENCE, 0);
    }
    const t0 = performance.now();
    try {
      renderer.execute(batch);
    } catch (err) {
      postMessage({ type: 'error', message: String(err) });
    }
    timing.executeMs += performance.now() - t0;
    timing.frames++;
    if (testMode && flags & P.LAST) {
      await finishScene();
    } else if (!testMode) {
      // Return to the event loop so the canvas presents this frame.
      await new Promise((r) => setTimeout(r, 0));
      const now = performance.now();
      if (now - timing.last > 500) {
        postMessage({
          type: 'stats',
          fps: (timing.frames * 1000) / (now - timing.last),
          executeMs: timing.executeMs / timing.frames,
          stats: JSON.parse(renderer.stats_json()),
          messages: renderer.take_messages(),
        });
        timing.frames = 0;
        timing.executeMs = 0;
        timing.last = now;
      }
    }
  }
}

onmessage = async (e) => {
  const m = e.data;
  if (m.type === 'init') {
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
    postMessage({ type: 'ready', adapter: renderer.adapter() });
    pump();
    loop();
  }
};
