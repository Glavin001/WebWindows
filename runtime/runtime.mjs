// WebWindows runtime: one shared WebAssembly memory laid out like a Windows
// process, the function table, the address lookup used by translated code,
// and the dispatcher kernel.
//
// Memory map (guest limit G, configurable):
//   0x00000000 .. 0x0000FFFF  null region (never mapped)
//   0x00010000 .. G           the Windows process (images, heaps, stacks, TEB)
//   G .. top                  native runtime: lookup tables, code bitmap,
//                             per-thread CPU state, runtime allocations
//
// x86-64 programs (`arch: 'x64'`) use the x86-64 CPU struct. With `mem64`
// the memory is a 64-bit (memory64) one, the CPU pointer and runtime tables
// are i64 for translated code and the first-level lookup table holds 8-byte
// pointers; x86-64 code on such a memory (`code64`) also has 64-bit code
// addresses, so images load at their preferred bases (0x1_4000_0000 for a
// MinGW or MSVC .exe) and the lookup covers the whole guest region. On a
// 32-bit memory, x86-64 images move below 4 GB.
//
// Addresses are JavaScript numbers (exact up to 2^53); guest memory is read
// through `dv` or the `r32`/`w32` helpers, which work above 4 GB, rather
// than through `u32[a >>> 2]`.
//
// Works in Node and in browsers (module workers).

export const PAGE = 0x1000;
const L1_ENTRIES = 1 << 20; // one entry per 4 KB page of the 4 GB space (32-bit code)
const L2_BYTES = PAGE * 4; // one u32 per byte address in a page
const STORE_CODE = 1; // store map bits (wwt::abi::store_map)
const STORE_EDGE = 2;

export class GuestFault extends Error {
  constructor(code, eip, info, message) {
    super(message ?? `guest fault ${hex(code)} at ${hex(eip)} (info ${hex(info)})`);
    this.code = code;
    this.eip = eip;
    this.info = info;
  }
}

export class ProcessExit extends Error {
  constructor(code) {
    super(`process exited with code ${code}`);
    this.exitCode = code;
  }
}

/** Floating-point operations WebAssembly lacks (x87 transcendentals). */
export function hostMath(op, a, b) {
  switch (op) {
    case 0: return Math.sin(a);
    case 1: return Math.cos(a);
    case 2: return Math.tan(a);
    case 3: return Math.atan2(a, b);
    case 4: return Math.log2(a);
    case 5: return Math.pow(2, a) - 1;
    case 6: return a % b;
    case 7: return ieeeRemainder(a, b);
    case 8: return a * Math.pow(2, Math.trunc(b));
    case 9: return Math.log1p(a) / Math.LN2;
    default: return NaN;
  }
}

/** IEEE 754 remainder, exact (quotient rounded to nearest, ties to even). */
function ieeeRemainder(a, b) {
  if (Number.isNaN(a) || Number.isNaN(b) || !Number.isFinite(a) || b === 0) return NaN;
  if (!Number.isFinite(b)) return a;
  const ab = Math.abs(b);
  let r = ab < Number.MAX_VALUE / 2 ? Math.abs(a) % (2 * ab) : Math.abs(a);
  let odd = false;
  if (r >= ab) {
    r -= ab;
    odd = true;
  }
  if (r > ab - r || (r === ab - r && odd)) r -= ab;
  return a < 0 || Object.is(a, -0) ? -r : r;
}

export function hex(v) {
  if (typeof v === 'bigint') v = BigInt.asUintN(64, v);
  else if (v < 0) v = v >>> 0; // i32 values above 2 GB arrive negative
  return '0x' + v.toString(16).padStart(8, '0');
}

/**
 * A shared 64-bit memory of `pages` 64 KB pages: the standard constructor
 * (`address: 'i64'`, BigInt sizes) or the earlier one Node 22 has behind
 * --experimental-wasm-memory64 (`index: 'i64'`, Number sizes).
 */
// A module with one shared 64-bit memory (1 page, at most 1): an engine
// without memory64 rejects it. Creating a memory is no test, since an engine
// that does not know the descriptor's `address` or `index` key ignores it and
// makes a 32-bit memory (WebKit, in every iOS browser).
const MEMORY64_PROBE = new Uint8Array([0, 0x61, 0x73, 0x6d, 1, 0, 0, 0, 5, 4, 1, 7, 1, 1]);

