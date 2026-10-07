// Milestone 2: the host side of Wine, standing in for Wine's Unix side.
//
// Wine's Windows-side DLLs (ntdll, kernelbase, kernel32, ...) run as
// translated x86 code. Their system calls arrive here through the pointer
// ntdll exports as `__wine_syscall_dispatcher`: `mov eax, id; call
// __wine_syscall` lands in a host thunk with the arguments on the guest
// stack, already valid pointers into the shared memory.
//
// This file sets up the process the way Wine's Unix side does (PEB, TEB,
// process parameters, shared user data, the initial thread context), maps
// image sections together with their translated modules, and implements
// the system calls in ./syscalls.mjs.

import { ProcessExit, hex } from '../runtime.mjs';
import { mapImage } from '../pe.mjs';
import { VirtualMemory, MEM_IMAGE, MEM_PRIVATE, PAGE_READWRITE, PAGE_EXECUTE_READ, PAGE_READONLY } from './vm.mjs';
import { SYSCALLS, STATUS } from './syscalls.mjs';
import { HANDLE_ROUTED, WIN32U_UNIXLIB } from './unix.mjs';

import layout from './layout.json' with { type: 'json' };
import { installAssemblies } from './sxs.mjs';
import { installExceptions } from './exceptions.mjs';
import { Scheduler, Thread } from './threads.mjs';
import { startTicker, writeClock } from './ticker.mjs';
import { THREAD_SYSCALLS } from './thread-syscalls.mjs';
import { AUDIO_UNIXLIB, BrowserAudio } from './audio.mjs';

export const L = layout;

const EAX = 0, ECX = 1, EDX = 2, EBX = 3, ESP = 4, EBP = 5, ESI = 6, EDI = 7;
export const USER_SHARED_DATA = 0x7ffe0000;

/** Reads the parts of a PE header the host needs (image info, exports). */
export function parsePe(bytes) {
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const pe = dv.getUint32(0x3c, true);
  const coff = pe + 4;
  const opt = coff + 20;
  const nsec = dv.getUint16(coff + 2, true);
  const optSize = dv.getUint16(coff + 16, true);
  const info = {
    machine: dv.getUint16(coff, true),
    characteristics: dv.getUint16(coff + 18, true),
    entryRva: dv.getUint32(opt + 16, true),
    imageBase: dv.getUint32(opt + 28, true),
    sizeOfImage: dv.getUint32(opt + 56, true),
    sizeOfHeaders: dv.getUint32(opt + 60, true),
    checksum: dv.getUint32(opt + 64, true),
    subsystem: dv.getUint16(opt + 68, true),
    dllCharacteristics: dv.getUint16(opt + 70, true),
    stackReserve: dv.getUint32(opt + 72, true),
    stackCommit: dv.getUint32(opt + 76, true),
    majorOs: dv.getUint16(opt + 40, true),
    minorOs: dv.getUint16(opt + 42, true),
    majorSubsystem: dv.getUint16(opt + 48, true),
    minorSubsystem: dv.getUint16(opt + 50, true),
    loaderFlags: dv.getUint32(opt + 88, true),
    exportDir: dv.getUint32(opt + 96, true),
    sections: [],
  };
  for (let i = 0; i < nsec; i++) {
    const s = opt + optSize + i * 40;
    info.sections.push({
      virtual_size: dv.getUint32(s + 8, true),
      virtual_address: dv.getUint32(s + 12, true),
      raw_size: dv.getUint32(s + 16, true),
      raw_offset: dv.getUint32(s + 20, true),
      characteristics: dv.getUint32(s + 36, true),
    });
  }
  return info;
}

/**
 * A copy of a PE file rebased to `base`: its base relocations applied and
 * ImageBase updated, so Wine's loader finds it already in place. Returns
 * null when the image cannot move (relocations stripped).
 */
export function rebaseImage(bytes, info, base) {
  const dv0 = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const opt = dv0.getUint32(0x3c, true) + 24;
  if (info.characteristics & 0x0001) return null; // IMAGE_FILE_RELOCS_STRIPPED
  const out = bytes.slice();
  const dv = new DataView(out.buffer);
  const offsetOf = (rva) => {
    if (rva < info.sizeOfHeaders) return rva;
    for (const s of info.sections) {
      if (rva >= s.virtual_address && rva < s.virtual_address + s.raw_size) return s.raw_offset + rva - s.virtual_address;
    }
    return -1;
  };
  const delta = (base - info.imageBase) | 0;
  const relRva = dv.getUint32(opt + 136, true);
  const relSize = dv.getUint32(opt + 140, true);
  for (let p = relRva; p + 8 <= relRva + relSize; ) {
    const page = dv.getUint32(offsetOf(p), true);
    const blockSize = dv.getUint32(offsetOf(p + 4), true);
    if (blockSize < 8) break;
    for (let e = p + 8; e < p + blockSize; e += 2) {
      const entry = dv.getUint16(offsetOf(e), true);
      const type = entry >> 12;
      if (type === 0) continue; // IMAGE_REL_BASED_ABSOLUTE (padding)
      if (type !== 3) throw new Error(`unsupported relocation type ${type}`);
      const at = offsetOf(page + (entry & 0xfff));
      // Fixups in uninitialized data have nothing to patch in the file.
      if (at >= 0) dv.setUint32(at, (dv.getUint32(at, true) + delta) >>> 0, true);
    }
    p += blockSize;
  }
  dv.setUint32(opt + 28, base, true);
  return out;
}

