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
// x86-64 programs (`arch: 'x64'`) use the same map with code below 4 GB and
// the x86-64 CPU struct; with `mem64` the memory is a 64-bit (memory64) one,
// the CPU pointer and runtime tables are i64 for translated code, and the
// first-level lookup table holds 8-byte pointers.
//
// Works in Node and in browsers (module workers).

export const PAGE = 0x1000;
const L1_ENTRIES = 1 << 20; // one entry per 4 KB page of the 4 GB space
const L2_BYTES = PAGE * 4; // one u32 per byte address in a page

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
  return '0x' + (v >>> 0).toString(16).padStart(8, '0');
}

/**
 * A shared 64-bit memory of `pages` 64 KB pages: the standard constructor
 * (`address: 'i64'`, BigInt sizes) or the earlier one Node 22 has behind
 * --experimental-wasm-memory64 (`index: 'i64'`, Number sizes).
 */
function newMemory64(pages) {
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

export class Machine {
  /**
   * @param {object} opts
   * @param {object} opts.abi       parsed abi.json from `wwt abi`
   * @param {BufferSource} opts.kernel  kernel.wasm bytes
   * @param {number} [opts.guestLimit]  size of the guest region (default 1 GB)
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
    this.cpuSize = this.x64 ? this.abi.cpu64.SIZE : this.abi.cpu.SIZE;
    this.guestLimit = opts.guestLimit ?? 0x4000_0000;
    this.nativeSize = opts.nativeSize ?? 64 << 20;
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
  }

  async init() {
    const total = this.guestLimit + this.nativeSize + this.extraSize;
    if (!this.mem64 && total > 0x1_0000_0000) throw new Error('guest limit + native region exceed 4 GB');
    if (this.guestLimit > 0x1_0000_0000 && this.thunkBase > 0xffff_0000) {
      // Code addresses (host thunks included) are 32-bit.
      this.thunkBase = 0xffff_0000 - this.thunkSize;
    }
    const pages = total / 65536;
    this.memory = this.mem64 ? newMemory64(pages) : new WebAssembly.Memory({ initial: pages, maximum: pages, shared: true });
    this.table = new WebAssembly.Table({ element: 'anyfunc', initial: 1 });
    this.refreshViews();

    // Native region layout.
    let p = this.guestLimit;
    const take = (n, align = 16) => {
      p = Math.ceil(p / align) * align;
      const at = p;
      p += n;
      return at;
    };
    // With 64-bit memory the L1 entries are 8-byte pointers.
    this.l1Entry = this.mem64 ? 8 : 4;
    this.l1 = take(L1_ENTRIES * this.l1Entry, PAGE);
    this.zeroL2 = take(L2_BYTES, PAGE);
    // One bit per 4 KB page of the guest region (at least the 4 GB of code).
    this.codeBitmap = take(Math.max(L1_ENTRIES, this.guestLimit / PAGE) / 8, PAGE);
    this.cpuArea = take(this.cpuSize * 128, PAGE);
    this.nativeNext = p;
    this.nativeEnd = this.guestLimit + this.nativeSize;
    if (this.mem64) {
      for (let i = 0; i < L1_ENTRIES; i++) this.setL1(i, this.zeroL2);
    } else {
      this.u32.fill(this.zeroL2, this.l1 >>> 2, (this.l1 >>> 2) + L1_ENTRIES);
    }
    this.cpuSlots = 0;

    const env = {
      memory: this.memory,
      table: this.table,
      lookup_l1: this.wide(this.l1),
      thunk_base: this.thunkBase,
      thunk_size: this.thunkSize,
      host_call: (cpu, eip) => this.hostCall(this.addr(cpu), eip),
      miss: (cpu, eip) => this.miss(this.addr(cpu), eip),
    };
    const { instance } = await WebAssembly.instantiate(this.kernelBytes, { env });
    this.kernel = instance.exports;
    this.table.set(0, this.kernel.miss_entry);
  }

  /**
   * A pointer from translated code as a Number: i32 pointers above 2 GB
   * arrive negative, i64 ones as BigInt.
   */
  addr(v) {
    return typeof v === 'bigint' ? Number(v) : v >>> 0;
  }

  /** A pointer-sized value for translated code (BigInt with 64-bit memory). */
  wide(v) {
    return this.mem64 ? BigInt(v) : v;
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
    if (this.cpuSlots >= 128) throw new Error('too many threads');
    const cpu = this.cpuArea + size * this.cpuSlots++;
    this.u8.fill(0, cpu, cpu + size);
    // x87 control word: 64-bit precision, round to nearest, all masked.
    this.dv.setUint16(cpu + this.abi.cpu.FPU_CW, 0x037f, true);
    this.dv.setUint32(cpu + this.abi.cpu.MXCSR, 0x1f80, true);
    return cpu;
  }

  /** General register `i` (the low 32 bits on x86-64). */
  reg(cpu, i) {
    if (this.x64) return this.u32[(cpu + this.abi.cpu64.GPR + i * 8) >>> 2];
    return this.u32[(cpu + this.abi.cpu.GPR + i * 4) >>> 2];
  }
  /** Sets a general register; on x86-64 zero-extended, as a 32-bit write. */
  setReg(cpu, i, v) {
    if (this.x64) return this.setReg64(cpu, i, BigInt(v >>> 0));
    this.u32[(cpu + this.abi.cpu.GPR + i * 4) >>> 2] = v >>> 0;
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
    const funcsSec = WebAssembly.Module.customSections(module, this.abi.funcs_section)[0];
    const metaSec = WebAssembly.Module.customSections(module, this.abi.meta_section)[0];
    const addrs = new Uint32Array(funcsSec.slice(4));
    const meta = metaSec ? JSON.parse(new TextDecoder().decode(metaSec)) : null;
    if (meta && meta.abi_version !== undefined && meta.abi_version !== this.abi.version) {
      throw new Error(`${name}: ABI version ${meta.abi_version}, runtime expects ${this.abi.version}`);
    }
    const base = this.table.length;
    this.table.grow(addrs.length);
    const env = {
      memory: this.memory,
      table: this.table,
      table_base: base,
      lookup_l1: this.wide(this.l1),
      guest_limit: this.wide(this.guestLimit - 0x10000 - 16),
      code_bitmap: this.wide(this.codeBitmap),
      fault: (cpu, code, eip, info) => this.fault(this.addr(cpu), code, eip, info),
      code_write: (cpu, addr) => this.codeWrite(this.addr(cpu), this.addr(addr)),
      math: hostMath,
    };
    const finish = (instance) => {
      for (let i = 0; i < addrs.length; i++) {
        if (opts.keepExisting && this.lookup(addrs[i])) continue;
        this.register(addrs[i], base + i);
      }
      this.funcCount += addrs.length;
      const rec = { name, base, count: addrs.length, meta, instance };
      this.modules.push(rec);
      this.log(`loaded ${name}: ${addrs.length} functions at table ${base}`);
      return rec;
    };
    const r = instantiate(module, { env });
    return r instanceof Promise ? r.then((x) => finish(x.instance ?? x)) : finish(r);
  }

  /** Registered function entries in [lo, hi). */
  entriesIn(lo, hi) {
    const out = [];
    for (const a of this.entries) if (a >= lo && a < hi) out.push(a);
    return out;
  }

  register(addr, index) {
    this.entries.add(addr >>> 0);
    const page = addr >>> 12;
    let l2 = this.l1At(page);
    if (l2 === this.zeroL2) {
      l2 = this.nativeAlloc(L2_BYTES, PAGE);
      this.u8.fill(0, l2, l2 + L2_BYTES);
      this.setL1(page, l2);
      this.u8[this.codeBitmap + (page >>> 3)] |= 1 << (page & 7);
    }
    this.u32[(l2 >>> 2) + (addr & 0xfff)] = index;
  }

  lookup(addr) {
    const l2 = this.l1At(addr >>> 12);
    return this.u32[(l2 / 4) + (addr & 0xfff)];
  }

  /** A store hit a page with translated code: drop the page's translations. */
  codeWrite(cpu, addr) {
    const page = addr >>> 12;
    this.log(`code write at ${hex(addr)}: invalidating page ${hex(page << 12)}`);
    this.setL1(page, this.zeroL2);
    this.u8[this.codeBitmap + (page >>> 3)] &= ~(1 << (page & 7));
  }

  miss(cpu, addr) {
    this.profile.add(addr >>> 0);
    if (this.onMiss) {
      const idx = this.onMiss(cpu, addr >>> 0);
      if (idx) return idx;
    }
    throw new GuestFault(
      this.abi.fault.ACCESS_VIOLATION,
      addr,
      addr,
      `no translated code at ${hex(addr)} (jumped to from guest code; ` +
        `add it with --seed or enable run-time translation)`,
    );
  }

  fault(cpu, code, eip, info) {
    if (this.onFault) {
      this.onFault(cpu, code >>> 0, eip >>> 0, info >>> 0);
    }
    const names = Object.entries(this.abi.fault).find(([, v]) => v === code >>> 0);
    throw new GuestFault(
      code >>> 0,
      eip >>> 0,
      info >>> 0,
      `${names ? names[0] : hex(code)} at ${hex(eip)} (address/info ${hex(info)})`,
    );
  }

  // ---- Host calls -----------------------------------------------------------

  /** Allocates a thunk address that runs `handler(cpu)` when called. */
  addThunk(handler) {
    const at = this.thunkBase + this.hostCalls.size * 16;
    if (at >= this.thunkBase + this.thunkSize) throw new Error('too many host thunks');
    this.hostCalls.set(at, handler);
    return at;
  }

  hostCall(cpu, eip) {
    const h = this.hostCalls.get(eip >>> 0);
    if (!h) throw new Error(`call to unknown host thunk ${hex(eip)}`);
    return h(cpu) >>> 0;
  }

  /** Runs guest code at `eip` until it returns to the stop address. */
  run(cpu, eip) {
    return this.kernel.run(this.wide(cpu), eip);
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
      this.u32[sp >>> 2] = args[i] >>> 0;
    }
    sp -= 4;
    this.u32[sp >>> 2] = this.abi.stop_address >>> 0;
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
    this.dv.setBigUint64(sp, BigInt(this.abi.stop_address >>> 0), true);
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
      const c = this.u16[(addr >>> 1) + i];
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
    for (let i = 0; i < s.length; i++) this.u16[(addr >>> 1) + i] = s.charCodeAt(i);
    this.u16[(addr >>> 1) + s.length] = 0;
  }
}
