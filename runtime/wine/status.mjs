// A running program's state, sampled every couple of seconds from the
// runtime's own layers: the scheduler's threads and where each is (an EBP
// backtrace as module+offset), system calls per second and the busiest,
// fixmes, the screen (updates and how much of it is not black), windows,
// input taken from the page, and Direct3D batches. Nothing here knows about
// any particular program; hosts stream the samples (runtime/web/worker.mjs
// posts them to the page, which keeps them in window.webwindows and logs
// them on the console; WWT_STATUS=1 prints them in Node).
//
// The guest runs synchronously, so timers do not fire: the host calls
// tick() from system calls, preemption and idle waits, and a sample is
// taken when the interval has passed.

const hex = (v) => '0x' + (v >>> 0).toString(16);

export class StatusSampler {
  /**
   * @param {object} host  the WineHost
   * @param {object} [opts]
   * @param {object} [opts.display]  runtime/wine/display.mjs, for screen and input
   * @param {number} [opts.intervalMs]
   * @param {(sample: object) => void} opts.emit
   * @param {() => object} [opts.extra]  host-specific fields merged in (output tail)
   */
  constructor(host, opts) {
    this.host = host;
    this.display = opts.display ?? null;
    this.intervalMs = opts.intervalMs ?? 2000;
    this.emit = opts.emit;
    this.extra = opts.extra ?? (() => ({}));
    this.t0 = performance.now();
    this.lastAt = this.t0;
    this.prev = { calls: new Map(), flushes: 0, taken: 0, queued: 0, batches: 0, fixmes: 0, missingSent: new Set(), listingsSent: new Set(), images: 0 };
    this.seq = 0;
  }

  tick() {
    const now = performance.now();
    if (now - this.lastAt < this.intervalMs) return;
    const dt = (now - this.lastAt) / 1000;
    this.lastAt = now;
    try {
      this.emit(this.sample(now, dt));
    } catch (e) {
      this.emit({ seq: this.seq++, error: String(e?.message ?? e) });
    }
  }

  sample(now, dt) {
    const h = this.host;
    const prev = this.prev;
    const rate = (v, p) => Math.round((v - p) / dt);
    // System calls since the last sample, busiest first.
    const calls = new Map();
    let total = 0;
    const top = [];
    for (const e of h.syscallEntries) {
      if (!e?.calls) continue;
      calls.set(e.name, e.calls);
      const d = e.calls - (prev.calls.get(e.name) ?? 0);
      if (d > 0) {
        total += d;
        top.push([e.name, d]);
      }
    }
    top.sort((a, b) => b[1] - a[1]);
    const threads = h.threads.threads
      .filter((t) => t.state !== 'dead')
      .map((t) => ({
        tid: hex(t.tid),
        state: t.state,
        call: t.pending?.name ?? t.inCall ?? null,
        frames: safe(() => h.backtrace(t, 6), []),
      }));
    const s = {
      seq: this.seq++,
      t: Math.round((now - this.t0) / 100) / 10,
      threads,
      syscalls: { perSec: Math.round(total / dt), top: top.slice(0, 5) },
      fixmes: h.fixmes?.size ?? 0,
      unimplemented: [...(h.unimplemented?.keys() ?? [])],
      // Paths looked for and not found since the last sample (the page
      // keeps them all, see host.missingFiles), and how many so far.
      missingFiles: { total: h.missingFiles?.size ?? 0, new: unsent(h.missingFiles, prev.missingSent, (p) => p) },
      // Directory listings since the last sample, [dir\mask, entries].
      dirListings: { total: h.dirListings?.size ?? 0, new: unsent(h.dirListings, prev.listingsSent, (k, n) => [k, n]) },
    };
    const d = this.display;
    if (d) {
      const flushes = d.flushes ?? 0;
      s.screen = { updatesPerSec: rate(flushes, prev.flushes), ...screenStats(d) };
      s.windows = d.order.map((hwnd) => {
        const w = d.windows.get(hwnd);
        return { hwnd: hex(hwnd), shown: !!w?.shown, rect: w ? [w.left, w.top, w.right - w.left, w.bottom - w.top] : null };
      });
      s.input = {
        queued: d.inputQueued ?? 0,
        taken: d.inputTaken ?? 0,
        pending: d.input.length,
        recentKeys: (d.recentKeys ?? []).slice(-6),
      };
      prev.flushes = flushes;
    }
    // The loaded images, when they changed: [base, size, path] (profilers
    // name translated code by its x86 address; tools/web/cdp-profile.mjs).
    if (h.images && h.images.size !== prev.images) {
      s.images = [...h.images].map(([base, img]) => [base, img.size, img.path]);
      prev.images = h.images.size;
    }
    if (h.d3d) {
      s.d3d = { batchesPerSec: rate(h.d3d.batches ?? 0, prev.batches) };
      prev.batches = h.d3d.batches ?? 0;
      // Time the program's thread spent waiting on the render worker, in ms
      // per second: for a full batch slot, and for fences (readbacks).
      const st = h.d3d.stats;
      if (st) {
        const p = (prev.d3dStats ??= {});
        const d = (k) => (st[k] - (p[k] ?? 0)) / dt;
        Object.assign(s.d3d, {
          submitWaitsPerSec: Math.round(d('submitWaits')),
          submitWaitMsPerSec: Math.round(d('submitWaitMs')),
          fenceWaitsPerSec: Math.round(d('fenceWaits')),
          fenceWaitMsPerSec: Math.round(d('fenceWaitMs')),
          mbPerSec: Math.round(d('bytes') / 1e5) / 10,
          readbackMbPerSec: Math.round(d('readbackBytes') / 1e5) / 10,
        });
        Object.assign(p, st);
      }
    }
    prev.calls = calls;
    return { ...s, ...this.extra() };
  }
}