/** Exported names of a mapped image -> addresses. */
export function readExports(m, base) {
  const u32 = (a) => m.dv.getUint32(a, true);
  const pe = base + u32(base + 0x3c);
  const dirRva = u32(pe + 24 + 96);
  const out = new Map();
  if (!dirRva) return out;
  const dir = base + dirRva;
  const nNames = u32(dir + 24);
  const funcs = base + u32(dir + 28);
  const names = base + u32(dir + 32);
  const ords = base + u32(dir + 36);
  for (let i = 0; i < nNames; i++) {
    const name = m.readCString(base + u32(names + i * 4));
    const ord = m.dv.getUint16(ords + i * 2, true);
    out.set(name, base + u32(funcs + ord * 4));
  }
  return out;
}

export class WineHost {
  /**
   * @param {import('../runtime.mjs').Machine} machine  initialized with a 2 GB guest limit
   * @param {object} opts
   * @param {(path: string, bytes: Uint8Array, at: {base: number, rebased: boolean}) => Uint8Array} opts.translate
   *        returns the translated module for an image file (`bytes` are
   *        already rebased when the image could not load at its own base)
   * @param {Map<string, Uint8Array>} opts.files  DOS paths (c:/...) -> contents
   */
  constructor(machine, opts) {
    this.m = machine;
    this.translate = opts.translate;
    this.files = opts.files;
    // The side-by-side store wineboot would have filled.
    installAssemblies(this.files);
    this.stdout = opts.stdout ?? (() => {});
    this.stderr = opts.stderr ?? (() => {});
    this.trace = opts.trace ?? false;
    this.vm = new VirtualMemory(machine, 0x10000, machine.thunkBase);
    this.handles = new Map();
    // The host's own handles (files, sections) are numbered apart from the
    // ones wineserver hands out (4, 8, 12, ...) when Wine's Unix side is
    // loaded (opts.unix), so either side can tell its handles from the other's.
    this.nextHandle = 0x40000;
    /** Wine's Unix side compiled with Emscripten (./unix.mjs), or null */
    this.unix = opts.unix ?? null;
    /** The audio driver (./audio.mjs); opts.audioSink receives what plays. */
    this.audio = new BrowserAudio(this, opts.audioSink ?? null);
    this.unix?.attach(this);
    /** Results of NtCallbackReturn, one per user callback in progress. */
    this.callbackResults = [];
    this.images = new Map(); // base -> {path, info}
    this.modulesByPath = new Map();
    this.unimplemented = new Map();
    this.counts = new Map();
    this.argv = opts.argv;
    this.exePath = opts.exePath; // DOS path, e.g. c:\hello.exe
    this.env = opts.env ?? {};
    /** Wine's debug channels, as WINEDEBUG sets them ("+actctx,warn+heap") */
    this.debug = opts.debug ?? '';
  }

  /**
   * Writes WINEDEBUG's channels as ntdll reads them: entries of a flags byte
   * (bit per class: fixme, err, warn, trace) and a 15-byte name, sorted by
   * name, ended by an empty name whose flags apply to all other channels.
   */
  writeDebugOptions(at, spec) {
    const CLASSES = ['fixme', 'err', 'warn', 'trace'];
    let all = 0b11; // fixme and err
    const channels = new Map();
    for (const item of spec.split(/[,;]/).map((x) => x.trim()).filter(Boolean)) {
      const m = item.match(/^(fixme|err|warn|trace)?([+-])(.+)$/);
      if (!m) continue;
      const bits = m[1] ? 1 << CLASSES.indexOf(m[1]) : 0xf;
      const apply = (flags) => (m[2] === '+' ? flags | bits : flags & ~bits);
      if (m[3] === 'all') all = apply(all);
      else if (m[3].length < 15) channels.set(m[3], apply(channels.get(m[3]) ?? all));
    }
    const names = [...channels.keys()].sort((a, b) => (a < b ? -1 : a > b ? 1 : 0)).slice(0, 255);
    names.forEach((name, i) => {
      this.m.u8[at + i * 16] = channels.get(name);
      for (let j = 0; j < name.length; j++) this.m.u8[at + i * 16 + 1 + j] = name.charCodeAt(j);
    });
    this.m.u8[at + names.length * 16] = all;
  }

