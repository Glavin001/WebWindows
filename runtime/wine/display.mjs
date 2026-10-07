// The screen behind the browser display driver (native/wine-unix/driver/browser.c).
//
// win32u draws each top-level window into a surface; the driver hands every
// changed rectangle to flush(), and window placement and stacking to
// windowPos(). This keeps an RGBA copy of each window and composes them,
// bottom to top, into `screen`, an RGBA image of the whole display that the
// page draws into a canvas (or a test saves as a PNG). Input queued with
// pushInput() (or brought in by `inputSource`, e.g. from the page's
// InputRing in a worker) goes to the driver through nextInput().

const INPUT_MOUSE = 0;
const INPUT_KEYBOARD = 1;

export class Display {
  /**
   * @param {object} [opts]
   * @param {number} [opts.width]
   * @param {number} [opts.height]
   * @param {SharedArrayBuffer} [opts.buffer]  memory for `screen` (width * height * 4 bytes),
   *        e.g. shared with the page that shows it
   * @param {(rect: {left: number, top: number, right: number, bottom: number}) => void} [opts.onChange]
   *        called after a part of the screen changed
   * @param {(display: Display) => void} [opts.inputSource]  called before input is taken;
   *        queues newly arrived events with mouse(), key() or pushInput()
   */
  constructor(opts = {}) {
    this.size = { width: opts.width ?? 800, height: opts.height ?? 600 };
    const bytes = this.size.width * this.size.height * 4;
    this.screen = new Uint8ClampedArray(opts.buffer ?? new ArrayBuffer(bytes), 0, bytes);
    this.onChange = opts.onChange ?? (() => {});
    this.inputSource = opts.inputSource ?? null;
    /** hwnd -> { shown, left, top, right, bottom, ox, oy, width, height, pixels } */
    this.windows = new Map();
    /** top-level windows, bottom first */
    this.order = [];
    this.input = [];
    this.inputWaiter = null;
    this.background = [0x3a, 0x6e, 0xa5, 0xff];
    this.fill(0, 0, this.size.width, this.size.height);
  }

  /** The machine whose memory the surfaces live in. */
  attach(machine) {
    this.m = machine;
  }

  window(hwnd) {
    let w = this.windows.get(hwnd);
    if (!w) {
      w = { shown: false, left: 0, top: 0, right: 0, bottom: 0, ox: 0, oy: 0, width: 0, height: 0, pixels: null };
      this.windows.set(hwnd, w);
      this.order.push(hwnd);
    }
    return w;
  }

  /** A surface's dirty rectangle (surface coordinates) changed; its pixels are BGRX at `bits`. */
  flush(hwnd, ox, oy, width, height, dl, dt, dr, db, bits, stride) {
    const w = this.window(hwnd);
    if (w.width !== width || w.height !== height || !w.pixels) {
      w.pixels = new Uint8ClampedArray(width * height * 4);
      w.width = width;
      w.height = height;
    }
    w.ox = ox;
    w.oy = oy;
    dl = Math.max(0, dl);
    dt = Math.max(0, dt);
    dr = Math.min(width, dr);
    db = Math.min(height, db);
    const src = this.m.u8;
    for (let y = dt; y < db; y++) {
      let s = bits + y * stride + dl * 4;
      let d = (y * width + dl) * 4;
      for (let x = dl; x < dr; x++, s += 4, d += 4) {
        w.pixels[d] = src[s + 2];
        w.pixels[d + 1] = src[s + 1];
        w.pixels[d + 2] = src[s];
        w.pixels[d + 3] = 255;
      }
    }
    if (w.shown) this.compose(w.left + ox + dl, w.top + oy + dt, w.left + ox + dr, w.top + oy + db);
  }

  /** Placement, visibility and stacking of a top-level window (after: window it is now below, 0 top, 1 bottom, -1 unchanged). */
  windowPos(hwnd, shown, left, top, right, bottom, after) {
    const w = this.window(hwnd);
    const old = w.shown ? [w.left, w.top, w.right, w.bottom] : null;
    Object.assign(w, { shown: !!shown, left, top, right, bottom });
    if (after !== -1) {
      this.order.splice(this.order.indexOf(hwnd), 1);
      if (after === 0) this.order.push(hwnd);
      else if (after === 1) this.order.unshift(hwnd);
      else {
        const i = this.order.indexOf(after);
        this.order.splice(i < 0 ? this.order.length : i, 0, hwnd);
      }
    }
    if (old) this.compose(...old);
    if (w.shown) this.compose(left, top, right, bottom);
  }

  destroyWindow(hwnd) {
    const w = this.windows.get(hwnd);
    if (!w) return;
    this.windows.delete(hwnd);
    this.order.splice(this.order.indexOf(hwnd), 1);
    if (w.shown) this.compose(w.left, w.top, w.right, w.bottom);
  }

