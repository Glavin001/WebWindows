// Keyboard and mouse events from the page to a Wine process in a worker.
//
// The worker spends its time inside the program (or blocked in one of its
// waits), so it cannot take messages; the page writes events into this ring
// in shared memory instead, and the worker reads them when win32u looks for
// input and sleeps on it (Atomics.wait) when the program waits.
//
// Layout (Int32): [written, read, signal, 0], then SLOTS events of
// [type, a, b, c, d]: a mouse event (type 0) is x, y, MOUSEEVENTF_* flags and
// wheel data; a key (type 1) is virtual key, scan code and KEYEVENTF_* flags.

const SLOTS = 256;
const SLOT = 5;
const HEADER = 4;

export class InputRing {
  /** A new ring (on the page); pass `.buffer` to the worker. */
  static create() {
    return new InputRing(new SharedArrayBuffer((HEADER + SLOTS * SLOT) * 4));
  }

  /** @param {SharedArrayBuffer} buffer */
  constructor(buffer) {
    this.buffer = buffer;
    this.i32 = new Int32Array(buffer);
  }

  /** Adds an event; false when the ring is full (the worker is not reading). */
  push(type, a = 0, b = 0, c = 0, d = 0) {
    const v = this.i32;
    const w = Atomics.load(v, 0);
    if (w - Atomics.load(v, 1) >= SLOTS) return false;
    v.set([type, a, b, c, d], HEADER + (w % SLOTS) * SLOT);
    Atomics.store(v, 0, w + 1);
    Atomics.add(v, 2, 1);
    Atomics.notify(v, 2);
    return true;
  }

  mouse(x, y, flags = 0, data = 0) {
    return this.push(0, x, y, flags, data);
  }

  key(vk, scan, flags = 0) {
    return this.push(1, vk, scan, flags);
  }

  pending() {
    return Atomics.load(this.i32, 0) !== Atomics.load(this.i32, 1);
  }

  /** Moves waiting events into a Display's queue; returns how many. */
  drain(display) {
    const v = this.i32;
    const w = Atomics.load(v, 0);
    let r = Atomics.load(v, 1);
    const n = w - r;
    for (; r !== w; r++) {
      const o = HEADER + (r % SLOTS) * SLOT;
      if (v[o] === 0) display.mouse(v[o + 1], v[o + 2], v[o + 3], v[o + 4]);
      else display.key(v[o + 1], v[o + 2], v[o + 3]);
    }
    Atomics.store(v, 1, w);
    return n;
  }

  /** Blocks (in a worker) until an event arrives or `ms` pass (-1: no limit). */
  wait(ms) {
    const s = Atomics.load(this.i32, 2);
    if (this.pending()) return;
    Atomics.wait(this.i32, 2, s, ms < 0 ? Infinity : ms);
  }
}