  // ---- Guest memory helpers ------------------------------------------------

  u32(a) {
    return this.m.dv.getUint32(a, true);
  }
  w32(a, v) {
    this.m.dv.setUint32(a, v >>> 0, true);
  }
  w16(a, v) {
    this.m.dv.setUint16(a, v & 0xffff, true);
  }
  u16(a) {
    return this.m.dv.getUint16(a, true);
  }
  w64(a, v) {
    this.m.dv.setBigUint64(a, BigInt.asUintN(64, BigInt(v)), true);
  }
  u64(a) {
    return this.m.dv.getBigUint64(a, true);
  }

  /** Reads a UNICODE_STRING. */
  ustr(a) {
    if (!a) return null;
    const len = this.u16(a);
    const buf = this.u32(a + 4);
    let s = '';
    for (let i = 0; i < len / 2; i++) s += String.fromCharCode(this.u16(buf + i * 2));
    return s;
  }

  /** Writes a JS string as UTF-16 (with terminator) and returns bytes written. */
  wstr(a, s) {
    for (let i = 0; i < s.length; i++) this.w16(a + i * 2, s.charCodeAt(i));
    this.w16(a + s.length * 2, 0);
    return s.length * 2 + 2;
  }

  /** Fills a UNICODE_STRING at `a` pointing to a string written at `buf`. */
  putUstr(a, buf, s) {
    this.wstr(buf, s);
    this.w16(a, s.length * 2);
    this.w16(a + 2, s.length * 2 + 2);
    this.w32(a + 4, buf);
    return buf + s.length * 2 + 2;
  }

  /** Allocates committed memory for host-built data. */
  alloc(size, prot = PAGE_READWRITE, name = '') {
    const base = this.vm.reserve(0, size, { prot, name });
    if (!base) throw new Error('out of guest address space');
    this.vm.commit(base, size, prot);
    return base;
  }

  // ---- Handles ---------------------------------------------------------------

  newHandle(obj) {
    const h = this.nextHandle;
    this.nextHandle += 4;
    this.handles.set(h, obj);
    return h;
  }

  object(h) {
    if (h === 0xffffffff) return { type: 'process', self: true };
    if (h === 0xfffffffe) return { type: 'thread', self: true };
    return this.handles.get(h >>> 0);
  }

  // ---- Paths and files -------------------------------------------------------