function newMemory64(pages) {
  if (!WebAssembly.validate(MEMORY64_PROBE)) throw new Error('this JavaScript engine has no 64-bit WebAssembly memory');
  try {
    return new WebAssembly.Memory({ address: 'i64', initial: BigInt(pages), maximum: BigInt(pages), shared: true });
  } catch (e) {
    try {
      return new WebAssembly.Memory({ index: 'i64', initial: pages, maximum: pages, shared: true });
    } catch {
      throw new Error(`this JavaScript engine has no 64-bit WebAssembly memory (${e.message})`);
    }
  }
}

/**
 * Whether this engine's stack traces give the module offset of a
 * WebAssembly trap ("wasm-function[N]:0xOFFSET": V8 and SpiderMonkey, not
 * JavaScriptCore), which memory traps need to find the faulting x86
 * instruction (see `Machine.trapSite`). Checked once, on a module that
 * loads past the end of its memory.
 */
let trapsMappableResult;
export function trapsMappable() {
  if (trapsMappableResult !== undefined) return trapsMappableResult;
  // (func (result i32) i32.const 0x20000 i32.load) with a one-page memory.
  const bytes = new Uint8Array([
    0, 0x61, 0x73, 0x6d, 1, 0, 0, 0, 1, 5, 1, 0x60, 0, 1, 0x7f, 3, 2, 1, 0, 5, 3, 1, 0, 1, 7, 5, 1, 1, 102, 0, 0,
    10, 11, 1, 9, 0, 0x41, 0x80, 0x80, 0x08, 0x28, 2, 0, 0x0b,
  ]);
  try {
    new WebAssembly.Instance(new WebAssembly.Module(bytes)).exports.f();
    trapsMappableResult = false;
  } catch (e) {
    trapsMappableResult = e instanceof WebAssembly.RuntimeError && /wasm-function\[\d+\]:0x[0-9a-f]+/.test(String(e.stack));
  }
  return trapsMappableResult;
}

/** Whether this engine has 64-bit (memory64) shared WebAssembly memory. */
export function hasMemory64() {
  try {
    newMemory64(1);
    return true;
  } catch {
    return false;
  }
}

export class Machine {
  /**
   * @param {object} opts
   * @param {object} opts.abi       parsed abi.json from `wwt abi`
   * @param {BufferSource} opts.kernel  kernel.wasm bytes
   * @param {number} [opts.guestLimit]  size of the guest region (default 1 GB);
   *        a multiple of 64 KB
   * @param {number} [opts.nativeSize]  size of the native region (default 64 MB)
   * @param {number} [opts.extraSize]   bytes above the native region for a
   *        module that brings its own allocator (Wine's Unix side, M4)
   */
  constructor(opts) {
    this.abi = opts.abi;
    this.kernelBytes = opts.kernel;
    /** 'x86' or 'x64': the guest's register file and CPU struct layout. */
    this.arch = opts.arch ?? 'x86';
    this.x64 = this.arch === 'x64';
    /** A 64-bit (memory64) WebAssembly memory; needs the matching kernel. */
    this.mem64 = opts.mem64 ?? false;
    /** 64-bit code addresses: x86-64 code on a 64-bit memory. */
    this.code64 = this.x64 && this.mem64;
    this.cpuSize = this.x64 ? this.abi.cpu64.SIZE : this.abi.cpu.SIZE;
    this.guestLimit = opts.guestLimit ?? 0x4000_0000;
    this.nativeSize = opts.nativeSize ?? 64 << 20;
    /** See setCodeWritable(). */
    this.codeWritable = 1;
    /** Called with a page number when the page first gets translated code. */
    this.onCodePage = null;
    this.extraSize = opts.extraSize ?? 0;
    this.thunkSize = 0x10000;
    this.thunkBase = this.guestLimit - this.thunkSize;
    this.log = opts.log ?? (() => {});
    this.hostCalls = new Map(); // thunk address -> handler(cpu) -> next eip
    this.onMiss = null; // (cpu, addr) -> table index, for run-time translation
    this.profile = new Set(); // addresses that missed (for the next AOT pass)
    this.modules = [];
    this.funcCount = 0;
    this.entries = new Set();
    // Native implementations of guest functions, by name: translated
    // modules import them in place of the x86 code (see wwt::builtin, e.g.
    // ntdll's heap from runtime/wine/heap.mjs).
    this.natives = {};
  }

