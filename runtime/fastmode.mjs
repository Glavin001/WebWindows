// Fast mode: translates code discovered while running, using the translator
// compiled to WebAssembly (crates/wwt-wasm). Missed addresses are recorded in
// the machine's profile so the next ahead-of-time pass includes them.

export class FastTranslator {
  static async load(bytes) {
    const { instance } = await WebAssembly.instantiate(bytes, {});
    return new FastTranslator(instance.exports);
  }

  constructor(exports) {
    this.x = exports;
  }

  take(ptr) {
    const mem = new Uint8Array(this.x.memory.buffer);
    const len = new DataView(this.x.memory.buffer).getUint32(ptr, true);
    const out = mem.slice(ptr + 4, ptr + 4 + len);
    this.x.wwt_free(ptr, len + 4);
    return out;
  }

  /** Translates code in `code` (located at `base`) reachable from `entries`. */
  translate(code, base, entries, { known = [], opt = 0, memChecks = true, smcChecks = true } = {}) {
    const x = this.x;
    const cp = x.wwt_alloc(code.length);
    new Uint8Array(x.memory.buffer).set(code, cp);
    const put = (arr) => {
      const p = x.wwt_alloc(Math.max(arr.length, 1) * 4);
      const v = new DataView(x.memory.buffer);
      arr.forEach((e, i) => v.setUint32(p + i * 4, e >>> 0, true));
      return p;
    };
    const ep = put(entries);
    const kp = put(known);
    const flags = (memChecks ? 0 : 1) | (smcChecks ? 0 : 2);
    const res = x.wwt_translate(cp, code.length, base >>> 0, ep, entries.length, kp, known.length, opt, flags);
    x.wwt_free(cp, code.length);
    x.wwt_free(ep, Math.max(entries.length, 1) * 4);
    x.wwt_free(kp, Math.max(known.length, 1) * 4);
    return this.take(res);
  }

  /** Translates a whole PE file; `profile` lists extra entry points. */
  translatePe(file, { profile = [], opt = 1 } = {}) {
    const x = this.x;
    const fp = x.wwt_alloc(file.length);
    new Uint8Array(x.memory.buffer).set(file, fp);
    const pp = x.wwt_alloc(Math.max(profile.length, 1) * 4);
    const v = new DataView(x.memory.buffer);
    profile.forEach((e, i) => v.setUint32(pp + i * 4, e >>> 0, true));
    const res = x.wwt_translate_pe(fp, file.length, pp, profile.length, opt, 0);
    x.wwt_free(fp, file.length);
    x.wwt_free(pp, Math.max(profile.length, 1) * 4);
    return this.take(res);
  }

  kernel() {
    return this.take(this.x.wwt_kernel());
  }

  abi() {
    return JSON.parse(new TextDecoder().decode(this.take(this.x.wwt_abi())));
  }
}

/**
 * Installs run-time translation on a machine: each miss translates a window
 * of guest memory starting at the address and loads the result.
 */
export function enableFastMode(machine, translator, { window = 0x40000, log } = {}) {
  // Translations by address and content: code that patches itself (Quake's
  // software renderers rewrite immediates every frame) misses again after
  // each patch, mostly with bytes it had before, and gets the module that
  // was translated for them back instead of a new one.
  const seen = new Map();
  machine.onMiss = (cpu, addr) => {
    // Within the code section around the address, when the host knows it:
    // the key then covers code only, not data that changes all the time.
    const end = Math.min(addr + window, machine.guestLimit, machine.codeEnd?.(addr) ?? Infinity);
    const code = machine.u8.slice(addr, end);
    const key = `${addr}:${contentHash(code)}`;
    const old = seen.get(key);
    if (old) {
      old.addrs.forEach((a, i) => machine.lookup(a) || machine.register(a, old.base + i));
      return machine.lookup(addr);
    }
    const t0 = performance.now();
    const known = machine.entriesIn(addr, end);
    const bytes = translator.translate(code, addr, [addr], { known });
    if (!bytes.length) return 0;
    const rec = machine.loadModuleSync(bytes, `fast@${addr.toString(16)}`, { keepExisting: true });
    seen.set(key, rec);
    log?.(`fast mode: translated ${addr.toString(16)} (${rec.count} functions, ${bytes.length} bytes) in ${(performance.now() - t0).toFixed(1)} ms`);
    return machine.lookup(addr);
  };
}

/** Two independent 32-bit hashes of bytes (FNV-1a and a murmur-style mix over 32-bit words). */
function contentHash(bytes) {
  let h = 0x811c9dc5;
  let g = 0x9747b28c;
  const words = new Uint32Array(bytes.buffer, bytes.byteOffset, bytes.length >>> 2);
  for (let i = 0; i < words.length; i++) {
    const w = words[i];
    h = Math.imul(h ^ w, 0x01000193);
    g = Math.imul(g ^ Math.imul(w, 0xcc9e2d51), 0x1b873593) + 0xe6546b64;
    g = (g << 13) | (g >>> 19);
  }
  for (let i = words.length * 4; i < bytes.length; i++) h = Math.imul(h ^ bytes[i], 0x01000193);
  return `${(h >>> 0).toString(16)}:${(g >>> 0).toString(16)}:${bytes.length}`;
}