  /** Normalizes an NT path (\??\C:\x, \Device\..., relative to a directory). */
  ntToDos(name, rootHandle) {
    let p = name.replace(/\//g, '\\');
    if (rootHandle) {
      const root = this.object(rootHandle);
      if (root?.path) p = root.path.replace(/\\$/, '') + '\\' + p;
    }
    p = p.replace(/^\\\?\?\\/, '').replace(/^\\DosDevices\\/i, '').replace(/^\\GLOBAL\?\?\\/i, '');
    if (/^unc\\/i.test(p)) return null;
    return p.toLowerCase().replace(/\\+$/, '');
  }

  fileAt(dosPath) {
    return this.files.get(dosPath) ?? null;
  }

  isDir(dosPath) {
    if (/^[a-z]:$/.test(dosPath)) return true;
    const prefix = dosPath + '\\';
    for (const k of this.files.keys()) if (k.startsWith(prefix)) return true;
    return false;
  }

  // ---- Images ----------------------------------------------------------------

  /** Maps an image file at its preferred base and loads its translation. */
  mapImageFile(dosPath, bytes) {
    let info = parsePe(bytes);
    let base = info.imageBase;
    const reserve = (at) => this.vm.reserve(at, info.sizeOfImage, { type: MEM_IMAGE, prot: 0x80, name: dosPath });
    let rebased = false;
    if (!reserve(base)) {
      // The preferred range is taken (many DLLs share MinGW's default base):
      // map elsewhere and translate the rebased image, as Windows would
      // relocate it.
      const at = this.vm.findFree(info.sizeOfImage, { topDown: true });
      const moved = at && rebaseImage(bytes, info, at);
      if (!moved || !reserve(at)) return { status: STATUS.CONFLICTING_ADDRESSES };
      this.log(`rebased ${dosPath} from ${hex(base)} to ${hex(at)}`);
      bytes = moved;
      info = parsePe(bytes);
      base = at;
      rebased = true;
    }
    this.vm.commit(base, info.sizeOfImage, PAGE_EXECUTE_READ);
    // The translation: module bytes, or an already compiled module (Wine's
    // DLLs are translated ahead of time, at their preferred base, and
    // compiled by the host).
    const wasm = this.translate(dosPath, bytes, { base, rebased });
    const name = dosPath.split('\\').pop();
    const rec = wasm instanceof WebAssembly.Module ? this.m.loadCompiledSync(wasm, name) : this.m.loadModuleSync(wasm, name);
    mapImage(this.m, bytes, rec.meta.image);
    this.images.set(base, { path: dosPath, info, size: info.sizeOfImage });
    this.log(`mapped ${dosPath} at ${hex(base)} (${rec.count} functions)`);
    return { status: STATUS.SUCCESS, base, size: info.sizeOfImage, info };
  }

  log(s) {
    if (this.trace) this.stderr(new TextEncoder().encode(`[wine] ${s}\n`));
  }

  // ---- Process setup -----------------------------------------------------------

  boot(ntdllPath, exeDosPath) {
    const m = this.m;
    // Shared user data.
    this.vm.reserve(USER_SHARED_DATA, 0x10000, { prot: PAGE_READONLY, name: 'KUSER_SHARED_DATA' });
    this.vm.commit(USER_SHARED_DATA, 0x1000, PAGE_READONLY);
    this.initSharedData();

    // ntdll and its hooks.
    const ntdll = this.mapImageFile(ntdllPath, this.fileAt(ntdllPath));
    if (ntdll.status) throw new Error(`cannot map ntdll at its base`);
    this.ntdll = ntdll.base;
    this.ntdllExports = readExports(m, ntdll.base);
    this.buildSyscallTable();
    const ex = (n) => {
      const a = this.ntdllExports.get(n);
      if (!a) throw new Error(`ntdll export ${n} missing`);
      return a;
    };
    this.syscallThunk = m.addThunk((cpu) => this.syscall(cpu));
    this.unixCallThunk = m.addThunk((cpu) => this.unixCall(cpu));
    this.w32(ex('__wine_syscall_dispatcher'), this.syscallThunk);
    this.w32(ex('__wine_unix_call_dispatcher'), this.unixCallThunk);
    this.w32(ex('__wine_unixlib_handle'), 0x1000);
    this.w32(ex('__wine_unixlib_handle') + 4, 0);
    this.kiUserCallbackDispatcher = ex('KiUserCallbackDispatcher');
    installExceptions(this, ex);

    // The main executable, mapped by the "Unix side" as Wine does.
    const exe = this.mapImageFile(exeDosPath, this.fileAt(exeDosPath));
    if (exe.status) throw new Error(`cannot map ${exeDosPath} at its base`);
    this.exe = exe;

    // PEB, TEB, process parameters.
    // The page after the PEB holds the debug channels (as Wine's Unix side
    // leaves them; ntdll's __wine_dbg_get_channel_flags reads them there).
    this.peb = this.alloc(0x2000, PAGE_READWRITE, 'PEB');
    this.buildPeb(exe);
    this.writeDebugOptions(this.peb + 0x1000, this.debug);
    this.ex = ex;
    this.stackReserve = exe.info.stackReserve;
    this.threads = new Scheduler(this);
    // The main thread: its TEB, CPU state and initial context. Wine's Unix
    // side registers the process with its first thread.
    const main = this.newThread(exe.base + exe.info.entryRva, this.peb, exe.info.stackReserve);
    this.teb = main.teb;
    this.cpu = main.cpu;
    if (this.unix) {
      this.unix.initProcess(main.teb, this.peb);
      // Waits in Wine's Unix side run the other threads (./threads.mjs).
      this.threads.realWait = this.unix.M.hostWait;
      this.unix.M.hostWait = (ms) => this.threads.waitNested(ms);
    } else {
      this.threads.realWait = (ms) => (ms < 0 ? -1 : (Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms), 0));
    }
    main.tid = this.u32(main.teb + L.TEB.ClientId + 4);
    this.threads.add(main);
    this.threads.switchTo(main);
    this.entry = main.resume;
  }

