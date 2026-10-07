#!/usr/bin/env node
// Spike (M1): how large a shared WebAssembly memory can the engine give us,
// and do addresses above 2 GB work from WebAssembly and JavaScript?
//
// The memory design puts the Windows process in the low `guest limit` bytes
// and the native runtime above it, so the total must fit. Run in Node (V8)
// here; spikes/memory/index.html runs the same checks in a browser.

const PAGE = 65536;
const results = [];

function tryMemory(bytes) {
  const pages = Math.ceil(bytes / PAGE);
  const t0 = performance.now();
  try {
    const m = new WebAssembly.Memory({ initial: pages, maximum: pages, shared: true });
    const ms = performance.now() - t0;
    // Touch the last page from JS through an unsigned index.
    const u8 = new Uint8Array(m.buffer);
    u8[u8.length - 1] = 0xab;
    return { ok: u8[u8.length - 1] === 0xab, ms, memory: m };
  } catch (e) {
    return { ok: false, error: String(e.message ?? e) };
  }
}

// A tiny module that stores and loads at a runtime address, to check that
// addresses above 2^31 are treated as unsigned.
const wat = new Uint8Array([
  0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
  // type: (i32, i32) -> i32
  0x01, 0x07, 0x01, 0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f,
  // import env.memory shared (min 1, max 65536)
  0x02, 0x12, 0x01, 0x03, 0x65, 0x6e, 0x76, 0x06, 0x6d, 0x65, 0x6d, 0x6f, 0x72, 0x79, 0x02, 0x03, 0x01, 0x80, 0x80, 0x04,
  // function section
  0x03, 0x02, 0x01, 0x00,
  // export "rw"
  0x07, 0x06, 0x01, 0x02, 0x72, 0x77, 0x00, 0x00,
  // code: (local.get 0) (local.get 1) (i32.store) (local.get 0) (i32.load)
  0x0a, 0x10, 0x01, 0x0e, 0x00, 0x20, 0x00, 0x20, 0x01, 0x36, 0x02, 0x00, 0x20, 0x00, 0x28, 0x02, 0x00, 0x0b,
]);

const sizes = [
  ['1 GB guest + 64 MB native', (1 << 30) + (64 << 20)],
  ['2 GB guest + 64 MB native', 2 ** 31 + (64 << 20)],
  ['3 GB', 3 * 2 ** 30],
  ['4 GB - 64 KB', 2 ** 32 - PAGE],
];
for (const [name, bytes] of sizes) {
  const r = tryMemory(bytes);
  let high = null;
  if (r.ok && bytes > 2 ** 31) {
    const { instance } = await WebAssembly.instantiate(wat, { env: { memory: r.memory } });
    const addr = 2 ** 31 + 0x1000; // above 2 GB, passed as a negative i32
    const v = instance.exports.rw(addr | 0, 0x1234567);
    const js = new DataView(r.memory.buffer).getUint32(addr, true);
    high = v === 0x1234567 && js === 0x1234567;
  }
  results.push({ name, bytes, ok: r.ok, ms: r.ms?.toFixed(1), highAddresses: high, error: r.error });
  r.memory = null;
}
console.log(`engine: ${typeof process !== 'undefined' ? `Node ${process.version} (V8 ${process.versions.v8})` : navigator.userAgent}`);
console.table(results);
const required = results[0];
if (!required.ok) {
  console.error('FAIL: the default layout (1 GB guest) cannot be allocated');
  process.exit(1);
}
