// A minimal Win32 environment for Milestone 1: process setup (image, stack,
// TEB/PEB, TLS) and JavaScript implementations of the kernel32 and msvcrt
// functions small test programs import. From Milestone 2 on, translated Wine
// DLLs replace these shims; they remain as a test harness.

import { mapImage } from './pe.mjs';
import { ProcessExit, hex } from './runtime.mjs';

const EAX = 0, EDX = 2, ESP = 4;
const MEM_COMMIT = 0x1000;

/** Reads 32-bit stack arguments starting at `addr`. */
class Args {
  constructor(m, addr) {
    this.m = m;
    this.p = addr;
    this.base = addr;
  }
  u32(i) {
    return this.m.u32[(this.base + i * 4) >>> 2];
  }
  i32(i) {
    return this.m.i32[(this.base + i * 4) >>> 2];
  }
  // Sequential reads for varargs.
  next() {
    const v = this.m.dv.getUint32(this.p, true);
    this.p += 4;
    return v;
  }
  nextI64() {
    const v = this.m.dv.getBigInt64(this.p, true);
    this.p += 8;
    return v;
  }
  nextF64() {
    const v = this.m.dv.getFloat64(this.p, true);
    this.p += 8;
    return v;
  }
}

/** Address-space manager for the guest region (64 KB granularity). */
class VirtualMemory {
  constructor(lo, hi) {
    this.regions = []; // sorted {base, size, protect, tag}
    this.lo = lo;
    this.hi = hi;
  }
  reserve(base, size, tag) {
    this.regions.push({ base, size, tag });
    this.regions.sort((a, b) => a.base - b.base);
    return base;
  }
  alloc(size, tag, align = 0x10000) {
    size = (size + 0xfff) & ~0xfff;
    let at = this.lo;
    for (const r of this.regions) {
      if (at + size <= r.base) break;
      at = Math.max(at, Math.ceil((r.base + r.size) / align) * align);
    }
    if (at + size > this.hi) return 0;
    return this.reserve(at, size, tag);
  }
  find(addr) {
    return this.regions.find((r) => addr >= r.base && addr < r.base + r.size);
  }
  free(base) {
    const i = this.regions.findIndex((r) => r.base === base);
    if (i >= 0) this.regions.splice(i, 1);
    return i >= 0;
  }
}

/** A simple heap in a guest arena: size-class free lists over a bump pointer. */
class Heap {
  constructor(proc, initial = 16 << 20) {
    this.proc = proc;
    this.sizes = new Map();
    this.free = new Map();
    this.arenas = [];
    this.grow(initial);
  }
  grow(n) {
    const base = this.proc.vm.alloc(n, 'heap');
    if (!base) throw new Error('out of guest memory');
    this.next = base;
    this.end = base + n;
  }
  alloc(n) {
    const size = Math.max(16, (n + 15) & ~15);
    const list = this.free.get(size);
    let p;
    if (list && list.length) {
      p = list.pop();
    } else {
      if (this.next + size > this.end) this.grow(Math.max(16 << 20, size + 0x10000));
      p = this.next;
      this.next += size;
    }
    this.sizes.set(p, size);
    return p;
  }
  release(p) {
    const size = this.sizes.get(p);
    if (size === undefined) return false;
    this.sizes.delete(p);
    if (!this.free.has(size)) this.free.set(size, []);
    this.free.get(size).push(p);
    return true;
  }
  size(p) {
    return this.sizes.get(p);
  }
}

export class Process {
  /**
   * @param {import('./runtime.mjs').Machine} machine
   * @param {object} opts
   * @param {string[]} [opts.argv]
   * @param {(bytes: Uint8Array) => void} [opts.stdout]
   * @param {(bytes: Uint8Array) => void} [opts.stderr]
   * @param {Map<string, Uint8Array>} [opts.files] in-memory file system
   */
  constructor(machine, opts = {}) {
    this.m = machine;
    this.argv = opts.argv ?? ['program.exe'];
    this.stdout = opts.stdout ?? (() => {});
    this.stderr = opts.stderr ?? (() => {});
    this.files = opts.files ?? new Map();
    this.trace = opts.trace ?? false;
    this.vm = new VirtualMemory(0x10000, machine.thunkBase - 0x40000);
    this.handles = new Map([
      [0xfffffff6, { kind: 'stdin' }],
      [0xfffffff5, { kind: 'stdout' }],
      [0xfffffff4, { kind: 'stderr' }],
    ]);
    this.nextHandle = 0x100;
    this.atexit = [];
    this.tlsSlots = 0;
    this.lastErrorFallback = 0;
    this.unimplemented = new Set();
  }