  async init() {
    const total = this.guestLimit + this.nativeSize + this.extraSize;
    if (!this.mem64 && total > 0x1_0000_0000) throw new Error('guest limit + native region exceed 4 GB');
    if (!this.code64 && this.guestLimit > 0x1_0000_0000 && this.thunkBase > 0xffff_0000) {
      // Code addresses (host thunks included) are 32-bit.
      this.thunkBase = 0xffff_0000 - this.thunkSize;
    }
    const pages = total / 65536;
    this.memory = this.mem64 ? newMemory64(pages) : new WebAssembly.Memory({ initial: pages, maximum: pages, shared: true });
    this.table = new WebAssembly.Table({ element: 'anyfunc', initial: 1 });
    this.refreshViews();

    // Native region layout, after an empty guard (see
    // wwt::abi::native_layout::GUARD).
    let p = this.guestLimit + (this.abi.native_layout?.LOOKUP_L1 ?? 0);
    const take = (n, align = 16) => {
      p = Math.ceil(p / align) * align;
      const at = p;
      p += n;
      return at;
    };
    // With 64-bit memory the L1 entries are 8-byte pointers. 64-bit code
    // has one entry per page of the guest region plus a last one that every
    // address above it uses.
    this.l1Entry = this.mem64 ? 8 : 4;
    this.codePages = this.code64 ? Math.ceil(this.guestLimit / PAGE) : L1_ENTRIES;
    const l1Entries = this.code64 ? this.codePages + 1 : L1_ENTRIES;
    this.l1 = take(l1Entries * this.l1Entry, PAGE);
    this.zeroL2 = take(L2_BYTES, PAGE);
    // Store map (see wwt::abi::store_map): one byte per page; non-zero sends
    // stores down the slow path. The null region, the last guest page (an
    // access there can straddle the guest limit) and everything above the
    // guest limit are EDGE; pages with translated code are CODE, and stores
    // there invalidate them in translated code itself (no call to the host).
    // With 64-bit memory it covers the guest region when that is larger than
    // 4 GB (stores there check their address first and read only CODE).
    this.storeMapSize = Math.max(L1_ENTRIES, Math.ceil(this.guestLimit / PAGE));
    this.storeMap = take(this.storeMapSize, PAGE);
    this.cpuArea = take(this.cpuSize * 128, PAGE);
    // The millisecond counter translated loops compare with their thread's
    // slice deadline (cpu.PREEMPT_AT). A host with a clock points tickAddr
    // at its own (the Wine host: KUSER_SHARED_DATA's tick count).
    this.tickAddr = take(16);
    this.nativeNext = p;
    this.nativeEnd = this.guestLimit + this.nativeSize;
    if (p > this.nativeEnd) throw new Error('native region too small for the lookup tables');
    if (this.mem64) {
      new BigUint64Array(this.memory.buffer, this.l1, l1Entries).fill(BigInt(this.zeroL2));
    } else {
      this.u32.fill(this.zeroL2, this.l1 / 4, this.l1 / 4 + L1_ENTRIES);
    }
    this.u8.fill(STORE_EDGE, this.storeMap, this.storeMap + (this.abi.null_limit >>> 12));
    this.u8.fill(STORE_EDGE, this.storeMap + Math.floor(this.guestLimit / PAGE) - 1, this.storeMap + this.storeMapSize);
    this.cpuSlots = 0;

    const env = {
      memory: this.memory,
      table: this.table,
      lookup_l1: this.wide(this.l1),
      thunk_base: this.code(this.thunkBase),
      thunk_size: this.code(this.thunkSize),
      host_call: (cpu, eip) => this.code(this.hostCall(this.addr(cpu), this.addr(eip))),
      miss: (cpu, eip) => this.miss(this.addr(cpu), this.addr(eip)),
    };
    if (this.code64) env.code_pages = BigInt(this.codePages);
    const { instance } = await WebAssembly.instantiate(this.kernelBytes, { env });
    this.kernel = instance.exports;
    this.table.set(0, this.kernel.miss_entry);
    this.table.grow(1);
    this.table.set(1, this.kernel.resume);
  }

