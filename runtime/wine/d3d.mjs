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
//
// With a canvas (an OffscreenCanvas the page placed over its screen), the
// render worker presents frames to it on the GPU, and wined3d tells the
// bridge where the Direct3D window is so the page can move the canvas there.
// Without one, wined3d reads frames back and draws them with GDI.

import * as P from '../d3dgpu/protocol.mjs';

/** The handle the host gives wined3d.dll for its unix calls. */
export const WINED3D_UNIXLIB = 0x5000;

const STATUS_SUCCESS = 0;
const STATUS_NOT_SUPPORTED = 0xc00000bb;
const STATUS_TIMEOUT = 0x102;
/** wgpu_open_params.flags: the host shows presented frames over the window. */
const WGPU_HOST_PRESENT = 0x1;

/** PCI vendor ids for the vendor names WebGPU gives (GPUAdapterInfo.vendor);
 * wined3d reports a card of that vendor (NVIDIA's for one not listed). */
const PCI_VENDORS = { nvidia: 0x10de, amd: 0x1002, ati: 0x1002, intel: 0x8086 };
export const pciVendor = (name) => PCI_VENDORS[String(name ?? '').toLowerCase()] ?? 0;

export class D3DBridge {
  /**
   * @param {SharedArrayBuffer} sab  P.SAB_BYTES, shared with the render worker
   * @param {string} adapter  the WebGPU adapter's name
   * @param {{worker?: Worker, onWindow?: (w: object) => void}} [present]
   *   with a render worker presenting to a canvas: where window changes go
   * @param {(w: object) => void} [onWindow]  told where the window is in
   *   either case (a page locks the pointer for a fullscreen game)
   */
  constructor(sab, adapter = 'WebGPU', present = null, onWindow = present?.onWindow) {
    this.onWindow = onWindow;
    this.ctrl = new Int32Array(sab, 0, P.CTRL_BYTES / 4);
    this.bytes = new Uint8Array(sab);
    this.adapter = adapter;
    this.present = present;
    this.produced = Atomics.load(this.ctrl, P.PRODUCED);
    this.batches = 0;
    /**
     * Where the program's thread waits on the render worker, cumulative (the
     * status sampler turns them into rates): submits that found the slot
     * still full and how long they waited, fence waits (readbacks) and how
     * long, and the bytes handed over.
     */
    this.stats = { submitWaits: 0, submitWaitMs: 0, fenceWaits: 0, fenceWaitMs: 0, bytes: 0, readbackBytes: 0 };
    /** The GPU's PCI vendor (pciVendor), 0 when not known. */
    this.vendorId = 0;
  }