  /**
   * A thread's TEB, stack, CPU state and initial context, as Wine's Unix
   * side builds them (signal_i386.c): LdrInitializeThunk runs first, then
   * continues at RtlUserThreadStart(start, param).
   */
  newThread(start, param, stackReserve) {
    const m = this.m;
    const teb = this.alloc(0x2000, PAGE_READWRITE, 'TEB');
    this.buildTeb(teb, stackReserve);
    const cpu = m.newCpu();
    m.dv.setUint16(cpu + m.abi.cpu.FPU_CW, 0x27f, true);
    // Windows starts threads with IF set.
    m.u32[(cpu + m.abi.cpu.EFLAGS_SYS) >>> 2] = 0x200;
    const sel = cpu + m.abi.cpu.SEG_SEL;
    [0x2b, 0x23, 0x2b, 0x2b, 0x53, 0x2b].forEach((v, i) => m.dv.setUint16(sel + i * 2, v, true));
    m.u32[(cpu + m.abi.cpu.FS_BASE) >>> 2] = teb;

    const C = L.CONTEXT;
    const stackTop = this.u32(teb + L.TEB['Tib.StackBase']);
    const ctx = ((stackTop - 16) & ~3) - C.__size;
    m.u8.fill(0, ctx, ctx + C.__size);
    this.w32(ctx + C.ContextFlags, 0x1003f); // CONTEXT_ALL for i386
    this.w32(ctx + C.SegCs, 0x23);
    this.w32(ctx + C.SegDs, 0x2b);
    this.w32(ctx + C.SegEs, 0x2b);
    this.w32(ctx + C.SegFs, 0x53);
    this.w32(ctx + C.SegSs, 0x2b);
    this.w32(ctx + C.EFlags, 0x202);
    this.w32(ctx + C.Eax, start);
    this.w32(ctx + C.Ebx, param);
    this.w32(ctx + C.Esp, stackTop - 16);
    this.w32(ctx + C.Eip, this.ex('RtlUserThreadStart'));
    this.w16(ctx + C.FloatSave, 0x27f);
    this.w16(ctx + C.ExtendedRegisters, 0x27f);
    this.w32(ctx + C.ExtendedRegisters + 24, 0x1f80);
    let sp = ctx;
    for (const v of [0, 0, 0, ctx, 0xdeadbabe]) {
      sp -= 4;
      this.w32(sp, v);
    }
    m.setReg(cpu, ESP, sp);
    return new Thread({ tid: 0, teb, cpu, start, resume: this.ex('LdrInitializeThunk') });
  }

  /** A thread ended: its CPU state slot, TEB and stack are freed. */
  endThread(t, status) {
    this.threads.ended(t, status);
    const stack = this.u32(t.teb + L.TEB.DeallocationStack);
    if (stack) this.vm.release(stack);
    this.vm.release(t.teb);
    this.m.freeCpu(t.cpu);
  }

  /** Starts the clock in KUSER_SHARED_DATA (call before run). */
  async startClock() {
    this.ticker = await startTicker(this.m.memory, this.clockBoot);
  }

  run() {
    try {
      this.threads.run();
      return { exitCode: null, error: new Error('every thread ended') };
    } catch (e) {
      if (e instanceof ProcessExit) return { exitCode: e.exitCode };
      return { exitCode: null, error: e };
    } finally {
      this.ticker?.stop();
    }
  }

  initSharedData() {
    const K = L.KUSER_SHARED_DATA;
    const b = USER_SHARED_DATA;
    // The clock (tick count, interrupt and system time): ./ticker.mjs keeps
    // it current once the process starts.
    this.clockBoot = performance.timeOrigin + performance.now();
    writeClock(this.m.dv, this.clockBoot);
    this.w32(b + K.NtProductType, 1);
    this.m.u8[b + K.ProductTypeIsValid] = 1;
    this.w32(b + K.NtMajorVersion, 10);
    this.w32(b + K.NtMinorVersion, 0);
    this.w32(b + K.NtBuildNumber, 19045);
    this.w32(b + K.ActiveProcessorCount, 1);
    this.w32(b + K.NumberOfPhysicalPages, 0x40000);
    this.wstr(b + K.NtSystemRoot, 'C:\\windows');
    this.w64(b + K.QpcFrequency, 10000000);
    // Processor features: x87, cmpxchg8b, MMX, SSE, SSE2 (the translator
    // will report them once SIMD is lifted), RDTSC.
    for (const f of [0, 2, 3, 8]) this.m.u8[b + K.ProcessorFeatures + f] = 1;
    this.updateTime();
  }

  updateTime() {
    const K = L.KUSER_SHARED_DATA;
    const b = USER_SHARED_DATA;
    const now = BigInt(Date.now()) * 10000n + 116444736000000000n;
    const sys = b + K.SystemTime;
    this.w32(sys, Number(now & 0xffffffffn));
    this.w32(sys + 4, Number(now >> 32n));
    this.w32(sys + 8, Number(now >> 32n));
    const ticks = Math.floor(performance.now());
    this.w64(b + K.TickCountQuad, ticks);
    const it = BigInt(Math.floor(performance.now() * 10000));
    this.w32(b + K.InterruptTime, Number(it & 0xffffffffn));
    this.w32(b + K.InterruptTime + 4, Number(it >> 32n));
    this.w32(b + K.InterruptTime + 8, Number(it >> 32n));
  }