  /**
   * A pointer from translated code as a Number: i32 pointers above 2 GB
   * arrive negative, i64 ones as BigInt.
   */
  addr(v) {
    return typeof v === 'bigint' ? Number(BigInt.asUintN(64, v)) : v >>> 0;
  }

  /** A pointer-sized value for translated code (BigInt with 64-bit memory). */
  wide(v) {
    return this.mem64 ? BigInt(v) : v;
  }

  /** A code address for translated code (BigInt with 64-bit code). */
  code(v) {
    return this.code64 ? BigInt(v) : v | 0;
  }

  /** u32 at a guest or native address (any address, unlike `u32[a >>> 2]`). */
  r32(a) {
    return this.dv.getUint32(a, true);
  }
  w32(a, v) {
    this.dv.setUint32(a, v >>> 0, true);
  }

  l1At(page) {
    const a = this.l1 + page * this.l1Entry;
    return this.mem64 ? Number(this.dv.getBigUint64(a, true)) : this.u32[a >>> 2];
  }

  setL1(page, v) {
    const a = this.l1 + page * this.l1Entry;
    if (this.mem64) this.dv.setBigUint64(a, BigInt(v), true);
    else this.u32[a >>> 2] = v;
  }

  refreshViews() {
    const buf = this.memory.buffer;
    this.u8 = new Uint8Array(buf);
    this.u16 = new Uint16Array(buf);
    this.u32 = new Uint32Array(buf);
    this.i32 = new Int32Array(buf);
    this.dv = new DataView(buf);
  }

  nativeAlloc(n, align = 16) {
    const at = Math.ceil(this.nativeNext / align) * align;
    if (at + n > this.nativeEnd) throw new Error('native region exhausted');
    this.nativeNext = at + n;
    return at;
  }

  // ---- CPU state ----------------------------------------------------------

  newCpu() {
    const size = this.cpuSize;
    this.freeCpus ??= [];
    let cpu = this.freeCpus.pop();
    if (cpu === undefined) {
      if (this.cpuSlots >= 128) throw new Error('too many threads');
      cpu = this.cpuArea + size * this.cpuSlots++;
    }
    this.u8.fill(0, cpu, cpu + size);
    this.w32(cpu + this.abi.cpu.PREEMPT_AT, 0xffffffff);
    // x87 control word: 64-bit precision, round to nearest, all masked.
    this.dv.setUint16(cpu + this.abi.cpu.FPU_CW, 0x037f, true);
    this.dv.setUint32(cpu + this.abi.cpu.MXCSR, 0x1f80, true);
    this.w32(cpu + this.abi.cpu.CODE_WRITABLE, this.codeWritable);
    return cpu;
  }

  /**
   * Whether translated code may be written (wwt::abi::cpu::CODE_WRITABLE):
   * while it is, stores in translated code look up the store map, which
   * notices writes to code. A host that knows no page with translated code
   * is writable clears it; the default is on.
   */
  setCodeWritable(on) {
    const v = on ? 1 : 0;
    if (v === this.codeWritable) return;
    this.codeWritable = v;
    for (let i = 0; i < this.cpuSlots; i++) this.w32(this.cpuArea + i * this.cpuSize + this.abi.cpu.CODE_WRITABLE, v);
  }

  /**
   * A translated loop's slice deadline passed. `onPreempt` (a host with
   * threads) returns whether it switched threads, the current one to resume
   * at `eip`; otherwise the deadline goes away.
   */
  preempt(cpu, eip) {
    if (this.onPreempt) return this.onPreempt(cpu, eip) ? 1 : 0;
    this.w32(cpu + this.abi.cpu.PREEMPT_AT, 0xffffffff);
    return 0;
  }

  /** Returns a thread's CPU state slot for reuse. */
  freeCpu(cpu) {
    this.freeCpus.push(this.addr(cpu));
  }