  /** A unix call from wined3d.dll (enum wgpu_unix_call). */
  unixCall(machine, code, args) {
    const u8 = machine.u8;
    const dv = new DataView(u8.buffer);
    const u32 = (p) => dv.getUint32(p, true);
    switch (code) {
      case 0: {
        // open: struct wgpu_open_params {version, max_batch, shared_size,
        // name[64], flags, vendor_id, device_id}
        if (u32(args) !== 1) return STATUS_NOT_SUPPORTED;
        dv.setUint32(args + 4, P.SLOT_BYTES, true);
        dv.setUint32(args + 8, P.SHARED_BYTES, true);
        const name = new TextEncoder().encode(`${this.adapter} (WebGPU)`.slice(0, 63));
        u8.fill(0, args + 12, args + 76);
        u8.set(name, args + 12);
        dv.setUint32(args + 76, this.present ? WGPU_HOST_PRESENT : 0, true);
        dv.setUint32(args + 80, this.vendorId, true);
        dv.setUint32(args + 84, 0, true);
        return STATUS_SUCCESS;
      }
      case 1: {
        // submit: struct wgpu_submit_params {data, size}
        const data = u32(args);
        const size = u32(args + 4);
        if (size > P.SLOT_BYTES) return STATUS_NOT_SUPPORTED;
        // One slot: wait until the render worker took the previous batch.
        let t0 = 0;
        for (;;) {
          const consumed = Atomics.load(this.ctrl, P.CONSUMED);
          if (consumed === this.produced) break;
          t0 ||= performance.now();
          Atomics.wait(this.ctrl, P.CONSUMED, consumed, 1000);
        }
        if (t0) (this.stats.submitWaits++, (this.stats.submitWaitMs += performance.now() - t0));
        this.stats.bytes += size;
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
        const t0 = performance.now();
        const deadline = t0 + 10000;
        let waited = false;
        for (;;) {
          const done = Atomics.load(this.ctrl, P.FENCE);
          if (done - fence >= 0) break;
          if (performance.now() > deadline) return STATUS_TIMEOUT;
          waited = true;
          Atomics.wait(this.ctrl, P.FENCE, done, 100);
        }
        if (waited) (this.stats.fenceWaits++, (this.stats.fenceWaitMs += performance.now() - t0));
        this.stats.readbackBytes += size;
        if (size) u8.set(this.bytes.subarray(P.SHARED + offset, P.SHARED + offset + size), dst);
        return STATUS_SUCCESS;
      }
      case 3: {
        // window: struct wgpu_window_params {x, y, width, height, buffer_width, buffer_height, visible}
        const w = {
          x: dv.getInt32(args, true),
          y: dv.getInt32(args + 4, true),
          width: u32(args + 8),
          height: u32(args + 12),
          bufferWidth: u32(args + 16),
          bufferHeight: u32(args + 20),
          visible: !!u32(args + 24),
        };
        this.onWindow?.(w);
        // Without a canvas, frames are read back and drawn into the window.
        if (!this.present) return STATUS_NOT_SUPPORTED;
        // The canvas's drawing buffer is the back buffer's size; the page
        // scales it to the window.
        this.present.worker?.postMessage({ type: 'resize', width: w.bufferWidth, height: w.bufferHeight });
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
  /** @param {(batch: Uint8Array) => void} [sink]  takes each batch as it comes (a
   *   long recording streamed to a file); without one they are kept for bytes() */
  constructor(sink = null) {
    this.batches = [];
    this.sink = sink;
    this.count = 0;
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
        this.count++;
        if (this.sink) this.sink(u8.subarray(u32(args), u32(args) + u32(args + 4)));
        else this.batches.push(u8.slice(u32(args), u32(args) + u32(args + 4)));
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
 * @param {{canvas?: OffscreenCanvas, offscreen?: boolean, port?: MessagePort, onWindow?: (w: object) => void}} [present]
 *   a canvas over the screen to present to (or, for tests, `offscreen`:
 *   present to a buffer that snapshots read), a port to the page for the
 *   render worker's messages and snapshots, and where to report the
 *   Direct3D window's position ({x, y, width, height, visible})
 */
export async function startD3D(workerUrl, log = () => {}, present = {}) {
  if (typeof Worker === 'undefined' || typeof navigator === 'undefined' || !navigator.gpu) {
    log('d3d: no WebGPU in workers in this browser (navigator.gpu is missing); Direct3D is off');
    return null;
  }
  const sab = new SharedArrayBuffer(P.SAB_BYTES);
  const worker = new Worker(workerUrl, { type: 'module' });
  const ready = await new Promise((resolve) => {
    // Given up on only after 30 s without progress: loading the core and
    // setting up WebGPU can take a while on a phone's first visit.
    let timer;
    const wait = () => {
      clearTimeout(timer);
      timer = setTimeout(() => {
        log('d3d: the render worker made no progress for 30 s; Direct3D is off');
        resolve(null);
      }, 30000);
    };
    wait();
    // The worker's script failed to load or threw before it could report.
    worker.onerror = (e) => {
      log(`d3d: render worker failed: ${e.message || 'could not load its script'}`);
      clearTimeout(timer);
      resolve(null);
    };
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
      } else if (e.data.type === 'progress') {
        wait();
      }
    };
    const { canvas, offscreen, port } = present;
    worker.postMessage({ type: 'init', sab, canvas, offscreen, port }, [canvas, port].filter(Boolean));
  });
  if (!ready) {
    worker.terminate();
    return null;
  }
  log(`d3d: render worker on ${ready.adapter}`);
  const bridge = new D3DBridge(sab, ready.adapter, ready.present ? { worker, onWindow: present.onWindow } : null, present.onWindow);
  bridge.worker = worker;
  bridge.vendorId = pciVendor(ready.vendor);
  return bridge;
}
