// Direct3D for translated Wine: the host side of wined3d's WebGPU backend
// (native/wined3d-wgpu/adapter_wgpu.c).
//
// wined3d encodes d3dgpu command batches (crates/d3dgpu-proto) in guest
// memory and hands each one over with a unix call. The bridge copies it into
// a SharedArrayBuffer slot that a render worker (./d3d-worker.mjs) takes and
// executes with the d3dgpu core on WebGPU; readbacks come back through the
// same buffer, and the program's thread blocks on a fence for them.
//
// The layout of the buffer is runtime/d3dgpu/protocol.mjs (one batch slot,
// a fence and a readback region).

import * as P from '../d3dgpu/protocol.mjs';

/** The handle the host gives wined3d.dll for its unix calls. */
export const WINED3D_UNIXLIB = 0x3000;
/** ... and opengl32.dll, which wined3d imports: its unix side is a stub. */
export const OPENGL_UNIXLIB = 0x3001;

const STATUS_SUCCESS = 0;
const STATUS_NOT_SUPPORTED = 0xc00000bb;
const STATUS_TIMEOUT = 0x102;

export class D3DBridge {
  /**
   * @param {SharedArrayBuffer} sab  P.SAB_BYTES, shared with the render worker
   * @param {string} adapter  the WebGPU adapter's name
   */
  constructor(sab, adapter = 'WebGPU') {
    this.ctrl = new Int32Array(sab, 0, P.CTRL_BYTES / 4);
    this.bytes = new Uint8Array(sab);
    this.adapter = adapter;
    this.produced = Atomics.load(this.ctrl, P.PRODUCED);
    this.batches = 0;
  }

  /** A unix call from wined3d.dll (enum wgpu_unix_call). */
  unixCall(machine, code, args) {
    const u8 = machine.u8;
    const dv = new DataView(u8.buffer);
    const u32 = (p) => dv.getUint32(p, true);
    switch (code) {
      case 0: {
        // open: struct wgpu_open_params {version, max_batch, shared_size, name[64]}
        if (u32(args) !== 1) return STATUS_NOT_SUPPORTED;
        dv.setUint32(args + 4, P.SLOT_BYTES, true);
        dv.setUint32(args + 8, P.SHARED_BYTES, true);
        const name = new TextEncoder().encode(`${this.adapter} (WebGPU)`.slice(0, 63));
        u8.fill(0, args + 12, args + 76);
        u8.set(name, args + 12);
        return STATUS_SUCCESS;
      }
      case 1: {
        // submit: struct wgpu_submit_params {data, size}
        const data = u32(args);
        const size = u32(args + 4);
        if (size > P.SLOT_BYTES) return STATUS_NOT_SUPPORTED;
        // One slot: wait until the render worker took the previous batch.
        for (;;) {
          const consumed = Atomics.load(this.ctrl, P.CONSUMED);
          if (consumed === this.produced) break;
          Atomics.wait(this.ctrl, P.CONSUMED, consumed, 1000);
        }
        this.bytes.set(u8.subarray(data, data + size), P.SLOT);
        Atomics.store(this.ctrl, P.LEN, size);
        Atomics.store(this.ctrl, P.FLAGS, this.batches++ ? 0 : P.FIRST);
        Atomics.store(this.ctrl, P.PRODUCED, ++this.produced);
        Atomics.notify(this.ctrl, P.PRODUCED);
        return STATUS_SUCCESS;
      }
      case 2: {
        // wait: struct wgpu_wait_params {fence_lo, fence_hi, shared_offset, size, dst}
        const fence = u32(args) | 0;
        const offset = u32(args + 8);
        const size = u32(args + 12);
        const dst = u32(args + 16);
        const deadline = performance.now() + 10000;
        for (;;) {
          const done = Atomics.load(this.ctrl, P.FENCE);
          if (done - fence >= 0) break;
          if (performance.now() > deadline) return STATUS_TIMEOUT;
          Atomics.wait(this.ctrl, P.FENCE, done, 100);
        }
        if (size) u8.set(this.bytes.subarray(P.SHARED + offset, P.SHARED + offset + size), dst);
        return STATUS_SUCCESS;
      }
      default:
        return STATUS_NOT_SUPPORTED;
    }
  }
}

/**
 * Records wined3d's command stream without executing it (Node has no
 * WebGPU): batches are kept for `cargo run -p d3dgpu-core --example replay`,
 * and readbacks return zeros.
 */
export class D3DRecorder {
  constructor() {
    this.batches = [];
  }

  unixCall(machine, code, args) {
    const u8 = machine.u8;
    const dv = new DataView(u8.buffer);
    const u32 = (p) => dv.getUint32(p, true);
    switch (code) {
      case 0:
        if (u32(args) !== 1) return STATUS_NOT_SUPPORTED;
        dv.setUint32(args + 4, P.SLOT_BYTES, true);
        dv.setUint32(args + 8, P.SHARED_BYTES, true);
        u8.fill(0, args + 12, args + 76);
        u8.set(new TextEncoder().encode('d3dgpu recorder'), args + 12);
        return STATUS_SUCCESS;
      case 1:
        this.batches.push(u8.slice(u32(args), u32(args) + u32(args + 4)));
        return STATUS_SUCCESS;
      case 2:
        u8.fill(0, u32(args + 16), u32(args + 16) + u32(args + 12));
        return STATUS_SUCCESS;
      default:
        return STATUS_NOT_SUPPORTED;
    }
  }

  /** "D3GR", then each batch as a u32 length and its bytes. */
  bytes() {
    const size = 4 + this.batches.reduce((n, b) => n + 4 + b.length, 0);
    const out = new Uint8Array(size);
    const dv = new DataView(out.buffer);
    out.set([0x44, 0x33, 0x47, 0x52]);
    let p = 4;
    for (const b of this.batches) {
      dv.setUint32(p, b.length, true);
      out.set(b, p + 4);
      p += 4 + b.length;
    }
    return out;
  }
}

/**
 * Starts a render worker and waits until it has a WebGPU device. Resolves
 * to a D3DBridge, or null when there is no WebGPU (the program then gets
 * wined3d without 3D).
 * @param {URL|string} workerUrl  ./d3d-worker.mjs
 * @param {(s: string) => void} [log]
 */
export async function startD3D(workerUrl, log = () => {}) {
  if (typeof Worker === 'undefined' || typeof navigator === 'undefined' || !navigator.gpu) return null;
  const sab = new SharedArrayBuffer(P.SAB_BYTES);
  const worker = new Worker(workerUrl, { type: 'module' });
  const ready = await new Promise((resolve) => {
    const timer = setTimeout(() => resolve(null), 20000);
    worker.onmessage = (e) => {
      if (e.data.type === 'ready') {
        clearTimeout(timer);
        resolve(e.data);
      } else if (e.data.type === 'error') {
        log(`d3d: ${e.data.message}`);
        if (!e.data.fatal) return;
        clearTimeout(timer);
        resolve(null);
      } else if (e.data.type === 'log') {
        log(`d3d: ${e.data.text}`);
      }
    };
    worker.postMessage({ type: 'init', sab });
  });
  if (!ready) {
    worker.terminate();
    return null;
  }
  log(`d3d: render worker on ${ready.adapter}`);
  const bridge = new D3DBridge(sab, ready.adapter);
  bridge.worker = worker;
  return bridge;
}
