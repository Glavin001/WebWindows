// The render worker for translated Wine's Direct3D (see ./d3d.mjs): the only
// thread that touches WebGPU. It takes the batches wined3d hands over and
// executes them with the d3dgpu core, waiting with Atomics.waitAsync so
// promises and readbacks keep running between batches.

import init, { Renderer } from '../d3dgpu/pkg/d3dgpu_web.js';
import * as P from '../d3dgpu/protocol.mjs';

let renderer, ctrl, bytes;

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
      postMessage({ type: 'error', message: String(err) });
    }
    const messages = renderer.take_messages();
    if (messages.length) postMessage({ type: 'log', text: messages.join('\n') });
  }
}

onmessage = async (e) => {
  if (e.data.type !== 'init') return;
  ctrl = new Int32Array(e.data.sab, 0, P.CTRL_BYTES / 4);
  bytes = new Uint8Array(e.data.sab);
  try {
    await init();
    renderer = await Renderer.create(undefined, true);
    renderer.set_shared_size(P.SHARED_BYTES);
  } catch (err) {
    postMessage({ type: 'error', message: String(err), fatal: true });
    return;
  }
  postMessage({ type: 'ready', adapter: renderer.adapter() });
  pump();
  loop();
};
