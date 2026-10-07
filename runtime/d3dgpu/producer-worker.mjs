// The producer: encodes scenes into command batches (with the wasm scene
// library) and hands them to the render worker through shared memory, as
// wined3d's CS thread will. It blocks with Atomics.wait, which only the
// render worker must never do: here it waits for slot space and, for
// readbacks, for the fence the render worker completes after mapAsync.

import init, { buildScene, buildPerf, buildPerf11 } from './pkg/d3dgpu_web.js';
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

let demoTimer = null;

onmessage = async (e) => {
  const m = e.data;
  if (m.type === 'init') {
    await init();
    ctrl = new Int32Array(m.sab, 0, P.CTRL_BYTES / 4);
    bytes = new Uint8Array(m.sab);
    postMessage({ type: 'ready' });
  } else if (m.type === 'scene') {
    Atomics.store(ctrl, P.SCENE, m.index);
    const data = buildScene(m.name, m.width ?? 64, m.height ?? 64);
    runScene(data);
    const rb = data.shared_size() > 0 ? waitReadbacks(data) : { ok: true, detail: 'no readbacks' };
    postMessage({ type: 'readback', name: m.name, ...rb });
  } else if (m.type === 'perf') {
    clearTimeout(demoTimer);
    const data = (m.api === 'd3d11' ? buildPerf11 : buildPerf)(m.draws, 2, m.width, m.height);
    submit(data.batch(0), P.FIRST); // setup
    let frame = 0;
    const next = () => {
      submit(data.batch(1 + (frame++ & 1)), 0);
      demoTimer = setTimeout(next, 0);
    };
    next();
  } else if (m.type === 'stop') {
    clearTimeout(demoTimer);
  }
};