  /** Loads the executable described by `meta.image` and prepares the main thread. */
  load(exeBytes, image) {
    const m = this.m;
    this.image = image;
    mapImage(m, exeBytes, image);
    this.vm.reserve(image.image_base, image.size_of_image, 'image');
    this.heap = new Heap(this);
    this.processHeap = 0x00ff0000;

    // TEB, PEB and process parameters live just below the thunk page.
    this.peb = m.thunkBase - 0x30000;
    this.teb = m.thunkBase - 0x20000;
    this.vm.reserve(this.peb, 0x30000, 'teb/peb');
    m.u8.fill(0, this.peb, this.peb + 0x30000);
    const stackSize = Math.max(image.stack_reserve || 0x100000, 0x100000);
    this.stackBase = this.vm.alloc(stackSize, 'stack');
    this.stackTop = this.stackBase + stackSize;

    const teb = this.teb;
    const w32 = (a, v) => (m.u32[a >>> 2] = v >>> 0);
    w32(teb + 0x00, 0xffffffff); // no SEH frames yet
    w32(teb + 0x04, this.stackTop);
    w32(teb + 0x08, this.stackBase);
    w32(teb + 0x18, teb);
    w32(teb + 0x20, 0x10); // process id
    w32(teb + 0x24, 0x14); // thread id
    w32(teb + 0x30, this.peb);
    w32(this.peb + 0x08, image.image_base);
    w32(this.peb + 0x18, this.processHeap);

    this.cpu = m.newCpu();
    m.u32[(this.cpu + m.abi.cpu.FS_BASE) >>> 2] = teb;
    // Flat selectors as Windows uses them.
    const sel = this.cpu + m.abi.cpu.SEG_SEL;
    [0x23, 0x1b, 0x23, 0x23, 0x3b, 0x00].forEach((v, i) => m.dv.setUint16(sel + i * 2, v, true));
    m.setReg(this.cpu, ESP, this.stackTop - 16);

    this.setupTls(image);
    this.resolveImports(image);
  }

  setupTls(image) {
    const m = this.m;
    const tls = image.tls;
    const array = this.heap.alloc(64 * 4);
    m.u32[(this.teb + 0x2c) >>> 2] = array;
    if (!tls) return;
    const size = tls.raw_data_end - tls.raw_data_start + tls.zero_fill;
    const block = this.heap.alloc(Math.max(size, 4));
    m.u8.fill(0, block, block + size);
    m.u8.copyWithin(block, tls.raw_data_start, tls.raw_data_end);
    m.u32[array >>> 2] = block;
    if (tls.index_address) m.u32[tls.index_address >>> 2] = 0;
    this.tlsSlots = 0;
  }

  // ---- Imports -------------------------------------------------------------

  resolveImports(image) {
    this.thunks = new Map(); // "dll!name" -> address
    for (const imp of image.imports) {
      const dll = imp.dll.toLowerCase();
      const name = imp.name.Name ? imp.name.Name.name : `#${imp.name.Ordinal}`;
      const addr = this.importAddress(dll, name);
      this.m.u32[(image.image_base + imp.iat_rva) >>> 2] = addr;
    }
  }

  importAddress(dll, name) {
    const key = `${dll}!${name}`;
    if (this.thunks.has(key)) return this.thunks.get(key);
    const lib = API[dll] ?? API[dll.replace(/\.dll$/, '') + '.dll'];
    const def = lib?.[name];
    let addr;
    if (def && def.data) {
      addr = def.data.call(this);
    } else {
      const stdcall = lib?.__cdecl !== true;
      addr = this.m.addThunk((cpu) => this.invoke(cpu, dll, name, def, stdcall));
    }
    this.thunks.set(key, addr);
    return addr;
  }

  invoke(cpu, dll, name, def, stdcall) {
    const m = this.m;
    const esp = m.reg(cpu, ESP);
    const ret = m.u32[esp >>> 2];
    if (!def) {
      throw new Error(`unimplemented API ${dll}!${name} (called from ${hex(ret)})`);
    }
    if (this.trace) this.stderr(new TextEncoder().encode(`[api] ${dll}!${name}\n`));
    const args = new Args(m, esp + 4);
    const r = def.fn.call(this, args, cpu);
    if (r && typeof r === 'object' && r.jump !== undefined) {
      // The handler set the registers itself (longjmp).
      return r.jump;
    }
    if (typeof r === 'bigint') {
      m.setReg(cpu, EAX, Number(r & 0xffffffffn));
      m.setReg(cpu, EDX, Number((r >> 32n) & 0xffffffffn));
    } else if (r !== undefined) {
      m.setReg(cpu, EAX, r >>> 0);
    }
    // The handler may have called back into guest code; esp is restored.
    const pop = stdcall ? (def.argc ?? 0) * 4 : 0;
    m.setReg(cpu, ESP, esp + 4 + pop);
    return ret;
  }

  // ---- Running ---------------------------------------------------------------

  /** Runs TLS callbacks and the entry point; returns the exit code. */
  start() {
    const m = this.m;
    try {
      for (const cb of this.image.tls?.callbacks ?? []) {
        m.callGuest(this.cpu, cb, [this.image.image_base, 1, 0]);
      }
      const code = m.callGuest(this.cpu, this.image.entry, [this.peb]);
      return this.exit(code);
    } catch (e) {
      if (e instanceof ProcessExit) return e.exitCode;
      throw e;
    }
  }

  exit(code) {
    while (this.atexit.length) {
      const f = this.atexit.pop();
      this.m.callGuest(this.cpu, f, []);
    }
    throw new ProcessExit(code >>> 0);
  }

  setLastError(v) {
    this.m.u32[(this.teb + 0x34) >>> 2] = v >>> 0;
  }

  write(handle, bytes) {
    const h = this.handles.get(handle >>> 0);
    if (!h) return false;
    if (h.kind === 'stdout') this.stdout(bytes);
    else if (h.kind === 'stderr') this.stderr(bytes);
    else if (h.kind === 'file') {
      const f = h.file;
      const need = h.pos + bytes.length;
      if (need > f.data.length) {
        const n = new Uint8Array(Math.max(need, f.data.length * 2));
        n.set(f.data);
        f.data = n;
      }
      f.data.set(bytes, h.pos);
      h.pos = need;
      f.size = Math.max(f.size, need);
    } else return false;
    return true;
  }

  allocString(s) {
    const p = this.heap.alloc(s.length + 1);
    this.m.writeCString(p, s);
    return p;
  }