  /** General register `i` (the low 32 bits on x86-64). */
  reg(cpu, i) {
    if (this.x64) return this.r32(cpu + this.abi.cpu64.GPR + i * 8);
    return this.r32(cpu + this.abi.cpu.GPR + i * 4);
  }
  /** Sets a general register; on x86-64 zero-extended, as a 32-bit write. */
  setReg(cpu, i, v) {
    if (this.x64) return this.setReg64(cpu, i, BigInt(v >>> 0));
    this.w32(cpu + this.abi.cpu.GPR + i * 4, v);
  }
  /** x86-64: the whole register as a BigInt. */
  reg64(cpu, i) {
    return this.dv.getBigUint64(cpu + this.abi.cpu64.GPR + i * 8, true);
  }
  setReg64(cpu, i, v) {
    this.dv.setBigUint64(cpu + this.abi.cpu64.GPR + i * 8, BigInt.asUintN(64, BigInt(v)), true);
  }
  /** The stack pointer (a Number: guest addresses fit in 53 bits). */
  sp(cpu) {
    return this.x64 ? Number(this.reg64(cpu, 4)) : this.reg(cpu, 4);
  }
  setSp(cpu, v) {
    if (this.x64) this.setReg64(cpu, 4, BigInt(v));
    else this.setReg(cpu, 4, v);
  }
  /** Reads and writes guest pointers (4 or 8 bytes). */
  ptr(addr) {
    return this.x64 ? Number(this.dv.getBigUint64(addr, true)) : this.dv.getUint32(addr, true);
  }
  setPtr(addr, v) {
    if (this.x64) this.dv.setBigUint64(addr, BigInt.asUintN(64, BigInt(v)), true);
    else this.dv.setUint32(addr, v >>> 0, true);
  }

  // ---- Modules and lookup -------------------------------------------------

  /** Instantiates a translated module and registers its functions. */
  async loadModule(bytes, name = 'module', opts = {}) {
    const module = await WebAssembly.compile(bytes);
    return this.instantiateModule(module, name, opts, (m, imports) => WebAssembly.instantiate(m, imports));
  }

  /** Synchronous variant, for run-time translation inside a miss. */
  loadModuleSync(bytes, name = 'module', opts = {}) {
    const module = new WebAssembly.Module(bytes);
    return this.instantiateModule(module, name, opts, (m, imports) => new WebAssembly.Instance(m, imports));
  }

  /** Synchronous instantiation of an already compiled module. */
  loadCompiledSync(module, name = 'module', opts = {}) {
    return this.instantiateModule(module, name, opts, (m, imports) => new WebAssembly.Instance(m, imports));
  }

  instantiateModule(module, name, opts, instantiate) {
    const sections = (n) => WebAssembly.Module.customSections(module, n);
    const metaSec = sections(this.abi.meta_section)[0];
    let addrs;
    const funcs64 = sections(this.abi.funcs64_section)[0];
    if (funcs64) {
      if (!this.code64) throw new Error(`${name}: 64-bit code needs an x86-64 machine with 64-bit memory`);
      addrs = Array.from(new BigUint64Array(funcs64.slice(4)), Number);
    } else {
      if (this.code64) throw new Error(`${name}: translated for 32-bit code addresses (translate with --mem64)`);
      addrs = new Uint32Array(sections(this.abi.funcs_section)[0].slice(4));
    }
    const meta = metaSec ? JSON.parse(new TextDecoder().decode(metaSec)) : null;
    if (meta && meta.abi_version !== undefined && meta.abi_version !== this.abi.version) {
      throw new Error(`${name}: ABI version ${meta.abi_version}, runtime expects ${this.abi.version}`);
    }
    // Translated with the guest limit as a constant in its memory checks.
    // Such a module also uses the native tables' addresses as constants
    // (wwt::abi::native_layout). (Code for a 64-bit memory reads them at
    // run time, whatever the module says.)
    const layout = this.abi.native_layout;
    const fixed = meta?.guest_limit !== undefined && !this.mem64;
    if (
      fixed &&
      (this.l1 !== this.guestLimit + layout.LOOKUP_L1 ||
        this.zeroL2 !== this.guestLimit + layout.ZERO_L2 ||
        this.storeMap !== this.guestLimit + layout.STORE_MAP)
    ) {
      throw new Error(`${name}: native region layout differs from wwt::abi::native_layout`);
    }
    if (fixed && meta.guest_limit !== this.guestLimit) {
      throw new Error(
        `${name}: translated for a guest limit of ${meta.guest_limit >>> 20} MB, this machine has ` +
          `${this.guestLimit >>> 20} MB (translate with --guest-limit-mb ${this.guestLimit >>> 20})`,
      );
    }
    const base = this.table.length;
    this.table.grow(addrs.length);
    const env = {
      ...this.natives,
      memory: this.memory,
      table: this.table,
      table_base: base,
      lookup_l1: this.wide(this.l1),
      guest_limit: this.wide(this.guestLimit - 0x10000 - 16),
      store_map: this.wide(this.storeMap),
      zero_l2: this.wide(this.zeroL2),
      // Returns where to continue (with exception dispatch).
      fault: (cpu, code, eip, info) => this.code(this.fault(this.addr(cpu), code, this.addr(eip), this.addr(info))),
      math: hostMath,
      // The builtins themselves, so engines call them directly.
      sin: Math.sin,
      cos: Math.cos,
      preempt: (cpu, eip) => this.preempt(this.addr(cpu), this.addr(eip)),
      tick: this.wide(this.tickAddr),
    };
    if (this.code64) env.code_pages = BigInt(this.codePages);
    const traps = this.abi.traps_section && sections(this.abi.traps_section)[0];
    const finish = (instance) => {
      for (let i = 0; i < addrs.length; i++) {
        if (opts.keepExisting && this.lookup(addrs[i])) continue;
        this.register(addrs[i], base + i);
      }
      this.funcCount += addrs.length;
      const rec = { name, base, count: addrs.length, meta, instance, addrs };
      if (traps) this.addTraps(rec, traps);
      this.modules.push(rec);
      this.log(`loaded ${name}: ${addrs.length} functions at table ${base}`);
      return rec;
    };
    const r = instantiate(module, { env });
    return r instanceof Promise ? r.then((x) => finish(x.instance ?? x)) : finish(r);
  }