  buildPeb(exe) {
    const P = L.PEB;
    const p = this.peb;
    this.w32(p + P.ImageBaseAddress, exe.base);
    this.w32(p + P.NumberOfProcessors, 1);
    this.w32(p + P.OSMajorVersion, 10);
    this.w32(p + P.OSMinorVersion, 0);
    this.w16(p + P.OSBuildNumber, 19045);
    this.w32(p + P.OSPlatformId, 2);
    this.w32(p + P.ImageSubSystem, exe.info.subsystem);
    this.w32(p + P.ImageSubSystemMajorVersion, exe.info.majorSubsystem);
    this.w32(p + P.ImageSubSystemMinorVersion, exe.info.minorSubsystem);
    this.w64(p + P.CriticalSectionTimeout, -(2592000n * 10000000n));
    this.w32(p + P.HeapSegmentReserve, 0x100000);
    this.w32(p + P.HeapSegmentCommit, 0x10000);
    this.w32(p + P.HeapDeCommitTotalFreeThreshold, 0x10000);
    this.w32(p + P.HeapDeCommitFreeBlockThreshold, 0x1000);
    this.w32(p + P.ActiveProcessAffinityMask, 1);
    // An empty API set map (version 6): no api-ms-win-* redirections yet.
    const api = this.alloc(0x1000, PAGE_READONLY, 'ApiSetMap');
    this.w32(api + 0, 6); // Version
    this.w32(api + 4, 28); // Size
    this.w32(api + 8, 0); // Flags
    this.w32(api + 12, 0); // Count
    this.w32(api + 16, 28); // EntryOffset
    this.w32(api + 20, 28); // HashOffset
    this.w32(api + 24, 31); // HashFactor
    this.w32(p + P.ApiSetMap, api);
    this.w32(p + P.ProcessParameters, this.buildParams());
  }