  allocWString(s) {
    const p = this.heap.alloc(s.length * 2 + 2);
    this.m.writeWString(p, s);
    return p;
  }

  commandLine() {
    return this.argv.map((a) => (/[\s"]/.test(a) ? `"${a}"` : a)).join(' ');
  }
}

// ---- printf ------------------------------------------------------------------

/** msvcrt-compatible printf formatting. `next` reads varargs. */
export function formatPrintf(m, fmt, args, wide = false) {
  let out = '';
  let i = 0;
  const readStr = (p) => (p === 0 ? '(null)' : m.readCString(p));
  const readWStr = (p) => (p === 0 ? '(null)' : m.readWString(p));
  while (i < fmt.length) {
    const c = fmt[i++];
    if (c !== '%') {
      out += c;
      continue;
    }
    let flags = '';
    while ('-+ #0'.includes(fmt[i]) && i < fmt.length) flags += fmt[i++];
    let width = '';
    if (fmt[i] === '*') {
      width = String(args.next() | 0);
      i++;
    } else while (/[0-9]/.test(fmt[i])) width += fmt[i++];
    let prec = null;
    if (fmt[i] === '.') {
      i++;
      prec = '';
      if (fmt[i] === '*') {
        prec = String(args.next() | 0);
        i++;
      } else while (/[0-9]/.test(fmt[i])) prec += fmt[i++];
      prec = prec === '' ? 0 : parseInt(prec, 10);
    }
    let len = '';
    for (;;) {
      if (fmt.startsWith('I64', i)) {
        len = 'll';
        i += 3;
      } else if (fmt.startsWith('I32', i)) {
        i += 3;
      } else if ('hlLwjzt'.includes(fmt[i]) && i < fmt.length) {
        len += fmt[i++];
      } else break;
    }
    const conv = fmt[i++];
    let w = parseInt(width || '0', 10);
    let left = flags.includes('-');
    if (w < 0) {
      left = true;
      w = -w;
    }
    const pad = (s, numeric) => {
      if (s.length >= w) return s;
      if (left) return s + ' '.repeat(w - s.length);
      if (numeric && flags.includes('0') && prec === null) {
        const sign = /^[+\- ]/.test(s) ? s[0] : '';
        const pfx = /^[+\- ]?0[xX]/.test(s) ? s.slice(0, sign.length + 2) : sign;
        return pfx + '0'.repeat(w - s.length) + s.slice(pfx.length);
      }
      return ' '.repeat(w - s.length) + s;
    };
    const intArg = (signed) => {
      if (len === 'll' || len === 'j') {
        const v = args.nextI64();
        return signed ? v : BigInt.asUintN(64, v);
      }
      let v = args.next();
      if (len === 'h') v = signed ? (v << 16) >> 16 : v & 0xffff;
      else if (len === 'hh') v = signed ? (v << 24) >> 24 : v & 0xff;
      else v = signed ? v | 0 : v >>> 0;
      return BigInt(v);
    };
    const digits = (s) => (prec !== null && s.length < prec ? '0'.repeat(prec - s.length) + s : prec === 0 && s === '0' ? '' : s);
    switch (conv) {
      case 'd':
      case 'i': {
        const v = intArg(true);
        let s = digits((v < 0n ? -v : v).toString());
        s = (v < 0n ? '-' : flags.includes('+') ? '+' : flags.includes(' ') ? ' ' : '') + s;
        out += pad(s, true);
        break;
      }
      case 'u':
      case 'x':
      case 'X':
      case 'o': {
        const v = intArg(false);
        const base = conv === 'u' ? 10 : conv === 'o' ? 8 : 16;
        let s = digits(v.toString(base));
        if (conv === 'X') s = s.toUpperCase();
        if (flags.includes('#') && v !== 0n) s = (conv === 'o' ? '0' : conv === 'x' ? '0x' : conv === 'X' ? '0X' : '') + s;
        out += pad(s, true);
        break;
      }
      case 'p': {
        out += pad(args.next().toString(16).toUpperCase().padStart(8, '0'), false);
        break;
      }
      case 'c': {
        out += pad(String.fromCharCode(args.next() & (len === 'l' || len === 'w' || wide ? 0xffff : 0xff)), false);
        break;
      }
      case 's':
      case 'S': {
        const p = args.next();
        const isWide = conv === 'S' ? !wide : wide;
        let s = len === 'l' || len === 'w' ? readWStr(p) : len === 'h' ? readStr(p) : isWide ? readWStr(p) : readStr(p);
        if (prec !== null) s = s.slice(0, prec);
        out += pad(s, false);
        break;
      }
      case 'f':
      case 'F':
      case 'e':
      case 'E':
      case 'g':
      case 'G':
      case 'a':
      case 'A': {
        const v = args.nextF64();
        out += pad(formatFloat(v, conv, prec ?? 6, flags), true);
        break;
      }
      case 'n': {
        const p = args.next();
        m.u32[p >>> 2] = out.length;
        break;
      }
      case '%':
        out += '%';
        break;
      default:
        out += '%' + flags + width + conv;
    }
  }
  return out;
}

function formatFloat(v, conv, prec, flags) {
  const upper = conv === conv.toUpperCase();
  const sign = v < 0 || Object.is(v, -0) ? '-' : flags.includes('+') ? '+' : flags.includes(' ') ? ' ' : '';
  const a = Math.abs(v);
  let s;
  if (!Number.isFinite(a)) {
    s = Number.isNaN(a) ? '1.#QNAN0' : '1.#INF00';
  } else {
    const lc = conv.toLowerCase();
    const exp3 = (str) =>
      str.replace(/e([+-])(\d+)$/, (_, sg, d) => 'e' + sg + d.padStart(3, '0'));
    if (lc === 'f') {
      s = a.toFixed(prec);
      if (flags.includes('#') && prec === 0) s += '.';
    } else if (lc === 'e') {
      s = exp3(a.toExponential(prec));
    } else if (lc === 'g') {
      const p = prec === 0 ? 1 : prec;
      if (a === 0) s = '0';
      else {
        const e = Math.floor(Math.log10(a));
        const ex = Number(a.toExponential(p - 1).split('e')[1]);
        if (ex < -4 || ex >= p) {
          s = a.toExponential(p - 1);
          if (!flags.includes('#')) s = s.replace(/\.?0+e/, 'e');
          s = exp3(s);
        } else {
          s = a.toFixed(Math.max(0, p - 1 - ex));
          if (!flags.includes('#') && s.includes('.')) s = s.replace(/\.?0+$/, '');
        }
        void e;
      }
    } else {
      s = a.toString(16);
    }
  }
  if (upper) s = s.toUpperCase();
  return sign + s;
}

// ---- API tables ---------------------------------------------------------------

function fn(argc, f) {
  return { argc, fn: f };
}

const kernel32 = {
  GetStdHandle: fn(1, function (a) {
    return a.u32(0);
  }),
  WriteFile: fn(5, function (a) {
    const [h, buf, n, written] = [a.u32(0), a.u32(1), a.u32(2), a.u32(3)];
    const ok = this.write(h, this.m.u8.slice(buf, buf + n));
    if (written) this.m.u32[written >>> 2] = ok ? n : 0;
    return ok ? 1 : 0;
  }),
  WriteConsoleA: fn(5, function (a) {
    const [h, buf, n, written] = [a.u32(0), a.u32(1), a.u32(2), a.u32(3)];
    this.write(h, this.m.u8.slice(buf, buf + n));
    if (written) this.m.u32[written >>> 2] = n;
    return 1;
  }),
  ExitProcess: fn(1, function (a) {
    this.exit(a.u32(0));
  }),
  GetLastError: fn(0, function () {
    return this.m.u32[(this.teb + 0x34) >>> 2];
  }),
  SetLastError: fn(1, function (a) {
    this.setLastError(a.u32(0));
  }),
  GetModuleHandleA: fn(1, function (a) {
    return a.u32(0) === 0 ? this.image.image_base : 0;
  }),
  GetModuleHandleW: fn(1, function (a) {
    return a.u32(0) === 0 ? this.image.image_base : 0;
  }),
  GetModuleFileNameA: fn(3, function (a) {
    const s = 'C:\\' + this.argv[0];
    const n = Math.min(s.length, a.u32(2) - 1);
    this.m.writeCString(a.u32(1), s.slice(0, n));
    return n;
  }),
  GetProcAddress: fn(2, function (a) {
    const name = a.u32(1) < 0x10000 ? `#${a.u32(1)}` : this.m.readCString(a.u32(1));
    const dll = this.moduleNames?.get(a.u32(0)) ?? 'kernel32.dll';
    const lib = API[dll];
    if (!lib || !lib[name]) return 0;
    return this.importAddress(dll, name);
  }),
  LoadLibraryA: fn(1, function (a) {
    return this.loadLibrary(this.m.readCString(a.u32(0)));
  }),
  LoadLibraryW: fn(1, function (a) {
    return this.loadLibrary(this.m.readWString(a.u32(0)));
  }),
  FreeLibrary: fn(1, () => 1),
  InitializeCriticalSection: fn(1, () => {}),
  InitializeCriticalSectionAndSpinCount: fn(2, () => 1),
  EnterCriticalSection: fn(1, () => {}),
  LeaveCriticalSection: fn(1, () => {}),
  DeleteCriticalSection: fn(1, () => {}),
  TlsAlloc: fn(0, function () {
    return this.tlsSlots < 64 ? this.tlsSlots++ + 1 : 0xffffffff;
  }),
  TlsGetValue: fn(1, function (a) {
    this.setLastError(0);
    return this.m.u32[(this.teb + 0xe10 + a.u32(0) * 4) >>> 2];
  }),
  TlsSetValue: fn(2, function (a) {
    this.m.u32[(this.teb + 0xe10 + a.u32(0) * 4) >>> 2] = a.u32(1);
    return 1;
  }),
  TlsFree: fn(1, () => 1),
  Sleep: fn(1, () => {}),
  SetUnhandledExceptionFilter: fn(1, () => 0),
  UnhandledExceptionFilter: fn(1, () => 0),
  GetCurrentProcess: fn(0, () => 0xffffffff),
  GetCurrentThread: fn(0, () => 0xfffffffe),
  GetCurrentProcessId: fn(0, () => 0x10),
  GetCurrentThreadId: fn(0, () => 0x14),
  TerminateProcess: fn(2, function (a) {
    this.exit(a.u32(1));
  }),
  GetTickCount: fn(0, () => (Date.now() & 0xffffffff) >>> 0),
  QueryPerformanceCounter: fn(1, function (a) {
    const t = BigInt(Math.floor(performance.now() * 1000));
    this.m.dv.setBigUint64(a.u32(0), t, true);
    return 1;
  }),
  QueryPerformanceFrequency: fn(1, function (a) {
    this.m.dv.setBigUint64(a.u32(0), 1000000n, true);
    return 1;
  }),
  GetSystemTimeAsFileTime: fn(1, function (a) {
    const t = BigInt(Date.now()) * 10000n + 116444736000000000n;
    this.m.dv.setBigUint64(a.u32(0), t, true);
  }),
  GetCommandLineA: fn(0, function () {
    return (this.cmdA ??= this.allocString(this.commandLine()));
  }),
  GetCommandLineW: fn(0, function () {
    return (this.cmdW ??= this.allocWString(this.commandLine()));
  }),
  GetStartupInfoA: fn(1, function (a) {
    this.m.u8.fill(0, a.u32(0), a.u32(0) + 68);
    this.m.u32[a.u32(0) >>> 2] = 68;
  }),
  GetStartupInfoW: fn(1, function (a) {
    this.m.u8.fill(0, a.u32(0), a.u32(0) + 68);
    this.m.u32[a.u32(0) >>> 2] = 68;
  }),
  VirtualAlloc: fn(4, function (a) {
    const [addr, size] = [a.u32(0), a.u32(1)];
    if (addr) {
      const r = this.vm.find(addr);
      if (r) return addr; // committing inside a reservation
      return this.vm.reserve(addr & ~0xffff, size, 'virtual') ? addr : 0;
    }
    const p = this.vm.alloc(size, 'virtual');
    if (p) this.m.u8.fill(0, p, p + size);
    return p;
  }),
  VirtualFree: fn(3, function (a) {
    return this.vm.free(a.u32(0)) ? 1 : 1;
  }),
  VirtualProtect: fn(4, function (a) {
    if (a.u32(3)) this.m.u32[a.u32(3) >>> 2] = 0x40;
    return 1;
  }),
  VirtualQuery: fn(3, function (a) {
    const [addr, buf, len] = [a.u32(0), a.u32(1), a.u32(2)];
    const r = this.vm.find(addr);
    const w = (o, v) => (this.m.u32[(buf + o) >>> 2] = v >>> 0);
    this.m.u8.fill(0, buf, buf + Math.min(len, 28));
    const page = addr & ~0xfff;
    w(0, page);
    w(4, r ? r.base : page);
    w(8, 0x40);
    w(12, r ? r.base + r.size - page : 0x1000);
    w(16, r ? MEM_COMMIT : 0x10000);
    w(20, r ? 0x40 : 1);
    w(24, r ? (r.tag === 'image' ? 0x1000000 : 0x20000) : 0);
    return 28;
  }),
  GetProcessHeap: fn(0, function () {
    return this.processHeap;
  }),
  HeapCreate: fn(3, function () {
    return this.processHeap;
  }),
  HeapDestroy: fn(1, () => 1),
  HeapAlloc: fn(3, function (a) {
    const p = this.heap.alloc(a.u32(2));
    if (a.u32(1) & 8) this.m.u8.fill(0, p, p + a.u32(2));
    return p;
  }),
  HeapFree: fn(3, function (a) {
    this.heap.release(a.u32(2));
    return 1;
  }),
  HeapReAlloc: fn(4, function (a) {
    return realloc(this, a.u32(2), a.u32(3));
  }),
  HeapSize: fn(3, function (a) {
    return this.heap.size(a.u32(2)) ?? 0xffffffff;
  }),
  MultiByteToWideChar: fn(6, function (a) {
    const [src, n, dst, cap] = [a.u32(2), a.i32(3), a.u32(4), a.u32(5)];
    const s = n < 0 ? this.m.readCString(src) + '\0' : this.m.readCString(src, n).padEnd(n, '\0');
    if (!cap) return s.length;
    for (let i = 0; i < Math.min(cap, s.length); i++) this.m.dv.setUint16(dst + i * 2, s.charCodeAt(i), true);
    return Math.min(cap, s.length);
  }),
  WideCharToMultiByte: fn(8, function (a) {
    const [src, n, dst, cap] = [a.u32(2), a.i32(3), a.u32(4), a.u32(5)];
    let s = n < 0 ? this.m.readWString(src) + '\0' : '';
    if (n >= 0) for (let i = 0; i < n; i++) s += String.fromCharCode(this.m.dv.getUint16(src + i * 2, true));
    if (!cap) return s.length;
    for (let i = 0; i < Math.min(cap, s.length); i++) this.m.u8[dst + i] = s.charCodeAt(i) & 0xff;
    return Math.min(cap, s.length);
  }),
  IsDBCSLeadByteEx: fn(2, () => 0),
  IsDBCSLeadByte: fn(1, () => 0),
  GetACP: fn(0, () => 1252),
  GetOEMCP: fn(0, () => 437),
  GetCPInfo: fn(2, function (a) {
    const p = a.u32(1);
    this.m.u8.fill(0, p, p + 20);
    this.m.u32[p >>> 2] = 1;
    this.m.u8[p + 4] = 0x3f;
    return 1;
  }),
  GetEnvironmentVariableA: fn(3, () => 0),
  GetEnvironmentStringsA: fn(0, function () {
    return (this.envA ??= this.allocString('\0'));
  }),
  GetEnvironmentStringsW: fn(0, function () {
    return (this.envW ??= this.allocWString('\0'));
  }),
  FreeEnvironmentStringsA: fn(1, () => 1),
  FreeEnvironmentStringsW: fn(1, () => 1),
  GetFileType: fn(1, (a) => (a.u32(0) >= 0xfffffff4 ? 2 : 1)),
  SetHandleCount: fn(1, (a) => a.u32(0)),
  GetVersion: fn(0, () => 0x0a280105),
  IsDebuggerPresent: fn(0, () => 0),
  IsProcessorFeaturePresent: fn(1, (a) => ([6, 7, 10].includes(a.u32(0)) ? 1 : 0)),
  RaiseException: fn(4, function (a) {
    throw new Error(`RaiseException ${hex(a.u32(0))}`);
  }),
  CloseHandle: fn(1, function (a) {
    return this.handles.delete(a.u32(0)) ? 1 : 1;
  }),
  CreateFileA: fn(7, function (a) {
    return openFile(this, this.m.readCString(a.u32(0)), a.u32(1), a.u32(4));
  }),
  ReadFile: fn(5, function (a) {
    const [h, buf, n, read] = [a.u32(0), a.u32(1), a.u32(2), a.u32(3)];
    const f = this.handles.get(h);
    if (!f || f.kind !== 'file') return 0;
    const k = Math.max(0, Math.min(n, f.file.size - f.pos));
    this.m.u8.set(f.file.data.subarray(f.pos, f.pos + k), buf);
    f.pos += k;
    if (read) this.m.u32[read >>> 2] = k;
    return 1;
  }),
  SetFilePointer: fn(4, function (a) {
    const f = this.handles.get(a.u32(0));
    if (!f || f.kind !== 'file') return 0xffffffff;
    const off = a.i32(1);
    const how = a.u32(3);
    f.pos = Math.max(0, (how === 0 ? 0 : how === 1 ? f.pos : f.file.size) + off);
    return f.pos;
  }),
  GetFileSize: fn(2, function (a) {
    const f = this.handles.get(a.u32(0));
    if (a.u32(1)) this.m.u32[a.u32(1) >>> 2] = 0;
    return f?.file ? f.file.size : 0xffffffff;
  }),
  FlushFileBuffers: fn(1, () => 1),
  SetConsoleCtrlHandler: fn(2, () => 1),
  GetConsoleMode: fn(2, () => 0),
};

function openFile(proc, path, access, disposition) {
  const key = path.replace(/^[a-zA-Z]:/, '').replace(/\\/g, '/').replace(/^\//, '').toLowerCase();
  let file = proc.files.get(key);
  const write = (access & 0x40000000) !== 0;
  if (!file) {
    if (!write || disposition === 3 /* OPEN_EXISTING */) {
      proc.setLastError(2);
      return 0xffffffff;
    }
    file = { data: new Uint8Array(0), size: 0 };
    proc.files.set(key, file);
  } else if (!(file.data instanceof Uint8Array) || file.size === undefined) {
    file = { data: file, size: file.length };
    proc.files.set(key, file);
  }
  if (disposition === 2 /* CREATE_ALWAYS */ || disposition === 5 /* TRUNCATE_EXISTING */) file.size = 0;
  const h = proc.nextHandle;
  proc.nextHandle += 4;
  proc.handles.set(h, { kind: 'file', file, pos: 0 });
  return h;
}

function realloc(proc, p, n) {
  if (!p) return proc.heap.alloc(n);
  const old = proc.heap.size(p) ?? 0;
  if (n <= old) return p;
  const q = proc.heap.alloc(n);
  proc.m.u8.copyWithin(q, p, p + old);
  proc.heap.release(p);
  return q;
}

Process.prototype.loadLibrary = function (name) {
  const n = name.toLowerCase().replace(/^.*[\\/]/, '');
  const dll = n.endsWith('.dll') ? n : n + '.dll';
  if (!API[dll]) return 0;
  this.moduleNames ??= new Map();
  const h = 0x70000000 + this.moduleNames.size * 0x10000;
  for (const [k, v] of this.moduleNames) if (v === dll) return k;
  this.moduleNames.set(h, dll);
  return h;
};

function saveJmp(proc, a, cpu) {
  const m = proc.m;
  const buf = a.u32(0);
  const esp = m.reg(cpu, ESP);
  const vals = [m.reg(cpu, 5), m.reg(cpu, 3), m.reg(cpu, 7), m.reg(cpu, 6), esp + 4, m.u32[esp >>> 2], m.u32[proc.teb >>> 2], 0xffffffff];
  vals.forEach((v, i) => (m.u32[(buf >>> 2) + i] = v >>> 0));
  return 0;
}

// msvcrt: FILE structures are 32 bytes; stdin/stdout/stderr are _iob[0..2].
const FILE_SIZE = 32;

function fileHandle(proc, fp) {
  const i = (fp - proc.iob) / FILE_SIZE;
  if (i >= 0 && i < 3) return 0xfffffff6 - i;
  return proc.fileHandles?.get(fp) ?? 0;
}

function cwrite(proc, fp, bytes) {
  return proc.write(fileHandle(proc, fp), bytes);
}

const enc = (s) => Uint8Array.from(s, (c) => c.charCodeAt(0) & 0xff);

const msvcrt = {
  __cdecl: true,
  _iob: {
    data() {
      if (!this.iob) {
        this.iob = this.heap.alloc(FILE_SIZE * 20);
        this.m.u8.fill(0, this.iob, this.iob + FILE_SIZE * 20);
        for (let i = 0; i < 3; i++) this.m.u32[(this.iob + i * FILE_SIZE + 16) >>> 2] = i;
      }
      return this.iob;
    },
  },
  __initenv: {
    data() {
      return (this.initenv ??= this.heap.alloc(4));
    },
  },
  _environ: {
    data() {
      return (this.initenv ??= this.heap.alloc(4));
    },
  },
  __mb_cur_max: {
    data() {
      const p = this.heap.alloc(4);
      this.m.u32[p >>> 2] = 1;
      return p;
    },
  },
  _acmdln: {
    data() {
      const p = this.heap.alloc(4);
      this.m.u32[p >>> 2] = this.allocString(this.commandLine());
      return p;
    },
  },
  __getmainargs: fn(5, function (a) {
    const argv = this.heap.alloc((this.argv.length + 1) * 4);
    this.argv.forEach((s, i) => (this.m.u32[(argv >>> 2) + i] = this.allocString(s)));
    this.m.u32[(argv >>> 2) + this.argv.length] = 0;
    const env = this.heap.alloc(4);
    this.m.u32[env >>> 2] = 0;
    this.m.u32[a.u32(0) >>> 2] = this.argv.length;
    this.m.u32[a.u32(1) >>> 2] = argv;
    this.m.u32[a.u32(2) >>> 2] = env;
    if (this.initenv) this.m.u32[this.initenv >>> 2] = env;
    return 0;
  }),
  __p__fmode: fn(0, function () {
    return (this.fmode ??= this.heap.alloc(4));
  }),
  __p__commode: fn(0, function () {
    return (this.commode ??= this.heap.alloc(4));
  }),
  __p___argc: fn(0, function () {
    const p = (this.pargc ??= this.heap.alloc(4));
    this.m.u32[p >>> 2] = this.argv.length;
    return p;
  }),
  __set_app_type: fn(1, () => {}),
  __setusermatherr: fn(1, () => {}),
  _controlfp: fn(2, () => 0x9001f),
  _amsg_exit: fn(1, function (a) {
    throw new Error(`_amsg_exit(${a.u32(0)})`);
  }),
  _initterm: fn(2, function (a) {
    for (let p = a.u32(0); p < a.u32(1); p += 4) {
      const f = this.m.u32[p >>> 2];
      if (f) this.m.callGuest(this.cpu, f, []);
    }
  }),
  _onexit: fn(1, function (a) {
    this.atexit.push(a.u32(0));
    return a.u32(0);
  }),
  atexit: fn(1, function (a) {
    this.atexit.push(a.u32(0));
    return 0;
  }),
  // jmp_buf layout (msvcrt _JUMP_BUFFER): Ebp Ebx Edi Esi Esp Eip
  // Registration TryLevel ...
  _setjmp3: fn(2, function (a, cpu) {
    return saveJmp(this, a, cpu);
  }),
  _setjmp: fn(1, function (a, cpu) {
    return saveJmp(this, a, cpu);
  }),
  longjmp: fn(2, function (a, cpu) {
    const m = this.m;
    const buf = a.u32(0);
    const at = (i) => m.u32[(buf >>> 2) + i];
    m.setReg(cpu, 5, at(0));
    m.setReg(cpu, 3, at(1));
    m.setReg(cpu, 7, at(2));
    m.setReg(cpu, 6, at(3));
    m.setReg(cpu, ESP, at(4));
    m.u32[this.teb >>> 2] = at(6);
    m.setReg(cpu, EAX, a.u32(1) || 1);
    return { jump: at(5) };
  }),
  qsort: fn(4, function (a) {
    const [base, n, size, cmp] = [a.u32(0), a.u32(1), a.u32(2), a.u32(3)];
    const m = this.m;
    const items = [];
    for (let i = 0; i < n; i++) items.push(m.u8.slice(base + i * size, base + (i + 1) * size));
    // Compare through two scratch slots in guest memory.
    const tmp = this.heap.alloc(size * 2);
    items.sort((x, y) => {
      m.u8.set(x, tmp);
      m.u8.set(y, tmp + size);
      return m.callGuest(this.cpu, cmp, [tmp, tmp + size]) | 0;
    });
    this.heap.release(tmp);
    items.forEach((it, i) => m.u8.set(it, base + i * size));
  }),
  _cexit: fn(0, () => {}),
  _c_exit: fn(0, () => {}),
  exit: fn(1, function (a) {
    this.exit(a.u32(0));
  }),
  _exit: fn(1, function (a) {
    throw new ProcessExit(a.u32(0));
  }),
  abort: fn(0, function () {
    this.stderr(enc('\nabnormal program termination\n'));
    throw new ProcessExit(3);
  }),
  signal: fn(2, () => 0),
  raise: fn(1, () => 0),
  _lock: fn(1, () => {}),
  _unlock: fn(1, () => {}),
  _errno: fn(0, function () {
    return (this.errnoPtr ??= this.heap.alloc(4));
  }),
  setlocale: fn(2, function () {
    return (this.localeC ??= this.allocString('C'));
  }),
  localeconv: fn(0, function () {
    if (!this.lconv) {
      this.lconv = this.heap.alloc(64);
      this.m.u8.fill(0, this.lconv, this.lconv + 64);
      const dot = this.allocString('.');
      const empty = this.allocString('');
      for (let i = 0; i < 10; i++) this.m.u32[(this.lconv >>> 2) + i] = i === 0 ? dot : empty;
    }
    return this.lconv;
  }),
  strerror: fn(1, function (a) {
    return this.allocString(`error ${a.u32(0)}`);
  }),
  malloc: fn(1, function (a) {
    return this.heap.alloc(a.u32(0));
  }),
  calloc: fn(2, function (a) {
    const n = a.u32(0) * a.u32(1);
    const p = this.heap.alloc(n);
    this.m.u8.fill(0, p, p + n);
    return p;
  }),
  realloc: fn(2, function (a) {
    return realloc(this, a.u32(0), a.u32(1));
  }),
  free: fn(1, function (a) {
    if (a.u32(0)) this.heap.release(a.u32(0));
  }),
  memcpy: fn(3, function (a) {
    this.m.u8.copyWithin(a.u32(0), a.u32(1), a.u32(1) + a.u32(2));
    return a.u32(0);
  }),
  memmove: fn(3, function (a) {
    this.m.u8.copyWithin(a.u32(0), a.u32(1), a.u32(1) + a.u32(2));
    return a.u32(0);
  }),
  memset: fn(3, function (a) {
    this.m.u8.fill(a.u32(1) & 0xff, a.u32(0), a.u32(0) + a.u32(2));
    return a.u32(0);
  }),
  memcmp: fn(3, function (a) {
    const u8 = this.m.u8;
    for (let i = 0; i < a.u32(2); i++) {
      const d = u8[a.u32(0) + i] - u8[a.u32(1) + i];
      if (d) return d;
    }
    return 0;
  }),
  strlen: fn(1, function (a) {
    let p = a.u32(0);
    while (this.m.u8[p]) p++;
    return p - a.u32(0);
  }),
  wcslen: fn(1, function (a) {
    let p = a.u32(0);
    while (this.m.dv.getUint16(p, true)) p += 2;
    return (p - a.u32(0)) / 2;
  }),
  strcpy: fn(2, function (a) {
    let i = 0;
    const u8 = this.m.u8;
    do u8[a.u32(0) + i] = u8[a.u32(1) + i];
    while (u8[a.u32(1) + i++]);
    return a.u32(0);
  }),
  strcmp: fn(2, function (a) {
    const u8 = this.m.u8;
    for (let i = 0; ; i++) {
      const x = u8[a.u32(0) + i];
      const y = u8[a.u32(1) + i];
      if (x !== y || !x) return x - y;
    }
  }),
  strncmp: fn(3, function (a) {
    const u8 = this.m.u8;
    for (let i = 0; i < a.u32(2); i++) {
      const x = u8[a.u32(0) + i];
      const y = u8[a.u32(1) + i];
      if (x !== y || !x) return x - y;
    }
    return 0;
  }),
  strchr: fn(2, function (a) {
    const u8 = this.m.u8;
    const c = a.u32(1) & 0xff;
    for (let p = a.u32(0); ; p++) {
      if (u8[p] === c) return p;
      if (!u8[p]) return 0;
    }
  }),
  atoi: fn(1, function (a) {
    return parseInt(this.m.readCString(a.u32(0)), 10) | 0;
  }),
  printf: fn(1, function (a) {
    const s = formatPrintf(this.m, this.m.readCString(a.u32(0)), new Args(this.m, a.base + 4));
    this.stdout(enc(s));
    return s.length;
  }),
  vprintf: fn(2, function (a) {
    const s = formatPrintf(this.m, this.m.readCString(a.u32(0)), new Args(this.m, a.u32(1)));
    this.stdout(enc(s));
    return s.length;
  }),
  fprintf: fn(2, function (a) {
    const s = formatPrintf(this.m, this.m.readCString(a.u32(1)), new Args(this.m, a.base + 8));
    cwrite(this, a.u32(0), enc(s));
    return s.length;
  }),
  vfprintf: fn(3, function (a) {
    const s = formatPrintf(this.m, this.m.readCString(a.u32(1)), new Args(this.m, a.u32(2)));
    cwrite(this, a.u32(0), enc(s));
    return s.length;
  }),
  sprintf: fn(2, function (a) {
    const s = formatPrintf(this.m, this.m.readCString(a.u32(1)), new Args(this.m, a.base + 8));
    this.m.writeCString(a.u32(0), s);
    return s.length;
  }),
  _snprintf: fn(3, function (a) {
    const s = formatPrintf(this.m, this.m.readCString(a.u32(2)), new Args(this.m, a.base + 12));
    const n = a.u32(1);
    const t = s.slice(0, n);
    for (let i = 0; i < t.length; i++) this.m.u8[a.u32(0) + i] = t.charCodeAt(i);
    if (s.length < n) this.m.u8[a.u32(0) + s.length] = 0;
    return s.length <= n ? s.length : -1;
  }),
  puts: fn(1, function (a) {
    this.stdout(enc(this.m.readCString(a.u32(0)) + '\n'));
    return 0;
  }),
  putchar: fn(1, function (a) {
    this.stdout(Uint8Array.of(a.u32(0) & 0xff));
    return a.u32(0) & 0xff;
  }),
  fputc: fn(2, function (a) {
    cwrite(this, a.u32(1), Uint8Array.of(a.u32(0) & 0xff));
    return a.u32(0) & 0xff;
  }),
  fputs: fn(2, function (a) {
    cwrite(this, a.u32(1), enc(this.m.readCString(a.u32(0))));
    return 0;
  }),
  fwrite: fn(4, function (a) {
    const n = a.u32(1) * a.u32(2);
    cwrite(this, a.u32(3), this.m.u8.slice(a.u32(0), a.u32(0) + n));
    return a.u32(2);
  }),
  fflush: fn(1, () => 0),
  _isatty: fn(1, () => 0),
  _fileno: fn(1, function (a) {
    return (a.u32(0) - this.iob) / FILE_SIZE;
  }),
  _setmode: fn(2, () => 0x4000),
  __iob_func: fn(0, function () {
    return msvcrt._iob.data.call(this);
  }),
};

export const API = {
  'kernel32.dll': kernel32,
  'msvcrt.dll': msvcrt,
};
