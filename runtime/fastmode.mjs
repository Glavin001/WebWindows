// Fast mode: translates code discovered while running, using the translator
// compiled to WebAssembly (crates/wwt-wasm). Missed addresses are recorded in
// the machine's profile so the next ahead-of-time pass includes them.

/**
 * Translator flags for a guest limit known at translation time (bits 16-31,
 * in MB): memory checks then compare against a constant, and the runtime
 * accepts the module only under that limit. 0 (or a limit that is not a
 * whole number of MB) leaves the limit to be read at run time.
 */
function limitFlags(guestLimit) {
  return guestLimit && guestLimit % (1 << 20) === 0 ? (guestLimit / (1 << 20)) << 16 : 0;
}

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
  translate(code, base, entries, { known = [], opt = 0, memChecks = true, smcChecks = true, guestLimit = 0 } = {}) {
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
    const flags = (memChecks ? 0 : 1) | (smcChecks ? 0 : 2) | limitFlags(guestLimit);
    const res = x.wwt_translate(cp, code.length, base >>> 0, ep, entries.length, kp, known.length, opt, flags);
    x.wwt_free(cp, code.length);
    x.wwt_free(ep, Math.max(entries.length, 1) * 4);
    x.wwt_free(kp, Math.max(known.length, 1) * 4);
    return this.take(res);
  }

  /** Translates a whole PE file; `profile` lists extra entry points. */
  translatePe(file, { profile = [], opt = 1, guestLimit = 0 } = {}) {
    const x = this.x;
    const fp = x.wwt_alloc(file.length);
    new Uint8Array(x.memory.buffer).set(file, fp);
    const pp = x.wwt_alloc(Math.max(profile.length, 1) * 4);
    const v = new DataView(x.memory.buffer);
    profile.forEach((e, i) => v.setUint32(pp + i * 4, e >>> 0, true));
    const res = x.wwt_translate_pe(fp, file.length, pp, profile.length, opt, limitFlags(guestLimit));
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
  machine.onMiss = (cpu, addr) => {
    const end = Math.min(addr + window, machine.guestLimit);
    const code = machine.u8.slice(addr, end);
    const t0 = performance.now();
    const known = machine.entriesIn(addr, end);
    const bytes = translator.translate(code, addr, [addr], { known, guestLimit: machine.guestLimit });
    if (!bytes.length) return 0;
    const rec = machine.loadModuleSync(bytes, `fast@${addr.toString(16)}`, { keepExisting: true });
    log?.(`fast mode: translated ${addr.toString(16)} (${rec.count} functions, ${bytes.length} bytes) in ${(performance.now() - t0).toFixed(1)} ms`);
    return machine.lookup(addr);
  };
}
