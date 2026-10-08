// Keeps the clock in KUSER_SHARED_DATA current, as wineserver does: Wine's
// GetTickCount and the interrupt and system times read it straight from
// that page, without a system call, so a program waiting for the tick count
// to move (a frame limiter) needs it to move while the program runs. A
// small worker writes it every millisecond into the shared memory.
//
// Works as a module worker in browsers and as a worker thread in Node.

export const USER_SHARED_DATA = 0x7ffe0000;
const TICK_COUNT_LOW = 0x0; // TickCountLowDeprecated
const INTERRUPT_TIME = 0x8; // KSYSTEM_TIME, 100 ns since boot
const SYSTEM_TIME = 0x14; // KSYSTEM_TIME, 100 ns since 1601
const TICK_COUNT_MULTIPLIER = 0x4;
const TICK_COUNT = 0x320; // KSYSTEM_TIME, milliseconds since boot
const EPOCH_1601 = 116444736000000000n;

function now() {
  return performance.timeOrigin + performance.now();
}

/** KSYSTEM_TIME: High2Time, LowPart, High1Time, so readers can see a torn write. */
function writeSystemTime(dv, at, v) {
  const hi = Number((v >> 32n) & 0xffffffffn) | 0;
  dv.setInt32(at + 8, hi, true);
  dv.setUint32(at, Number(v & 0xffffffffn), true);
  dv.setInt32(at + 4, hi, true);
}

/** Writes the clock fields for the time since `boot` (epoch milliseconds). */
export function writeClock(dv, boot) {
  const t = now();
  const sinceBoot = t - boot;
  const ms = BigInt(Math.floor(sinceBoot));
  dv.setUint32(USER_SHARED_DATA + TICK_COUNT_MULTIPLIER, 1 << 24, true);
  writeSystemTime(dv, USER_SHARED_DATA + TICK_COUNT, ms);
  dv.setUint32(USER_SHARED_DATA + TICK_COUNT_LOW, Number(ms & 0xffffffffn), true);
  writeSystemTime(dv, USER_SHARED_DATA + INTERRUPT_TIME, BigInt(Math.floor(sinceBoot * 10000)));
  writeSystemTime(dv, USER_SHARED_DATA + SYSTEM_TIME, BigInt(Math.floor(t * 10000)) + EPOCH_1601);
}

/**
 * Starts the ticker for a machine's memory, counting from `boot` (epoch
 * milliseconds). Resolves once the worker runs (a program blocks its own
 * thread, which may keep a worker from starting). Returns {stop()}.
 */
export async function startTicker(memory, boot = now()) {
  writeClock(new DataView(memory.buffer), boot);
  const url = new URL('./ticker-worker.mjs', import.meta.url);
  if (globalThis.process?.versions?.node) {
    const { Worker } = await import('node:worker_threads');
    const w = new Worker(url, { workerData: { buffer: memory.buffer, boot } });
    w.unref();
    await new Promise((ok, fail) => (w.once('message', ok), w.once('error', fail)));
    return { stop: () => w.terminate() };
  }
  const w = new Worker(url, { type: 'module' });
  const ready = new Promise((ok, fail) => ((w.onmessage = ok), (w.onerror = fail)));
  w.postMessage({ buffer: memory.buffer, boot });
  await ready;
  return { stop: () => w.terminate() };
}