  buildParams() {
    const R = L.RTL_USER_PROCESS_PARAMETERS;
    const cmdline = this.argv.map((a) => (/[\s"]/.test(a) ? `"${a}"` : a)).join(' ');
    const env = Object.entries({
      PATH: 'C:\\windows\\system32;C:\\windows',
      SystemRoot: 'C:\\windows',
      windir: 'C:\\windows',
      TEMP: 'C:\\windows\\temp',
      TMP: 'C:\\windows\\temp',
      USERPROFILE: 'C:\\users\\wine',
      WINEDLLPATH: 'C:\\windows\\system32',
      ...this.env,
    })
      .map(([k, v]) => `${k}=${v}`)
      .join('\0') + '\0\0';
    const size = R.__size + 520 + (cmdline.length + this.exePath.length * 2 + env.length + 64) * 2;
    const p = this.alloc(size, PAGE_READWRITE, 'params');
    this.w32(p + R.AllocationSize, size);
    this.w32(p + R.Size, size);
    this.w32(p + R.Flags, 1); // normalized
    this.w32(p + R.wShowWindow, 1);
    this.w32(p + R.ProcessGroupId, 0x20);
    // Standard handles: console-less pipes the host prints.
    this.w32(p + R.hStdInput, this.newHandle({ type: 'file', std: 'stdin', path: null }));
    this.w32(p + R.hStdOutput, this.newHandle({ type: 'file', std: 'stdout', path: null }));
    this.w32(p + R.hStdError, this.newHandle({ type: 'file', std: 'stderr', path: null }));
    let dst = p + R.__size;
    // CurrentDirectory: DosPath UNICODE_STRING with MAX_PATH buffer.
    // The program's own directory, as when started from Explorer.
    const curdir = this.exePath.replace(/[^\\]*$/, '');
    this.wstr(dst, curdir);
    this.w16(p + R.CurrentDirectory, curdir.length * 2);
    this.w16(p + R.CurrentDirectory + 2, 520);
    this.w32(p + R.CurrentDirectory + 4, dst);
    dst += 520;
    dst = this.putUstr(p + R.ImagePathName, dst, this.exePath);
    dst = this.putUstr(p + R.CommandLine, dst, cmdline);
    dst = this.putUstr(p + R.WindowTitle, dst, this.exePath);
    dst = (dst + 3) & ~3;
    this.w32(p + R.Environment, dst);
    for (let i = 0; i < env.length; i++) this.w16(dst + i * 2, env.charCodeAt(i));
    this.w32(p + R.EnvironmentSize, env.length * 2);
    return p;
  }

  buildTeb(teb, stackReserve) {
    const T = L.TEB;
    const stackSize = Math.max(Math.ceil((stackReserve || 0x100000) / 0x10000) * 0x10000, 0x100000);
    const stack = this.vm.reserve(0, stackSize, { prot: PAGE_READWRITE, name: 'stack' });
    this.vm.commit(stack, stackSize, PAGE_READWRITE);
    this.w32(teb + T['Tib.ExceptionList'], 0xffffffff);
    this.w32(teb + T['Tib.StackBase'], stack + stackSize);
    this.w32(teb + T['Tib.StackLimit'], stack);
    this.w32(teb + T['Tib.Self'], teb);
    this.w32(teb + T.ClientId, 0x20);
    this.w32(teb + T.ClientId + 4, 0x24);
    this.w32(teb + T.Peb, this.peb);
    this.w32(teb + T.WOW32Reserved, this.syscallThunk);
    this.w32(teb + T.DeallocationStack, stack);
    this.w32(teb + T.ActivationContextStackPointer, teb + T.ActivationContextStack);
    // ActivationContextStack.FrameListCache list head (after ActiveFrame, at +4..+12)
    const flc = teb + T.ActivationContextStack + 4;
    this.w32(flc, flc);
    this.w32(flc + 4, flc);
    this.w32(teb + T.StaticUnicodeString, 0x020a0000);
    this.w32(teb + T.StaticUnicodeString + 4, teb + T.StaticUnicodeBuffer);
    this.w32(teb + T.CurrentLocale, 0x409);
  }

  // ---- System calls ------------------------------------------------------------

  /** Syscall numbers from ntdll's stubs: `mov eax, imm32` at each Nt export. */
  buildSyscallTable() {
    this.syscallNames = new Map();
    for (const [name, addr] of this.ntdllExports) {
      if (!/^(Nt|Zw|NtWine|wine_)/.test(name) && !name.startsWith('Nt')) continue;
      // mov eax, id; then mov edx, __wine_syscall or call [fs:0xc0]
      const u8 = this.m.u8;
      const stub = u8[addr] === 0xb8 && (u8[addr + 5] === 0xba || (u8[addr + 5] === 0x64 && u8[addr + 6] === 0xff));
      if (stub) {
        const id = this.u32(addr + 1);
        if (!this.syscallNames.has(id) || name.startsWith('Nt')) this.syscallNames.set(id, name.replace(/^Zw/, 'Nt'));
      }
    }
    this.log(`${this.syscallNames.size} system calls`);
  }

  syscall(cpu) {
    const m = this.m;
    const id = m.reg(cpu, EAX);
    const esp = m.reg(cpu, ESP);
    const ret = this.u32(esp);
    const name = this.syscallNames.get(id) ?? this.unix?.win32uNames.get(id) ?? `syscall_${id.toString(16)}`;
    const argBase = esp + 8; // [esp] -> stub, [esp+4] -> caller
    const a = (i) => this.u32(argBase + i * 4);
    this.counts.set(name, (this.counts.get(name) ?? 0) + 1);
    const unixImpl = this.unix?.syscalls.get(name);
    const threadImpl = this.unix ? THREAD_SYSCALLS[name] : undefined;
    const impl = SYSCALLS[name];
    const t = this.threads.current;
    this.sys = { name, ret, esp };
    let status;
    if (id >= 0x1000 && this.unix) {
      // win32u (user32 and gdi32 underneath): its Unix side reads the
      // arguments from the guest stack.
      t.nest++;
      try {
        status = this.unix.win32uSyscall(id, argBase);
      } finally {
        t.nest--;
      }
    } else if (threadImpl) {
      status = threadImpl.call(this, a, cpu, argBase);
    } else if (unixImpl && this.routeToUnix(name, a)) {
      t.nest++;
      try {
        status = unixImpl(a(0), a(1), a(2), a(3), a(4), a(5), a(6), a(7), a(8), a(9), a(10), a(11)) >>> 0;
      } catch (e) {
        if (e instanceof WebAssembly.RuntimeError) e.message += ` (in ${name} on Wine's Unix side)`;
        throw e;
      } finally {
        t.nest--;
      }
    } else if (!impl) {
      if (!this.unimplemented.has(name)) this.log(`UNIMPLEMENTED ${name}`);
      this.unimplemented.set(name, (this.unimplemented.get(name) ?? 0) + 1);
      status = STATUS.NOT_IMPLEMENTED;
    } else {
      status = impl.call(this, a, cpu, argBase);
    }
    if (status && typeof status === 'object') {
      if (status.jump !== undefined) {
        if (this.trace) this.log(`${name} -> jump ${hex(status.jump)}`);
        return status.jump;
      }
      if (status.yield) {
        if (this.trace) this.log(`${name}: thread ${hex(t.tid)} blocks`);
        return this.threads.yieldAddr;
      }
      status = status.status;
    }
    if (this.trace) this.log(`${name}(${[0, 1, 2, 3].map((i) => hex(a(i))).join(', ')}) = ${hex(status >>> 0)}`);
    m.setReg(cpu, EAX, status >>> 0);
    m.setReg(cpu, ESP, esp + 4);
    // Time slices end at system calls.
    if (this.threads.canYield() && this.threads.shouldPreempt()) {
      this.threads.yieldAt(ret);
      return this.threads.yieldAddr;
    }
    return ret;
  }

  /** An NT call from Wine's Unix side (native/wine-unix/inproc/host.c), run by the host's own implementation. */
  hostNtCall(name, args) {
    const impl = THREAD_SYSCALLS[name] ?? SYSCALLS[name];
    if (!impl) {
      this.unimplemented.set(`${name} (from Unix side)`, (this.unimplemented.get(name) ?? 0) + 1);
      return STATUS.NOT_IMPLEMENTED;
    }
    this.sys = { name, ret: 0, esp: 0 };
    let status = impl.call(this, (i) => args[i] >>> 0, this.cpu, 0);
    // Under the Unix side the thread cannot yield: waits come back done.
    if (status && typeof status === 'object') status = status.status ?? 0;
    if (this.trace) this.log(`unix -> ${name}(${args.slice(0, 4).map(hex).join(', ')}) = ${hex(status >>> 0)}`);
    return status >>> 0;
  }

  /**
   * KeUserModeCallback: win32u calls back into user32 (a window procedure,
   * a hook). Runs ntdll's KiUserCallbackDispatcher(id, args, len) on the
   * guest stack below the current frame until it ends with NtCallbackReturn,
   * then restores the thread's registers.
   */
  userCallback(id, args, len, retPtr, retLen) {
    const m = this.m;
    const saved = m.u8.slice(this.cpu, this.cpu + m.abi.cpu.SIZE);
    let sp = ((m.reg(this.cpu, ESP) - 256 - len) & ~15) >>> 0;
    const copy = sp;
    m.u8.copyWithin(copy, args, args + len);
    for (const v of [len, copy, id, m.abi.stop_address]) {
      sp -= 4;
      this.w32(sp, v);
    }
    m.setReg(this.cpu, ESP, sp);
    const depth = this.callbackResults.length;
    this.callbackResults.push(null);
    const t = this.threads.current;
    t.nest++;
    try {
      m.run(this.cpu, this.kiUserCallbackDispatcher);
    } finally {
      t.nest--;
      m.u8.set(saved, this.cpu);
    }
    const r = this.callbackResults.pop();
    if (this.callbackResults.length !== depth || !r) throw new Error(`user callback ${id} returned without NtCallbackReturn`);
    this.w32(retPtr, r.ptr);
    this.w32(retLen, r.len);
    return r.status;
  }

  /** Whether a host handle (or pseudo-handle the host implements) is involved. */
  isHostHandle(h) {
    h >>>= 0;
    return this.handles.has(h) || h === 0xffffffff || h === 0xfffffffe;
  }

  /** Handle-based calls go to Wine's Unix side only for server handles. */
  routeToUnix(name, a) {
    if (name === 'NtWaitForMultipleObjects') {
      for (let i = 0; i < a(0); i++) if (this.isHostHandle(this.u32(a(1) + i * 4))) return false;
      return true;
    }
    if (!HANDLE_ROUTED.has(name)) return true;
    return !this.isHostHandle(name === 'NtDuplicateObject' ? a(1) : a(0));
  }

  /** __wine_unix_call_dispatcher(UINT64 handle, UINT code, void *args): stdcall. */
  unixCall(cpu) {
    const m = this.m;
    const esp = m.reg(cpu, ESP);
    const ret = this.u32(esp);
    const handle = this.u32(esp + 4);
    const code = this.u32(esp + 12);
    const args = this.u32(esp + 16);
    let status = STATUS.NOT_IMPLEMENTED;
    if (handle === 0x1000) {
      status = this.ntdllUnixCall(code, args);
    } else if (handle === WIN32U_UNIXLIB && this.unix) {
      const t = this.threads.current;
      t.nest++;
      try {
        status = this.unix.win32uUnixCall(code, args);
      } finally {
        t.nest--;
      }
    } else if (handle === AUDIO_UNIXLIB && this.audio) {
      // The audio driver's timer and main loops block in the scheduler.
      this.sys = { name: 'audio', ret, esp, espAfter: esp + 20 };
      status = this.audio.call(code, args);
      if (status && typeof status === 'object') {
        if (status.yield) return this.threads.yieldAddr;
        status = status.status;
      }
    } else {
      this.log(`unix call to unknown library ${hex(handle)} code ${code}`);
    }
    m.setReg(cpu, EAX, status >>> 0);
    m.setReg(cpu, ESP, esp + 20);
    return ret;
  }

  ntdllUnixCall(code, args) {
    // enum ntdll_unix_funcs (dlls/ntdll/unixlib.h)
    switch (code) {
      case 2: {
        // wine_dbg_write(const char *str, ULONG len)
        const str = this.u32(args);
        const len = this.u32(args + 4);
        this.stderr(this.m.u8.slice(str, str + len));
        return len;
      }
      case 7: {
        // system_time_precise(LONGLONG *time)
        const now = BigInt(Math.floor((Date.now() + (performance.now() % 1)) * 10000)) + 116444736000000000n;
        this.w64(this.u32(args), now);
        return 0;
      }
      default:
        this.log(`ntdll unix call ${code} not implemented`);
        return STATUS.NOT_IMPLEMENTED;
    }
  }
}

export { EAX, ECX, EDX, EBX, ESP, EBP, ESI, EDI, MEM_PRIVATE };
