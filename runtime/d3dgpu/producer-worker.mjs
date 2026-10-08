// The producer: encodes scenes into command batches (with the wasm scene
// library) and hands them to the render worker through shared memory, as
// wined3d's CS thread will. It blocks with Atomics.wait, which only the
// render worker must never do: here it waits for slot space and, for
// readbacks, for the fence the render worker completes after mapAsync.

import init, { buildScene, DemoRunner } from './pkg/d3dgpu_web.js';
import * as P from './protocol.mjs';

let ctrl, bytes;

function submit(batch, flags) {
  // Wait until the render worker took the previous batch.
  for (;;) {
    const produced = Atomics.load(ctrl, P.PRODUCED);
    const consumed = Atomics.load(ctrl, P.CONSUMED);
    if (produced === consumed) break;
    Atomics.wait(ctrl, P.CONSUMED, consumed, 1000);
  }
  if (batch.length > P.SLOT_BYTES) throw new Error(`batch of ${batch.length} bytes does not fit the slot`);
  bytes.set(batch, P.SLOT);
  Atomics.store(ctrl, P.LEN, batch.length);
  Atomics.store(ctrl, P.FLAGS, flags);
  Atomics.add(ctrl, P.PRODUCED, 1);
  Atomics.notify(ctrl, P.PRODUCED);
}

function runScene(data) {
  const n = data.batch_count();
  Atomics.store(ctrl, P.SHARED_SIZE, data.shared_size());
  for (let i = 0; i < n; i++) {
    submit(data.batch(i), (i === 0 ? P.FIRST : 0) | (i === n - 1 ? P.LAST : 0));
  }
}

/// Blocks until `fence` completes and compares the shared region with the
/// scene's expected readbacks: the synchronous LockRect path.
function waitReadbacks(data) {
  const expected = data.expected_shared();
  const view = new DataView(expected.buffer, expected.byteOffset, expected.byteLength);
  let fences = 0;
  for (let p = 0; p < expected.length; p += 8 + view.getUint32(p + 4, true)) fences++;
  const t0 = performance.now();
  while (Atomics.load(ctrl, P.FENCE) < fences) {
    if (Atomics.wait(ctrl, P.FENCE, Atomics.load(ctrl, P.FENCE), 5000) === 'timed-out') {
      return { ok: false, detail: `fence ${fences} did not complete` };
    }
  }
  const waited = performance.now() - t0;
  for (let p = 0; p < expected.length; ) {
    const offset = view.getUint32(p, true);
    const len = view.getUint32(p + 4, true);
    const want = expected.subarray(p + 8, p + 8 + len);
    const got = bytes.subarray(P.SHARED + offset, P.SHARED + offset + len);
    if (!want.every((b, i) => b === got[i])) return { ok: false, detail: `shared memory at ${offset} differs` };
    p += 8 + len;
  }
  return { ok: true, detail: `${fences} readback(s), blocked ${waited.toFixed(1)} ms` };
}

// Streaming demos: one batch per frame. Scheduling through a message
// channel instead of setTimeout avoids the 4 ms clamp on nested timers;
// submit() blocks until the render worker took the previous batch, so the
// render worker sets the pace.
let demo = null;
let generation = 0;
const tickChannel = new MessageChannel();
tickChannel.port1.onmessage = (e) => {
  if (!demo || e.data !== generation) return;
  submit(demo.runner.frame((performance.now() - demo.start) / 1000), 0);
  tickChannel.port2.postMessage(generation);
};

onmessage = async (e) => {
  const m = e.data;
  if (m.type === 'init') {
    await init();
    ctrl = new Int32Array(m.sab, 0, P.CTRL_BYTES / 4);
    bytes = new Uint8Array(m.sab);
    postMessage({ type: 'ready' });
  } else if (m.type === 'scene') {
    demo = null;
    Atomics.store(ctrl, P.SCENE, m.index);
    const data = buildScene(m.name, m.width ?? 64, m.height ?? 64);
    runScene(data);
    const rb = data.shared_size() > 0 ? waitReadbacks(data) : { ok: true, detail: 'no readbacks' };
    postMessage({ type: 'readback', name: m.name, ...rb });
  } else if (m.type === 'demo') {
    const runner = DemoRunner.create(m.name, m.width, m.height, m.param ?? undefined);
    if (!runner) {
      postMessage({ type: 'error', message: `no demo ${m.name}` });
      return;
    }
    Atomics.store(ctrl, P.SHARED_SIZE, 0);
    submit(runner.setup(), P.FIRST);
    demo = { runner, start: performance.now() - (m.time ?? 0) * 1000 };
    tickChannel.port2.postMessage(++generation);
  } else if (m.type === 'demotest') {
    // A few frames of a demo, checked by the render worker (test mode).
    demo = null;
    const runner = DemoRunner.create(m.name, m.width, m.height, m.param ?? undefined);
    Atomics.store(ctrl, P.SCENE, P.DEMO_BASE + m.index);
    Atomics.store(ctrl, P.SHARED_SIZE, 0);
    submit(runner.setup(), P.FIRST);
    for (let f = 0; f < 4; f++) submit(runner.frame(f * 0.4), f === 3 ? P.LAST : 0);
    postMessage({ type: 'readback', name: m.name, ok: true, detail: 'no readbacks' });
  } else if (m.type === 'stop') {
    demo = null;
    generation++;
  }
};
