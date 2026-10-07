// Fast mode: translates code discovered while running, using the translator
// compiled to WebAssembly (crates/wwt-wasm). Missed addresses are recorded in
// the machine's profile so the next ahead-of-time pass includes them.

/** Copies addresses (numbers) into the translator's memory as u64s. */
function putAddrs(x, arr) {
  const p = x.wwt_alloc(Math.max(arr.length, 1) * 8);
  const v = new DataView(x.memory.buffer);
  arr.forEach((e, i) => v.setBigUint64(p + i * 8, BigInt(e), true));
  return p;
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

  /**
   * Translates code in `code` (located at `base`) reachable from `entries`;
   * `x64` for x86-64 code, `mem64` for a 64-bit (memory64) memory.
   */
  translate(code, base, entries, { known = [], opt = 0, memChecks = true, smcChecks = true, x64 = false, mem64 = false } = {}) {
    const x = this.x;
    const cp = x.wwt_alloc(code.length);
    new Uint8Array(x.memory.buffer).set(code, cp);
    const ep = putAddrs(x, entries);
    const kp = putAddrs(x, known);
    const flags = (memChecks ? 0 : 1) | (smcChecks ? 0 : 2) | (x64 ? 4 : 0) | (mem64 ? 8 : 0);
    const res = x.wwt_translate(cp, code.length, BigInt(base), ep, entries.length, kp, known.length, opt, flags);
    x.wwt_free(cp, code.length);
    x.wwt_free(ep, Math.max(entries.length, 1) * 8);
    x.wwt_free(kp, Math.max(known.length, 1) * 8);
    return this.take(res);
  }

  /** Translates a whole PE file; `profile` lists extra entry points. */
  translatePe(file, { profile = [], opt = 1, mem64 = false } = {}) {
    const x = this.x;
    const fp = x.wwt_alloc(file.length);
    new Uint8Array(x.memory.buffer).set(file, fp);
    const pp = putAddrs(x, profile);
    const res = x.wwt_translate_pe(fp, file.length, pp, profile.length, opt, mem64 ? 8 : 0);
    x.wwt_free(fp, file.length);
    x.wwt_free(pp, Math.max(profile.length, 1) * 8);
    return this.take(res);
  }

  /** The kernel; `code64` for x86-64 code on a 64-bit memory. */
  kernel({ mem64 = false, code64 = false } = {}) {
    const x = this.x;
    return this.take(code64 ? x.wwt_kernel_code64() : mem64 ? x.wwt_kernel64() : x.wwt_kernel());
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
    const bytes = translator.translate(code, addr, [addr], { known, x64: machine.x64, mem64: machine.mem64 });
    if (!bytes.length) return 0;
    const rec = machine.loadModuleSync(bytes, `fast@${addr.toString(16)}`, { keepExisting: true });
    log?.(`fast mode: translated ${addr.toString(16)} (${rec.count} functions, ${bytes.length} bytes) in ${(performance.now() - t0).toFixed(1)} ms`);
    return machine.lookup(addr);
  };
}