/** A map's entries not sent with an earlier sample, at most 500 (the rest go with the next). */
function unsent(map, sent, entry) {
  const out = [];
  for (const [k, v] of map ?? []) {
    if (sent.has(k)) continue;
    if (out.length === 500) break;
    sent.add(k);
    out.push(entry(k, v));
  }
  return out;
}

function safe(f, fallback) {
  try {
    return f();
  } catch {
    return fallback;
  }
}

/** How much of the composed screen is not black, its mean brightness and
 * how many colours it has, from a 32x24 grid of samples. */
function screenStats(d) {
  const { width, height } = d.size;
  const px = d.screen;
  let lit = 0;
  let sum = 0;
  const colours = new Set();
  const n = 32 * 24;
  for (let gy = 0; gy < 24; gy++) {
    const y = Math.floor(((gy + 0.5) * height) / 24);
    for (let gx = 0; gx < 32; gx++) {
      const x = Math.floor(((gx + 0.5) * width) / 32);
      const i = (y * width + x) * 4;
      const l = (px[i] * 2 + px[i + 1] * 5 + px[i + 2]) / 8;
      sum += l;
      if (l > 8) lit++;
      colours.add(((px[i] >> 4) << 8) | ((px[i + 1] >> 4) << 4) | (px[i + 2] >> 4));
    }
  }
  // Distinct colours (4 bits a channel): about 1 for a cleared screen or a
  // bare desktop, tens for a picture.
  return { nonBlack: Math.round((100 * lit) / n) / 100, brightness: Math.round(sum / n), colours: colours.size };
}

/** One line for a log: time, frames, busiest thread, system calls, input. */
export function formatStatus(s) {
  if (s.error) return `[status] error: ${s.error}`;
  const run = s.threads.find((t) => t.state === 'running') ?? s.threads[0];
  const where = run ? `${run.tid} ${run.state}${run.call ? ` in ${run.call}` : ''} ${run.frames.slice(0, 3).join('<')}` : '-';
  const scr = s.screen ? ` screen ${s.screen.updatesPerSec}/s lit ${Math.round(s.screen.nonBlack * 100)}% ${s.screen.colours}c` : '';
  const d3d = s.d3d
    ? ` d3d ${s.d3d.batchesPerSec}/s${s.d3d.submitWaitMsPerSec !== undefined ? ` (waits: slot ${s.d3d.submitWaitMsPerSec} ms/s, fence ${s.d3d.fenceWaitsPerSec}x ${s.d3d.fenceWaitMsPerSec} ms/s)` : ''}`
    : '';
  const inp = s.input ? ` input ${s.input.taken}/${s.input.queued}` : '';
  return `[${s.t}s]${scr}${d3d} sys ${s.syscalls.perSec}/s (${s.syscalls.top.map(([n]) => n).slice(0, 2).join(',')})${inp} | ${where}`;
}