  // ---- Fast mode: bounds traps as faults ---------------------------------------

  /**
   * Records a module's trapping accesses (`wwt.traps`, see
   * `CodegenConfig::mem_traps`): their offsets in the module, keyed by the
   * module's function entries, which name its functions in stack traces.
   */
  addTraps(rec, section) {
    const d = new Uint32Array(section.slice(0, section.byteLength & ~3));
    rec.traps = new Map();
    for (let i = 0, e = 1; i < d[0]; i++, e += 4) rec.traps.set(d[e], { eip: d[e + 1], flags: d[e + 2], disp: d[e + 3] });
    this.trapModules ??= new Map();
    for (const a of rec.addrs) this.trapModules.set(Number(a), rec);
  }

  /**
   * The trapping access an engine trap stopped at, from the innermost
   * WebAssembly frame of its stack trace (V8's and SpiderMonkey's formats),
   * or null when the trap was elsewhere.
   */
  trapSite(e) {
    if (!this.trapModules || !(e instanceof WebAssembly.RuntimeError)) return null;
    const line = String(e.stack ?? '').split('\n').find((l) => l.includes('wasm-function['));
    const at = line && /wasm-function\[\d+\]:0x([0-9a-f]+)/.exec(line);
    if (!at) return null;
    // V8: "at NAME (URL:wasm-function[i]:0xOFF)"; SpiderMonkey: "NAME@URL...".
    const name = (/^\s*at (\S+) \(/.exec(line) ?? /^(.*?)@[a-z-]+:/.exec(line))?.[1] ?? '';
    const entry = /(?:@|^x86_)([0-9a-f]+)$/.exec(name);
    const rec = entry && this.trapModules.get(parseInt(entry[1], 16));
    return rec?.traps.get(parseInt(at[1], 16)) ?? null;
  }

  /**
   * The address a trapping access accessed, from its x86 memory operand and
   * the registers as last written back (0 when the operand is not known).
   */
  trapAddress(cpu, t) {
    const T = this.abi.trap;
    if (!(t.flags & T.OPERAND)) return 0;
    const reg = (shift) => {
      const r = (t.flags >>> shift) & 15;
      return r ? this.reg(cpu, r - 1) : 0;
    };
    const scale = (t.flags >>> T.SCALE_SHIFT) & 3;
    const seg = (t.flags >>> T.SEG_SHIFT) & 3;
    const segBase = seg ? this.r32(cpu + (seg === 1 ? this.abi.cpu.FS_BASE : this.abi.cpu.GS_BASE)) : 0;
    return (reg(T.BASE_SHIFT) + reg(T.INDEX_SHIFT) * (1 << scale) + t.disp + segBase) >>> 0;
  }

  /** Registered function entries in [lo, hi). */
  entriesIn(lo, hi) {
    const out = [];
    for (const a of this.entries) if (a >= lo && a < hi) out.push(a);
    return out;
  }

  /** The lookup page of a code address (the overflow page above the guest region). */
  codePage(addr) {
    const page = Math.floor(addr / PAGE);
    return page < this.codePages ? page : this.codePages;
  }

  register(addr, index) {
    if (!this.code64 && addr > 0xffff_ffff) throw new Error(`code address ${hex(addr)} above 4 GB`);
    if (addr >= this.guestLimit) throw new Error(`code address ${hex(addr)} above the guest limit`);
    this.entries.add(addr);
    const page = this.codePage(addr);
    let l2 = this.l1At(page);
    if (l2 === this.zeroL2) {
      // A store into translated code points the page back at zero_l2 (in
      // translated code); the page's old table is reused, or code that
      // patches itself every frame would use up the native region.
      this.pageL2 ??= new Map();
      l2 = this.pageL2.get(page);
      if (l2 === undefined) this.pageL2.set(page, (l2 = this.nativeAlloc(L2_BYTES, PAGE)));
      this.u8.fill(0, l2, l2 + L2_BYTES);
      this.setL1(page, l2);
      this.u8[this.storeMap + page] |= STORE_CODE;
      this.onCodePage?.(page);
    }
    this.w32(l2 + (addr % PAGE) * 4, index);
  }

  lookup(addr) {
    const l2 = this.l1At(this.codePage(addr));
    return this.r32(l2 + (addr % PAGE) * 4);
  }

  /** Table slot that continues at `eip` (for a miss turned into an exception). */
  resumeAt(cpu, eip) {
    if (this.x64) this.dv.setBigUint64(cpu + this.abi.cpu64.RIP, BigInt(eip), true);
    else this.u32[((cpu >>> 0) + this.abi.cpu.EIP) >>> 2] = eip >>> 0;
    return 1;
  }

  miss(cpu, addr) {
    const F = this.abi.fault;
    // With exception dispatch, jumping to memory with nothing there raises
    // an access violation in the guest.
    if (this.onFault && this.canExecute && !this.canExecute(addr)) return this.resumeAt(cpu, this.fault(cpu, F.ACCESS_VIOLATION_EXECUTE, addr, addr));
    this.profile.add(addr);
    if (this.onMiss) {
      const idx = this.onMiss(cpu, addr);
      if (idx) return idx;
      if (this.onFault) return this.resumeAt(cpu, this.fault(cpu, F.ILLEGAL_INSTRUCTION, addr, 0));
    }
    throw new GuestFault(
      this.abi.fault.ACCESS_VIOLATION,
      addr,
      addr,
      `no translated code at ${hex(addr)} (jumped to from guest code; ` +
        `add it with --seed or enable run-time translation)`,
    );
  }

  /**
   * A fault in translated code. `onFault` (set by a host with exception
   * dispatch) returns the address to continue at; otherwise the fault stops
   * the program.
   */
  fault(cpu, code, eip, info, trap = false) {
    if (this.onFault) {
      const next = this.onFault(cpu, code >>> 0, eip, info, trap);
      if (next !== undefined) return next;
    }
    const names = Object.entries(this.abi.fault).find(([, v]) => v === code >>> 0);
    throw new GuestFault(
      code >>> 0,
      eip,
      info,
      `${names ? names[0] : hex(code)} at ${hex(eip)} (address/info ${hex(info)})`,
    );
  }

  /** Full eflags of a thread, with its lazy arithmetic flags evaluated. */
  eflags(cpu) {
    const A = this.abi.cpu;
    const f = (o) => this.i32[(cpu + o) >>> 2];
    const evaluate = this.modules[0].instance.exports.eflags;
    const arith = evaluate(f(A.FK), f(A.FR), f(A.FA), f(A.FB), f(A.FC));
    return ((arith & 0x8d5) | (f(A.DF) << 10) | f(A.EFLAGS_SYS) | 2) >>> 0;
  }

  // ---- Host calls -----------------------------------------------------------

  /** Allocates a thunk address that runs `handler(cpu)` when called. */
  addThunk(handler) {
    const at = this.thunkBase + this.hostCalls.size * 16;
    if (at >= this.thunkBase + this.thunkSize) throw new Error('too many host thunks');
    this.hostCalls.set(at, handler);
    return at;
  }

  /** Runs the host thunk at `eip`; returns the address to continue at. */
  hostCall(cpu, eip) {
    const h = this.hostCalls.get(eip);
    if (!h) throw new Error(`call to unknown host thunk ${hex(eip)}`);
    const next = h(cpu);
    return this.code64 ? next : next >>> 0;
  }

  /** The address that stops the dispatcher when guest code returns to it. */
  get stopAddress() {
    return this.code64 ? this.abi.stop_address64 : this.abi.stop_address >>> 0;
  }

  /** The address a host returns to make the dispatcher stop and switch threads. */
  get yieldAddress() {
    return this.code64 ? this.abi.yield_address64 : this.abi.yield_address >>> 0;
  }

  /**
   * Runs guest code at `eip` until it returns to the stop address. A bounds
   * trap at a trapping access (fast mode) is an access violation at its
   * instruction: the registers are as last written back, and the address
   * accessed is not known (reported as 0).
   */
  run(cpu, eip) {
    for (;;) {
      try {
        return this.addr(this.kernel.run(this.wide(cpu), this.code(eip)));
      } catch (e) {
        const t = this.trapSite(e);
        if (!t) throw e;
        const F = this.abi.fault;
        this.resumeAt(cpu, t.eip);
        const write = t.flags & this.abi.trap.WRITE;
        eip = this.fault(cpu, write ? F.ACCESS_VIOLATION_WRITE : F.ACCESS_VIOLATION, t.eip, this.trapAddress(cpu, t), true);
      }
    }
  }

  /**
   * Calls a guest function with 32-bit arguments (pushed right to left) and
   * returns eax. Works for cdecl and stdcall callees.
   */
  callGuest(cpu, addr, args = []) {
    if (this.x64) return this.callGuest64(cpu, addr, args);
    const ESP = 4;
    const saved = this.reg(cpu, ESP);
    let sp = saved;
    for (let i = args.length - 1; i >= 0; i--) {
      sp -= 4;
      this.w32(sp, args[i]);
    }
    sp -= 4;
    this.w32(sp, this.stopAddress);
    this.setReg(cpu, ESP, sp);
    this.run(cpu, addr);
    this.setReg(cpu, ESP, saved);
    return this.reg(cpu, 0);
  }

  /**
   * x86-64 (Windows x64 convention): the first four arguments in rcx, rdx,
   * r8 and r9, the rest on the stack above 32 bytes of home space; returns
   * eax (use `reg64` for all of rax).
   */
  callGuest64(cpu, addr, args = []) {
    const saved = this.sp(cpu);
    const regs = [1, 2, 8, 9];
    const stackArgs = Math.max(0, args.length - 4);
    // Keep rsp 16-byte aligned at the call (8 off after the return address).
    let sp = (saved - 8 * stackArgs - 32) & ~15;
    for (let i = 0; i < args.length; i++) {
      const v = BigInt.asUintN(64, BigInt(args[i]));
      if (i < 4) this.setReg64(cpu, regs[i], v);
      else this.dv.setBigUint64(sp + 32 + (i - 4) * 8, v, true);
    }
    sp -= 8;
    this.dv.setBigUint64(sp, BigInt(this.stopAddress), true);
    this.setSp(cpu, sp);
    this.run(cpu, addr);
    this.setSp(cpu, saved);
    return this.reg(cpu, 0);
  }

  // ---- Guest memory helpers ---------------------------------------------------

  readCString(addr, max = 1 << 20) {
    let end = addr;
    while (this.u8[end] !== 0 && end - addr < max) end++;
    return new TextDecoder('latin1').decode(this.u8.slice(addr, end));
  }

  readWString(addr, max = 1 << 20) {
    let s = '';
    for (let i = 0; i < max; i++) {
      const c = this.dv.getUint16(addr + i * 2, true);
      if (c === 0) break;
      s += String.fromCharCode(c);
    }
    return s;
  }

  writeCString(addr, s) {
    for (let i = 0; i < s.length; i++) this.u8[addr + i] = s.charCodeAt(i) & 0xff;
    this.u8[addr + s.length] = 0;
  }

  writeWString(addr, s) {
    for (let i = 0; i < s.length; i++) this.dv.setUint16(addr + i * 2, s.charCodeAt(i), true);
    this.dv.setUint16(addr + s.length * 2, 0, true);
  }
}
