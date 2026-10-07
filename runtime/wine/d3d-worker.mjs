// The render worker for translated Wine's Direct3D (see ./d3d.mjs): the only
// thread that touches WebGPU. It takes the batches wined3d hands over and
// executes them with the d3dgpu core, waiting with Atomics.waitAsync so
// promises and readbacks keep running between batches. With a canvas from
// the page, Present draws to it.

import init, { Renderer } from '../d3dgpu/pkg/d3dgpu_web.js';
import * as P from '../d3dgpu/protocol.mjs';

/** d3dgpu_cmd_present.flags */
const PRESENT_VSYNC = 0x1;
/** Frames the GPU may be behind before the program waits (Direct3D's
 * default maximum frame latency). */
const MAX_FRAME_LATENCY = 3;

let renderer, ctrl, bytes;

// The page's port, when it gave one: the Wine worker that started this one
// is busy running the program and cannot relay, so the core's messages go
// to the page directly, and {type: 'snapshot'} from the page is answered
// with the next presented frame, {type: 'frame', width, height, pixels}
// (RGBA), for tests. That frame goes to an offscreen buffer instead of the
// canvas. (Headless browsers without a GPU compositor can neither show nor
// read a WebGPU canvas, so tests present offscreen throughout.)
let port = null;
let capture = 'idle'; // 'requested', 'reading'
const log = (text) => (port ?? self).postMessage({ type: 'log', text });
function onPortMessage(e) {
  if (e.data?.type !== 'snapshot' || !renderer || capture !== 'idle') return;
  capture = 'requested';
  renderer.capture_frames();
}
async function snapshot() {
  capture = 'reading';
  const width = renderer.frame_width();
  const started = renderer.start_frame_read();
  let pixels;
  for (let i = 0; started && i < 2000 && !(pixels = renderer.frame_ready()); i++) await new Promise((r) => setTimeout(r, 5));
  // Back to the canvas once the read is done (dropping the offscreen
  // buffer destroys its texture).
  renderer.show_frames();
  capture = 'idle';
  port.postMessage({ type: 'frame', width, height: pixels?.length ? pixels.length / 4 / width : 0, pixels });
}

/** Retires readbacks: copies the core's readback region into the shared
 * buffer and moves the fence the program's thread waits on. */
function pump() {
  const done = renderer.poll();
  if (done > Atomics.load(ctrl, P.FENCE)) {
    bytes.set(renderer.shared_bytes(0, P.SHARED_BYTES), P.SHARED);
    Atomics.store(ctrl, P.FENCE, done);
    Atomics.notify(ctrl, P.FENCE);
  }
  setTimeout(pump, 1);
}

// After a frame the worker returns to its event loop, which is when the
// canvas shows it: at once, or at the next display frame for a vsynced
// Present. It also waits while the GPU is MAX_FRAME_LATENCY frames behind.
// Either paces the program, which cannot hand over the batch after next
// until this one is taken.
const channel = new MessageChannel();
const yielded = [];
channel.port1.onmessage = () => yielded.shift()?.();
const nextTask = () => new Promise((resolve) => (yielded.push(resolve), channel.port2.postMessage(0)));
const nextFrame = () =>
  new Promise((resolve) => (typeof requestAnimationFrame === 'function' ? requestAnimationFrame(() => resolve()) : setTimeout(resolve, 16)));

async function loop() {
  let seen = Atomics.load(ctrl, P.PRODUCED);
  for (;;) {
    const w = Atomics.waitAsync(ctrl, P.PRODUCED, seen);
    if (w.async) await w.value;
    seen = Atomics.load(ctrl, P.PRODUCED);
    const len = Atomics.load(ctrl, P.LEN);
    const batch = bytes.slice(P.SLOT, P.SLOT + len);
    Atomics.store(ctrl, P.CONSUMED, seen);
    Atomics.notify(ctrl, P.CONSUMED);
    try {
      renderer.execute(batch);
    } catch (err) {
      log(`error: ${err}`);
    }
    const messages = renderer.take_messages();
    if (messages.length) log(messages.join('\n'));
    const present = renderer.take_present();
    if (present >= 0 && capture === 'requested') snapshot();
    if (present >= 0) {
      renderer.track_gpu();
      await (present & PRESENT_VSYNC ? nextFrame() : nextTask());
      while (renderer.gpu_in_flight() >= MAX_FRAME_LATENCY) await new Promise((r) => setTimeout(r, 1));
    }
  }
}

onmessage = async (e) => {
  if (e.data.type === 'resize') {
    renderer?.resize(e.data.width, e.data.height);
    return;
  }
  if (e.data.type !== 'init') return;
  ctrl = new Int32Array(e.data.sab, 0, P.CTRL_BYTES / 4);
  bytes = new Uint8Array(e.data.sab);
  let canvas = e.data.canvas ?? null;
  // Presenting offscreen, for tests: frames stay on the GPU, as with a canvas.
  const offscreen = !canvas && !!e.data.offscreen;
  port = e.data.port ?? null;
  if (port) port.onmessage = onPortMessage;
  try {
    await init();
    try {
      renderer = await Renderer.create(canvas ?? undefined, true);
    } catch (err) {
      if (!canvas) throw err;
      // No surface for the canvas: read frames back instead.
      log(`no canvas presentation: ${err}`);
      canvas = null;
      renderer = await Renderer.create(undefined, true);
    }
    renderer.set_shared_size(P.SHARED_BYTES);
  } catch (err) {
    postMessage({ type: 'error', message: String(err), fatal: true });
    return;
  }
  postMessage({ type: 'ready', adapter: renderer.adapter(), present: !!canvas || offscreen });
  pump();
  loop();
};