  fill(l, t, r, b) {
    const [R, G, B, A] = this.background;
    for (let y = t; y < b; y++) {
      for (let x = l, d = (y * this.size.width + l) * 4; x < r; x++, d += 4) {
        this.screen[d] = R;
        this.screen[d + 1] = G;
        this.screen[d + 2] = B;
        this.screen[d + 3] = A;
      }
    }
  }

  /** Redraws a screen rectangle from the windows that cover it, bottom to top. */
  compose(l, t, r, b) {
    const { width: W, height: H } = this.size;
    l = Math.max(0, l);
    t = Math.max(0, t);
    r = Math.min(W, r);
    b = Math.min(H, b);
    if (l >= r || t >= b) return;
    this.fill(l, t, r, b);
    for (const hwnd of this.order) {
      const w = this.windows.get(hwnd);
      if (!w.shown || !w.pixels) continue;
      // The window's surface on the screen, clipped to its visible area.
      const sl = Math.max(l, w.left, w.left + w.ox);
      const st = Math.max(t, w.top, w.top + w.oy);
      const sr = Math.min(r, w.right, w.left + w.ox + w.width);
      const sb = Math.min(b, w.bottom, w.top + w.oy + w.height);
      for (let y = st; y < sb; y++) {
        const sy = y - w.top - w.oy;
        const src = (sy * w.width + (sl - w.left - w.ox)) * 4;
        this.screen.set(w.pixels.subarray(src, src + (sr - sl) * 4), (y * W + sl) * 4);
      }
    }
    this.onChange({ left: l, top: t, right: r, bottom: b });
  }

  // ---- Input --------------------------------------------------------------

  /** Queues a mouse event: screen coordinates and MOUSEEVENTF_* flags (button changes). */
  mouse(x, y, flags = 0, data = 0) {
    // MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE: hardware input (as display
    // drivers send it) is in screen pixels, not SendInput's 0..65535.
    this.pushInput({
      type: INPUT_MOUSE,
      dx: Math.round(x),
      dy: Math.round(y),
      data,
      flags: 0x8001 | flags,
    });
  }

  /** Queues a key press or release: virtual-key code, scan code, KEYEVENTF_* flags. */
  key(vk, scan, flags = 0) {
    this.pushInput({ type: INPUT_KEYBOARD, vk, scan, flags });
  }

  /** Whether input is waiting (after fetching any that arrived). */
  hasInput() {
    this.inputSource?.(this);
    return this.input.length > 0;
  }

  pushInput(ev) {
    this.input.push(ev);
    this.inputWaiter?.();
  }

  /** The driver takes the next event: writes an INPUT structure at `ptr`; 0 when none is queued. */
  nextInput(ptr) {
    if (!this.input.length) this.inputSource?.(this);
    const ev = this.input.shift();
    if (!ev) return 0;
    const dv = this.m.dv;
    this.m.u8.fill(0, ptr, ptr + 28);
    dv.setUint32(ptr, ev.type, true);
    if (ev.type === INPUT_MOUSE) {
      dv.setInt32(ptr + 4, ev.dx, true);
      dv.setInt32(ptr + 8, ev.dy, true);
      dv.setUint32(ptr + 12, ev.data >>> 0, true);
      dv.setUint32(ptr + 16, ev.flags, true);
    } else {
      dv.setUint16(ptr + 4, ev.vk, true);
      dv.setUint16(ptr + 6, ev.scan, true);
      dv.setUint32(ptr + 8, ev.flags, true);
    }
    return 1;
  }

  /** The screen as a PNG file (for tests and screenshots in Node). */
  async png() {
    const { deflateSync, crc32 } = await import('node:zlib');
    const { width: W, height: H } = this.size;
    const raw = Buffer.alloc((W * 4 + 1) * H);
    for (let y = 0; y < H; y++) Buffer.from(this.screen.buffer, y * W * 4, W * 4).copy(raw, y * (W * 4 + 1) + 1);
    const chunk = (type, data) => {
      const len = Buffer.alloc(4);
      len.writeUInt32BE(data.length);
      const td = Buffer.concat([Buffer.from(type), data]);
      const crc = Buffer.alloc(4);
      crc.writeUInt32BE(crc32(td) >>> 0);
      return Buffer.concat([len, td, crc]);
    };
    const ihdr = Buffer.alloc(13);
    ihdr.writeUInt32BE(W, 0);
    ihdr.writeUInt32BE(H, 4);
    ihdr.set([8, 6, 0, 0, 0], 8);
    return Buffer.concat([
      Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
      chunk('IHDR', ihdr),
      chunk('IDAT', deflateSync(raw)),
      chunk('IEND', Buffer.alloc(0)),
    ]);
  }
}
