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
//
// 64-bit programs (a machine with `arch: 'x64'`) run Wine's x86_64 DLLs the
// same way. Their system-call stubs (`mov r10, rcx; mov eax, id; test byte
// [0x7ffe0308], 1; jne; syscall; ...; call [0x7ffe1000]`) take the
// `call [0x7ffe1000]` path because the host sets KUSER_SHARED_DATA's
// SystemCall flag and stores its thunk at 0x7ffe1000; the arguments are in
// r10, rdx, r8 and r9, then on the stack past the home space. Structures
// come from layout64.json, and pointer-sized fields go through `ptr` and
// `wptr`.

import { ProcessExit, hex } from '../runtime.mjs';
import { mapImage } from '../pe.mjs';
import { VirtualMemory, MEM_IMAGE, MEM_MAPPED, MEM_PRIVATE, PAGE_READWRITE, PAGE_EXECUTE_READ, PAGE_READONLY, PAGE_WRITECOPY, PAGE_EXECUTE_WRITECOPY } from './vm.mjs';
import { SYSCALLS, SYSCALLS64, STATUS, STUB_UNIXLIB } from './syscalls.mjs';
import { HANDLE_ROUTED, WIN32U_UNIXLIB } from './unix.mjs';
import { WINED3D_UNIXLIB } from './d3d.mjs';

/** opengl32's Unix side: a stub without OpenGL (see unixCall). */
export const GL_UNIXLIB = 0x4000;
/** System calls that signal objects other threads may be waiting on. */
const SIGNALLING = new Set(['NtSetEvent', 'NtPulseEvent', 'NtSetEventBoostPriority', 'NtReleaseSemaphore', 'NtReleaseMutant']);
/** The handle the host gives ws2_32.dll: name lookups only (enum ws_unix_funcs). */
export const WS2_32_UNIXLIB = 0x6000;

import layout from './layout.json' with { type: 'json' };
import layout64 from './layout64.json' with { type: 'json' };
import { installAssemblies } from './sxs.mjs';
import { attachNativeHeap } from './heap.mjs';
import { CodeWriteWatch } from './codewrite.mjs';
import { installExceptions } from './exceptions.mjs';
import { Scheduler, Thread } from './threads.mjs';
import { startTicker, writeClock } from './ticker.mjs';
import { THREAD_SYSCALLS, WIN32U_WAITS } from './thread-syscalls.mjs';
import { AUDIO_UNIXLIB, BrowserAudio } from './audio.mjs';
import { installRegistrations } from './registry-setup.mjs';
import { attachNativeStrings } from './strings.mjs';

export const L = layout;
export const L64 = layout64;

const EAX = 0, ECX = 1, EDX = 2, EBX = 3, ESP = 4, EBP = 5, ESI = 6, EDI = 7;
export const USER_SHARED_DATA = 0x7ffe0000;

/** Names (lowercase) of the DLLs a PE image imports. */
export function peImports(bytes) {
  const info = parsePe(bytes);
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const off = (rva) => {
    const sec = info.sections.find((x) => rva >= x.virtual_address && rva < x.virtual_address + Math.max(x.virtual_size, x.raw_size));
    return sec ? rva - sec.virtual_address + sec.raw_offset : -1;
  };
  const opt = dv.getUint32(0x3c, true) + 24;
  // The data directories: after 96 bytes in PE32, 112 in PE32+.
  const dirs = opt + (info.wide ? 112 : 96);
  const names = [];
  // Data directory 1: import descriptors of 20 bytes, ended by a zero one.
  for (let d = off(dv.getUint32(dirs + 8, true)); d >= 0 && dv.getUint32(d + 12, true); d += 20) {
    let p = off(dv.getUint32(d + 12, true));
    let name = '';
    while (bytes[p]) name += String.fromCharCode(bytes[p++]);
    names.push(name.toLowerCase());
  }
  return names;
}

/** Reads the parts of a PE header the host needs (image info, exports). */
export function parsePe(bytes) {
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const pe = dv.getUint32(0x3c, true);
  const coff = pe + 4;
  const opt = coff + 20;
  const nsec = dv.getUint16(coff + 2, true);
  const optSize = dv.getUint16(coff + 16, true);
  // PE32+ (64-bit): an 8-byte ImageBase and stack sizes, no BaseOfData.
  const wide = dv.getUint16(opt, true) === 0x20b;
  const u64 = (o) => Number(dv.getBigUint64(o, true));
  const info = {
    machine: dv.getUint16(coff, true),
    wide,
    characteristics: dv.getUint16(coff + 18, true),
    entryRva: dv.getUint32(opt + 16, true),
    imageBase: wide ? u64(opt + 24) : dv.getUint32(opt + 28, true),
    sizeOfImage: dv.getUint32(opt + 56, true),
    sectionAlignment: dv.getUint32(opt + 32, true),
    sizeOfHeaders: dv.getUint32(opt + 60, true),
    checksum: dv.getUint32(opt + 64, true),
    subsystem: dv.getUint16(opt + 68, true),
    dllCharacteristics: dv.getUint16(opt + 70, true),
    stackReserve: wide ? u64(opt + 72) : dv.getUint32(opt + 72, true),
    stackCommit: wide ? u64(opt + 80) : dv.getUint32(opt + 76, true),
    majorOs: dv.getUint16(opt + 40, true),
    minorOs: dv.getUint16(opt + 42, true),
    majorSubsystem: dv.getUint16(opt + 48, true),
    minorSubsystem: dv.getUint16(opt + 50, true),
    loaderFlags: dv.getUint32(opt + (wide ? 104 : 88), true),
    exportDir: dv.getUint32(opt + (wide ? 112 : 96), true),
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
 * Section protections of a mapped image, as Windows (and Wine's own image
 * mapper) sets them: headers read-only; each section readable, executable
 * and copy-on-write as its characteristics say; an image whose sections are
 * not page-aligned is one executable copy-on-write range.
 */
function protectImage(vm, base, info) {
  if (info.sectionAlignment < 0x1000) {
    vm.protect(base, info.sizeOfImage, PAGE_EXECUTE_WRITECOPY);
    return;
  }
  vm.protect(base, info.sizeOfHeaders || 0x1000, PAGE_READONLY);
  for (const s of info.sections) {
    const size = Math.max(s.virtual_size, s.raw_size);
    if (!size) continue;
    const c = s.characteristics;
    const x = c & 0x20000000, w = c & 0x80000000;
    const prot = x ? (w ? PAGE_EXECUTE_WRITECOPY : PAGE_EXECUTE_READ) : w ? PAGE_WRITECOPY : PAGE_READONLY;
    vm.protect(base + s.virtual_address, Math.min(size, info.sizeOfImage - s.virtual_address), prot);
  }
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
  // A copy with its own buffer: Node's small file buffers are views into a
  // shared pool, and Buffer#slice does not copy.
  const out = new Uint8Array(bytes);
  const dv = new DataView(out.buffer);
  const offsetOf = (rva) => {
    if (rva < info.sizeOfHeaders) return rva;
    for (const s of info.sections) {
      if (rva >= s.virtual_address && rva < s.virtual_address + s.raw_size) return s.raw_offset + rva - s.virtual_address;
    }
    return -1;
  };
  const delta = (base - info.imageBase) | 0;
  const delta64 = BigInt(base) - BigInt(info.imageBase);
  const dirs = opt + (info.wide ? 112 : 96);
  const relRva = dv.getUint32(dirs + 5 * 8, true);
  const relSize = dv.getUint32(dirs + 5 * 8 + 4, true);
  for (let p = relRva; p + 8 <= relRva + relSize; ) {
    const page = dv.getUint32(offsetOf(p), true);
    const blockSize = dv.getUint32(offsetOf(p + 4), true);
    if (blockSize < 8) break;
    for (let e = p + 8; e < p + blockSize; e += 2) {
      const entry = dv.getUint16(offsetOf(e), true);
      const type = entry >> 12;
      if (type === 0) continue; // IMAGE_REL_BASED_ABSOLUTE (padding)
      if (type !== 3 && type !== 10) throw new Error(`unsupported relocation type ${type}`);
      const at = offsetOf(page + (entry & 0xfff));
      // Fixups in uninitialized data have nothing to patch in the file.
      if (at < 0) continue;
      if (type === 3) dv.setUint32(at, (dv.getUint32(at, true) + delta) >>> 0, true);
      else dv.setBigUint64(at, BigInt.asUintN(64, dv.getBigUint64(at, true) + delta64), true); // DIR64
    }
    p += blockSize;
  }
  if (info.wide) dv.setBigUint64(opt + 24, BigInt(base), true);
  else dv.setUint32(opt + 28, base, true);
  return out;
}

/** Exported names of a mapped image -> addresses. */
export function readExports(m, base) {
  const u32 = (a) => m.dv.getUint32(a, true);
  const pe = base + u32(base + 0x3c);
  const wide = m.dv.getUint16(pe + 24, true) === 0x20b;
  const dirRva = u32(pe + 24 + (wide ? 112 : 96));
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
   * @param {boolean} [opts.storeMapAlways]  stores always look up the store map (see ./codewrite.mjs)
   * @param {WebAssembly.Module} [opts.nativeHeap]  ntdll's heap as native WebAssembly
   *        (./heap.mjs), for an ntdll translated with --native-heap
   * @param {WebAssembly.Module} [opts.nativeStrings]  string and locale functions as
   *        native WebAssembly (./strings.mjs), for DLLs translated with --native-strings
   */
  constructor(machine, opts) {
    this.m = machine;
    /** 64-bit Wine (x86_64 DLLs): pointer-sized fields are 8 bytes. */
    this.x64 = machine.x64;
    this.ps = this.x64 ? 8 : 4;
    /** Structure layouts for this architecture (tools/wine-layout). */
    this.L = this.x64 ? layout64 : layout;
    this.translate = opts.translate;
    this.files = opts.files;
    /** lower-case DOS path -> its last name as created (see rememberCase) */
    this.caseNames = opts.caseNames ?? new Map();
    /** lower-case DOS path -> FILETIME (BigInt) of its last write; others read as a fixed date */
    this.fileTimes = opts.fileTimes ?? new Map();
    this.dirs = indexDirectories(this.files);
    // The side-by-side store wineboot would have filled.
    installAssemblies(this.files);
    this.stdout = opts.stdout ?? (() => {});
    this.stderr = opts.stderr ?? (() => {});
    this.trace = opts.trace ?? false;
    this.vm = new VirtualMemory(machine, 0x10000, machine.thunkBase);
    /** Tells translated code when stores have to watch for code writes
     * (opts.storeMapAlways: on all the time). */
    this.codeWrites = opts.storeMapAlways ? null : new CodeWriteWatch(machine, this.vm);
    this.nativeHeap = opts.nativeHeap ? attachNativeHeap(machine, opts.nativeHeap, this.vm) : null;
    if (opts.nativeStrings) attachNativeStrings(machine, opts.nativeStrings);
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
    /** wined3d's WebGPU bridge (./d3d.mjs), or null: Direct3D without 3D */
    this.d3d = opts.d3d ?? null;
    // Translated loops preempt against the ticker's tick count (set before
    // any module is instantiated: it is an import).
    machine.tickAddr = USER_SHARED_DATA;
    // WWT_TRACE_CALLS=NtA,NtB: logs the first calls of these system calls.
    const tc = globalThis.process?.env?.WWT_TRACE_CALLS;
    this.traceCalls = tc ? new Set(tc.split(',')) : null;
    /** Results of NtCallbackReturn, one per user callback in progress. */
    this.callbackResults = [];
    this.images = new Map(); // base -> {path, info}
    /** Resolve import thunks in the address lookup (./thunks.mjs). */
    this.aliasThunks = opts.aliasThunks ?? true;
    this.modulesByPath = new Map();
    this.unimplemented = new Map();
    /** A ./status.mjs StatusSampler a host can attach; ticked from system calls and preemption. */
    this.status = null;
    this.statusCalls = 0;
    /** The system call being run: its name, return address and stack. */
    this.sys = { name: '', ret: 0, esp: 0, espAfter: undefined };
    this.syscallEntries = [];
    this.argv = opts.argv;
    this.exePath = opts.exePath; // DOS path, e.g. c:\hello.exe
    this.env = opts.env ?? {};
    /** Wine's debug channels, as WINEDEBUG sets them ("+actctx,warn+heap") */
    this.debug = opts.debug ?? '';
    /** The user's default locale (an LCID); the system's is always en-US. */
    this.userLocale = opts.locale ?? 0x409;
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
  /**
   * Remembers the case of each name in a DOS path (paths are kept lower
   * case, for Windows' case-insensitive lookups): directory listings give
   * names as they were created, as Windows does (Far Cry finds its shaders
   * by the names it lists).
   */
  rememberCase(path) {
    recordCase(this.caseNames, path);
  }

  /** Whether a program's pointer to `size` bytes is committed memory above the null page. */
  writable(a, size) {
    if (a < 0x10000 || a + size > this.m.guestLimit) return false;
    for (let p = this.vm.pageOf(a); p <= this.vm.pageOf(a + size - 1); p++) if (!this.vm.prot[p]) return false;
    return true;
  }
  u64(a) {
    return this.m.dv.getBigUint64(a, true);
  }
  /** Pointer-sized fields (pointers, handles, SIZE_T): 4 or 8 bytes. */
  ptr(a) {
    return this.x64 ? Number(this.u64(a)) : this.u32(a);
  }
  wptr(a, v) {
    if (this.x64) this.w64(a, v);
    else this.w32(a, v);
  }

  /** Reads a UNICODE_STRING. */
  ustr(a) {
    if (!a) return null;
    const len = this.u16(a);
    const buf = this.ptr(a + this.L.UNICODE_STRING.Buffer);
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
    this.wptr(a + this.L.UNICODE_STRING.Buffer, buf);
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
    return /^[a-z]:$/.test(dosPath) || this.dirs.has(dosPath);
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
    protectImage(this.vm, base, info);
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

  /**
   * Something the host does not do, or only approximates: said once on
   * stderr, in Wine's style, so a wrong or missing answer shows where the
   * program first asked instead of as a crash later.
   */
  fixme(s) {
    this.fixmes ??= new Set();
    if (this.fixmes.has(s)) return;
    this.fixmes.add(s);
    this.stderr(new TextEncoder().encode(`fixme:wwt:${s}\n`));
  }

  // ---- Process setup -----------------------------------------------------------

  boot(ntdllPath, exeDosPath) {
    const m = this.m;
    // Shared user data.
    this.vm.reserve(USER_SHARED_DATA, 0x10000, { prot: PAGE_READONLY, name: 'KUSER_SHARED_DATA' });
    // x86-64: the page after it holds the system-call dispatcher pointer.
    this.vm.commit(USER_SHARED_DATA, this.x64 ? 0x2000 : 0x1000, PAGE_READONLY);
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
    this.syscallThunk = m.addThunk((cpu) => (this.x64 ? this.syscall64(cpu) : this.syscall(cpu)));
    this.unixCallThunk = m.addThunk((cpu) => (this.x64 ? this.unixCall64(cpu) : this.unixCall(cpu)));
    this.wptr(ex('__wine_syscall_dispatcher'), this.syscallThunk);
    this.wptr(ex('__wine_unix_call_dispatcher'), this.unixCallThunk);
    this.w32(ex('__wine_unixlib_handle'), 0x1000);
    this.w32(ex('__wine_unixlib_handle') + 4, 0);
    if (this.x64) {
      // The stubs call through 0x7ffe1000 when SystemCall is set.
      this.m.u8[USER_SHARED_DATA + this.L.KUSER_SHARED_DATA.SystemCall] = 1;
      this.w64(USER_SHARED_DATA + 0x1000, this.syscallThunk);
    }
    this.kiUserCallbackDispatcher = ex('KiUserCallbackDispatcher');
    this.nativeHeap?.init(ex('RtlRaiseStatus'));
    // Exception dispatch (./exceptions.mjs) builds i386 records and
    // contexts; on x86-64 an exception still stops the program.
    if (!this.x64) installExceptions(this, ex);

    // The main executable, mapped by the "Unix side" as Wine does.
    const exe = this.mapImageFile(exeDosPath, this.fileAt(exeDosPath));
    if (exe.status) throw new Error(`cannot map ${exeDosPath} at its base`);
    this.exe = exe;

    // PEB, TEB, process parameters.
    // The debug channels follow the PEB (as Wine's Unix side leaves them;
    // ntdll's __wine_dbg_get_channel_flags reads them one page per 4 bytes
    // of pointer size after it: +0x1000 on i386, +0x2000 on x86-64).
    const debugAt = 0x1000 * (this.ps / 4);
    this.peb = this.alloc(debugAt + 0x1000, PAGE_READWRITE, 'PEB');
    this.buildPeb(exe);
    this.writeDebugOptions(this.peb + debugAt, this.debug);
    this.ex = ex;
    this.stackReserve = exe.info.stackReserve;
    this.threads = new Scheduler(this);
    this.m.onPreempt = (cpu, eip) => {
      this.status?.tick();
      return this.threads.preempt(cpu, eip);
    };
    // Fast mode translates within the executable section around a miss.
    this.m.codeEnd = (addr) => this.codeEnd(addr);
    // The main thread: its TEB, CPU state and initial context. Wine's Unix
    // side registers the process with its first thread.
    const main = this.newThread(exe.base + exe.info.entryRva, this.peb, exe.info.stackReserve);
    this.teb = main.teb;
    this.cpu = main.cpu;
    if (this.unix) {
      this.unix.initProcess(main.teb, this.peb);
      // COM classes and the like, as wineboot's DLL registration leaves them.
      const keys = installRegistrations(this, this.files);
      this.log(`registry: ${keys} keys from Wine's DLL registrations`);
      // Waits in Wine's Unix side run the other threads (./threads.mjs).
      this.threads.realWait = this.unix.M.hostWait;
      this.unix.M.hostWait = (ms) => this.threads.waitNested(ms);
    } else {
      this.threads.realWait = (ms) => (ms < 0 ? -1 : (Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms), 0));
    }
    // ClientId.UniqueThread
    main.tid = this.u32(main.teb + this.L.TEB.ClientId + this.ps);
    this.threads.add(main);
    this.threads.switchTo(main);
    this.entry = main.resume;
  }

  /**
   * A thread's TEB, stack, CPU state and initial context, as Wine's Unix
   * side builds them (signal_i386.c, signal_x86_64.c): LdrInitializeThunk
   * runs first, then continues at RtlUserThreadStart(start, param).
   */
  newThread(start, param, stackReserve) {
    const m = this.m;
    const teb = this.alloc(0x2000, PAGE_READWRITE, 'TEB');
    this.buildTeb(teb, stackReserve);
    const cpu = m.newCpu();
    m.dv.setUint16(cpu + m.abi.cpu.FPU_CW, 0x27f, true);
    // Windows starts threads with IF set.
    m.w32(cpu + m.abi.cpu.EFLAGS_SYS, 0x200);
    if (this.x64) return this.newThread64(start, param, teb, cpu);
    const sel = cpu + m.abi.cpu.SEG_SEL;
    [0x2b, 0x23, 0x2b, 0x2b, 0x53, 0x2b].forEach((v, i) => m.dv.setUint16(sel + i * 2, v, true));
    m.w32(cpu + m.abi.cpu.FS_BASE, teb);

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
    const stack = this.ptr(t.teb + this.L.TEB.DeallocationStack);
    if (stack) this.vm.release(stack);
    this.vm.release(t.teb);
    this.m.freeCpu(t.cpu);
  }

  /** End of the executable section of a loaded image that contains `addr` (undefined outside one). */
  codeEnd(addr) {
    for (const [base, img] of this.images) {
      if (addr < base || addr >= base + img.size) continue;
      for (const sec of img.info.sections) {
        const lo = base + sec.virtual_address;
        const hi = lo + Math.max(sec.virtual_size, sec.raw_size);
        if (addr >= lo && addr < hi && sec.characteristics & 0x20000000) return hi; // IMAGE_SCN_MEM_EXECUTE
      }
      return undefined;
    }
    return undefined;
  }

  /** `module+offset` for a guest address, for diagnostics. */
  describeAddress(addr) {
    addr >>>= 0;
    for (const [base, img] of this.images) {
      if (addr >= base && addr < base + img.size) return `${img.path.split('\\').pop()}+${hex(addr - base)}`;
    }
    return hex(addr);
  }

  /** Return addresses up a thread's EBP chain (frame-pointer code only). */
  backtrace(t, depth = 8) {
    const out = [];
    let ebp = this.m.reg(t.cpu, 5) >>> 0;
    for (let i = 0; i < depth && ebp > 0x10000 && ebp < 0x80000000; i++) {
      out.push(this.describeAddress(this.u32(ebp + 4)));
      const next = this.u32(ebp) >>> 0;
      if (next <= ebp) break;
      ebp = next;
    }
    return out;
  }

  /** Starts the clock in KUSER_SHARED_DATA (call before run). */
  async startClock() {
    this.ticker = await startTicker(this.m.memory, this.clockBoot);
    this.threads.useTicker(this.clockBoot);
  }

  /**
   * x86-64: the TEB at gs:0, and the initial context for
   * LdrInitializeThunk as signal_x86_64.c builds it: rcx = start, rdx =
   * param, rip = RtlUserThreadStart, the CONTEXT just below the stack top
   * and rcx pointing at it.
   */
  newThread64(start, param, teb, cpu) {
    const m = this.m;
    const sel = cpu + m.abi.cpu.SEG_SEL;
    [0x2b, 0x33, 0x2b, 0x2b, 0x53, 0x2b].forEach((v, i) => m.dv.setUint16(sel + i * 2, v, true));
    this.w64(cpu + m.abi.cpu64.GS_BASE, teb);
    const C = this.L.CONTEXT;
    const rsp = this.ptr(teb + this.L.TEB['Tib.StackBase']) - 0x28;
    const ctx = rsp - (rsp % 16) - C.__size;
    m.u8.fill(0, ctx, ctx + C.__size);
    this.w32(ctx + C.ContextFlags, 0x10000b); // CONTEXT_FULL
    this.w64(ctx + C.Rcx, start);
    this.w64(ctx + C.Rdx, param);
    this.w64(ctx + C.Rsp, rsp);
    this.w64(ctx + C.Rip, this.ex('RtlUserThreadStart'));
    this.w16(ctx + C.SegCs, 0x33);
    for (const r of ['SegDs', 'SegEs', 'SegGs', 'SegSs']) this.w16(ctx + C[r], 0x2b);
    this.w16(ctx + C.SegFs, 0x53);
    this.w32(ctx + C.EFlags, 0x200);
    this.w16(ctx + C['FltSave.ControlWord'], 0x27f);
    this.w32(ctx + C['FltSave.MxCsr'], 0x1f80);
    this.w32(ctx + C.MxCsr, 0x1f80);
    this.w64(ctx - 8, 0);
    m.setSp(cpu, ctx - 8);
    m.setReg64(cpu, ECX, ctx);
    return new Thread({ tid: 0, teb, cpu, start, resume: this.ex('LdrInitializeThunk') });
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
    const K = this.L.KUSER_SHARED_DATA;
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
    const K = this.L.KUSER_SHARED_DATA;
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
    const P = this.L.PEB;
    const p = this.peb;
    this.wptr(p + P.ImageBaseAddress, exe.base);
    this.w32(p + P.NumberOfProcessors, 1);
    this.w32(p + P.OSMajorVersion, 10);
    this.w32(p + P.OSMinorVersion, 0);
    this.w16(p + P.OSBuildNumber, 19045);
    this.w32(p + P.OSPlatformId, 2);
    this.w32(p + P.ImageSubSystem, exe.info.subsystem);
    this.w32(p + P.ImageSubSystemMajorVersion, exe.info.majorSubsystem);
    this.w32(p + P.ImageSubSystemMinorVersion, exe.info.minorSubsystem);
    this.w64(p + P.CriticalSectionTimeout, -(2592000n * 10000000n));
    this.wptr(p + P.HeapSegmentReserve, 0x100000);
    this.wptr(p + P.HeapSegmentCommit, 0x10000);
    this.wptr(p + P.HeapDeCommitTotalFreeThreshold, 0x10000);
    this.wptr(p + P.HeapDeCommitFreeBlockThreshold, 0x1000);
    this.wptr(p + P.ActiveProcessAffinityMask, 1);
    this.wptr(p + P.ApiSetMap, this.loadApiSet());
    this.wptr(p + P.ProcessParameters, this.buildParams());
  }

  /**
   * The API set map (api-ms-win-* names to DLLs, e.g. the Universal CRT's
   * api-ms-win-crt-* to ucrtbase): as Wine's Unix loader does, the
   * `.apiset` section of apisetschema.dll mapped as a file. Without that
   * DLL, an empty map (version 6).
   */
  loadApiSet() {
    const file = this.fileAt('c:\\windows\\system32\\apisetschema.dll');
    if (file) {
      const info = parsePe(file);
      const dv = new DataView(file.buffer, file.byteOffset, file.byteLength);
      const pe = dv.getUint32(0x3c, true);
      const first = pe + 24 + dv.getUint16(pe + 20, true);
      for (let i = 0; i < info.sections.length; i++) {
        const sec = first + i * 40;
        const name = String.fromCharCode(...file.subarray(sec, sec + 8)).replace(/\0+$/, '');
        const s = info.sections[i];
        if (name !== '.apiset' || dv.getUint32(s.raw_offset, true) !== 6) continue;
        const size = Math.ceil(file.length / 0x1000) * 0x1000;
        const base = this.vm.reserve(0, size, { type: MEM_MAPPED, prot: PAGE_READONLY, name: 'apisetschema.dll' });
        this.vm.commit(base, size, PAGE_READONLY);
        this.m.u8.set(file, base);
        return base + s.raw_offset;
      }
    }
    const api = this.alloc(0x1000, PAGE_READONLY, 'ApiSetMap');
    this.w32(api + 0, 6); // Version
    this.w32(api + 4, 28); // Size
    this.w32(api + 8, 0); // Flags
    this.w32(api + 12, 0); // Count
    this.w32(api + 16, 28); // EntryOffset
    this.w32(api + 20, 28); // HashOffset
    this.w32(api + 24, 31); // HashFactor
    return api;
  }

  buildParams() {
    const R = this.L.RTL_USER_PROCESS_PARAMETERS;
    const cmdline = this.argv.map((a) => (/[\s"]/.test(a) ? `"${a}"` : a)).join(' ');
    const env = Object.entries({
      PATH: 'C:\\windows\\system32;C:\\windows',
      SystemRoot: 'C:\\windows',
      windir: 'C:\\windows',
      TEMP: 'C:\\windows\\temp',
      TMP: 'C:\\windows\\temp',
      USERPROFILE: 'C:\\users\\wine',
      WINEDLLPATH: 'C:\\windows\\system32',
      // wined3d on the program's thread (its command stream thread would
      // spin between the cooperative scheduler's switches). With a WebGPU
      // bridge (./d3d.mjs) it renders through that; without one, DirectDraw
      // runs without 3D and presents through GDI.
      WINE_D3D_CONFIG: [this.d3d && !this.x64 ? 'csmt=0' : 'renderer=no3d,csmt=0']
        .filter(Boolean)
        .join(','),
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
    this.wptr(p + R.hStdInput, this.newHandle({ type: 'file', std: 'stdin', path: null }));
    this.wptr(p + R.hStdOutput, this.newHandle({ type: 'file', std: 'stdout', path: null }));
    this.wptr(p + R.hStdError, this.newHandle({ type: 'file', std: 'stderr', path: null }));
    let dst = p + R.__size;
    // CurrentDirectory: DosPath UNICODE_STRING with MAX_PATH buffer.
    // The program's own directory, as when started from Explorer.
    const curdir = this.exePath.replace(/[^\\]*$/, '');
    this.wstr(dst, curdir);
    this.w16(p + R.CurrentDirectory, curdir.length * 2);
    this.w16(p + R.CurrentDirectory + 2, 520);
    this.wptr(p + R.CurrentDirectory + this.L.UNICODE_STRING.Buffer, dst);
    dst += 520;
    dst = this.putUstr(p + R.ImagePathName, dst, this.exePath);
    dst = this.putUstr(p + R.CommandLine, dst, cmdline);
    dst = this.putUstr(p + R.WindowTitle, dst, this.exePath);
    dst = (dst + 7) & ~7;
    this.wptr(p + R.Environment, dst);
    for (let i = 0; i < env.length; i++) this.w16(dst + i * 2, env.charCodeAt(i));
    this.wptr(p + R.EnvironmentSize, env.length * 2);
    return p;
  }

  buildTeb(teb, stackReserve) {
    const T = this.L.TEB;
    const ps = this.ps;
    const stackSize = Math.max(Math.ceil((stackReserve || 0x100000) / 0x10000) * 0x10000, 0x100000);
    const stack = this.vm.reserve(0, stackSize, { prot: PAGE_READWRITE, name: 'stack' });
    this.vm.commit(stack, stackSize, PAGE_READWRITE);
    // No SEH frames (x86; x86-64 exceptions are table-based).
    if (!this.x64) this.w32(teb + T['Tib.ExceptionList'], 0xffffffff);
    this.wptr(teb + T['Tib.StackBase'], stack + stackSize);
    this.wptr(teb + T['Tib.StackLimit'], stack);
    this.wptr(teb + T['Tib.Self'], teb);
    this.wptr(teb + T.ClientId, 0x20);
    this.wptr(teb + T.ClientId + ps, 0x24);
    this.wptr(teb + T.Peb, this.peb);
    if (!this.x64) this.w32(teb + T.WOW32Reserved, this.syscallThunk);
    this.wptr(teb + T.DeallocationStack, stack);
    this.wptr(teb + T.ActivationContextStackPointer, teb + T.ActivationContextStack);
    // ActivationContextStack.FrameListCache list head (after ActiveFrame)
    const flc = teb + T.ActivationContextStack + ps;
    this.wptr(flc, flc);
    this.wptr(flc + ps, flc);
    this.w32(teb + T.StaticUnicodeString, 0x020a0000);
    this.wptr(teb + T.StaticUnicodeString + this.L.UNICODE_STRING.Buffer, teb + T.StaticUnicodeBuffer);
    this.w32(teb + T.CurrentLocale, 0x409);
  }

  // ---- System calls ------------------------------------------------------------

  /** Syscall numbers from ntdll's stubs: `mov eax, imm32` at each Nt export. */
  buildSyscallTable() {
    this.syscallNames = new Map();
    for (const [name, addr] of this.ntdllExports) {
      if (!/^(Nt|Zw|NtWine|wine_)/.test(name) && !name.startsWith('Nt')) continue;
      // i386: mov eax, id; then mov edx, __wine_syscall or call [fs:0xc0].
      // x86-64: mov r10, rcx; mov eax, id; test byte [0x7ffe0308], 1.
      const u8 = this.m.u8;
      const at = this.x64 ? addr + 3 : addr;
      const stub = this.x64
        ? u8[addr] === 0x4c && u8[addr + 1] === 0x8b && u8[addr + 2] === 0xd1 && u8[at] === 0xb8
        : u8[addr] === 0xb8 && (u8[addr + 5] === 0xba || (u8[addr + 5] === 0x64 && u8[addr + 6] === 0xff));
      if (stub) {
        const id = this.u32(at + 1);
        if (!this.syscallNames.has(id) || name.startsWith('Nt')) this.syscallNames.set(id, name.replace(/^Zw/, 'Nt'));
      }
    }
    this.syscallEntries = [];
    this.log(`${this.syscallNames.size} system calls`);
  }

  /** What system call `id` runs, looked up once per number. */
  syscallEntry(id) {
    const name = this.syscallNames.get(id) ?? this.unix?.win32uNames.get(id) ?? `syscall_${id.toString(16)}`;
    const e = {
      name,
      impl: (this.x64 && SYSCALLS64[name]) || SYSCALLS[name],
      unixImpl: this.unix?.syscalls.get(name),
      // The scheduler's system calls serve i386 only so far.
      threadImpl: this.unix && !this.x64 ? THREAD_SYSCALLS[name] : undefined,
      win32uWait: id >= 0x1000 && this.unix ? WIN32U_WAITS[this.unix.win32uNames.get(id)] : undefined,
      calls: 0,
    };
    this.syscallEntries[id] = e;
    return e;
  }

  /** Calls per system call name, for diagnostics. */
  get counts() {
    return new Map(this.syscallEntries.filter(Boolean).map((e) => [e.name, e.calls]));
  }

  syscall(cpu) {
    const m = this.m;
    const id = m.reg(cpu, EAX);
    const esp = m.reg(cpu, ESP);
    const ret = this.u32(esp);
    const e = this.syscallEntries[id] ?? this.syscallEntry(id);
    const { name, impl, unixImpl, threadImpl, win32uWait } = e;
    e.calls++;
    // Status samples (./status.mjs), checked every 64 calls.
    if (this.status && (++this.statusCalls & 63) === 0) this.status.tick();
    const argBase = esp + 8; // [esp] -> stub, [esp+4] -> caller
    const a = (i) => this.u32(argBase + i * 4);
    const t = this.threads.current;
    const sys = this.sys;
    sys.name = name;
    sys.ret = ret;
    sys.esp = esp;
    sys.espAfter = undefined;
    const outer = t.inCall;
    t.inCall = name;
    let status;
    if (win32uWait) {
      status = win32uWait.call(this, a, cpu, argBase);
    } else if (id >= 0x1000 && this.unix) {
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
        status = unixImpl(Array.from({ length: 12 }, (_, i) => a(i)));
      } catch (e) {
        if (e instanceof WebAssembly.RuntimeError) e.message += ` (in ${name} on Wine's Unix side)`;
        throw e;
      } finally {
        t.nest--;
      }
      // Waits an object satisfies complete when it is signalled.
      if (SIGNALLING.has(name) && !status) this.threads.signalled();
    } else if (!impl) {
      if (!this.unimplemented.has(name)) this.fixme(`${name} not implemented`);
      this.unimplemented.set(name, (this.unimplemented.get(name) ?? 0) + 1);
      status = STATUS.NOT_IMPLEMENTED;
    } else {
      try {
        status = impl.call(this, a, cpu, argBase);
      } catch (e) {
        // A pointer outside memory: Windows checks the program's pointers
        // and fails the call.
        if (!(e instanceof RangeError) || /call stack/.test(e.message)) throw e;
        status = STATUS.ACCESS_VIOLATION;
      }
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
    t.inCall = outer;
    if (this.trace || this.traceCalls?.has(name)) {
      const n = (this.traceCount = (this.traceCount ?? 0) + 1);
      if (this.trace || n <= Number(process.env.WWT_TRACE_LIMIT ?? 40)) this.stderr(new TextEncoder().encode(`[call] ${name}(${[0, 1, 2, 3, 4, 5, 6, 7, 8, 9].map((i) => hex(a(i))).join(", ")}) = ${hex(status >>> 0)} [thread ${hex(t.tid)}]\n`));
    }
    m.setReg(cpu, EAX, status >>> 0);
    m.setReg(cpu, ESP, esp + 4);
    // Time slices end at system calls.
    if (this.threads.canYield() && this.threads.shouldPreempt()) {
      this.threads.yieldAt(ret);
      return this.threads.yieldAddr;
    }
    return ret;
  }

  /**
   * x86-64: reached by `call [0x7ffe1000]` from a stub, so [rsp] returns to
   * the stub, [rsp+8] to its caller, and the arguments are in r10, rdx, r8
   * and r9, then at rsp+0x30 (past the caller's home space).
   */
  syscall64(cpu) {
    const m = this.m;
    const id = m.reg(cpu, EAX);
    const rsp = m.sp(cpu);
    const ret = m.ptr(rsp);
    const e = this.syscallEntries[id] ?? this.syscallEntry(id);
    const { name, impl, unixImpl } = e;
    e.calls++;
    const R = [10, EDX, 8, 9];
    const raw = (i) => (i < 4 ? m.reg64(cpu, R[i]) : this.u64(rsp + 0x30 + (i - 4) * 8));
    const a = (i) => this.arg64(raw(i));
    // The module's thunks take the raw slots: C's casts drop a 32-bit
    // argument's garbage upper half. The most any call takes is 17
    // (NtUserCreateWindowEx, whose last is `ansi`); the module has room for 32.
    const slots = () => Array.from({ length: 20 }, (_, i) => raw(i));
    let status;
    if (id >= 0x1000 && this.unix) {
      // win32u: a 64-bit result (handles, LRESULTs).
      const r = this.unix.win32uSyscall64(id, slots());
      m.setReg64(cpu, EAX, r);
      m.setSp(cpu, rsp + 8);
      return ret;
    } else if (unixImpl && this.routeToUnix(name, a)) {
      status = unixImpl(slots());
    } else if (!impl) {
      if (!this.unimplemented.has(name)) this.fixme(`${name} not implemented`);
      this.unimplemented.set(name, (this.unimplemented.get(name) ?? 0) + 1);
      status = STATUS.NOT_IMPLEMENTED;
    } else {
      status = impl.call(this, a, cpu, rsp + 0x30);
    }
    if (status && typeof status === 'object' && status.jump !== undefined) {
      if (this.trace) this.log(`${name} -> jump ${hex(status.jump)}`);
      return status.jump;
    }
    if (this.trace) this.log(`${name}(${[0, 1, 2, 3].map((i) => hex(a(i))).join(', ')}) = ${hex(status >>> 0)}`);
    m.setReg(cpu, EAX, status >>> 0);
    m.setSp(cpu, rsp + 8);
    return ret;
  }

  /** An NT call from Wine's Unix side (native/wine-unix/inproc/host.c), run by the host's own implementation. */
  hostNtCall(name, args) {
    const impl = (this.x64 ? SYSCALLS64[name] : THREAD_SYSCALLS[name]) ?? SYSCALLS[name];
    if (!impl) {
      this.unimplemented.set(`${name} (from Unix side)`, (this.unimplemented.get(name) ?? 0) + 1);
      return STATUS.NOT_IMPLEMENTED;
    }
    // wasm64 passes 64-bit slots (BigInts); pseudo-handles read as their
    // 32-bit forms, as on i386.
    const a = this.x64 ? (i) => exactArg64(BigInt(args[i] ?? 0)) : (i) => args[i] >>> 0;
    this.sys = { name, ret: 0, esp: 0, espAfter: undefined };
    let status = impl.call(this, a, this.cpu, 0);
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
    if (this.x64) return this.userCallback64(id, args, len, retPtr, retLen);
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
      // The saved state's copy of a flag the runtime owns may be stale.
      m.w32(this.cpu + m.abi.cpu.CODE_WRITABLE, m.codeWritable);
    }
    const r = this.callbackResults.pop();
    if (this.callbackResults.length !== depth || !r) throw new Error(`user callback ${id} returned without NtCallbackReturn`);
    this.w32(retPtr, r.ptr);
    this.w32(retLen, r.len);
    return r.status;
  }

  /**
   * x86-64: KiUserCallbackDispatcher reads Wine's callback_stack_layout at
   * rsp: home space, then args (+0x20), len (+0x28), id (+0x2c), a machine
   * frame (+0x30) and the copied arguments (+0x58).
   */
  userCallback64(id, args, len, retPtr, retLen) {
    const m = this.m;
    const saved = m.u8.slice(this.cpu, this.cpu + m.cpuSize);
    const rsp = m.sp(this.cpu);
    let sp = rsp - 256 - 0x58 - len;
    sp -= sp % 16;
    m.u8.fill(0, sp, sp + 0x58);
    m.u8.copyWithin(sp + 0x58, args, args + len);
    this.w64(sp + 0x20, sp + 0x58);
    this.w32(sp + 0x28, len);
    this.w32(sp + 0x2c, id);
    this.w64(sp + 0x30 + 0x18, rsp); // machine frame: rsp
    m.setSp(this.cpu, sp);
    const depth = this.callbackResults.length;
    this.callbackResults.push(null);
    try {
      m.run(this.cpu, this.kiUserCallbackDispatcher);
    } finally {
      m.u8.set(saved, this.cpu);
    }
    const r = this.callbackResults.pop();
    if (this.callbackResults.length !== depth || !r) throw new Error(`user callback ${id} returned without NtCallbackReturn`);
    this.w64(retPtr, r.ptr);
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
      for (let i = 0; i < a(0); i++) if (this.isHostHandle(this.u32(a(1) + i * this.ps))) return false;
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
    } else if (handle === WINED3D_UNIXLIB) {
      status = this.d3d ? this.d3d.unixCall(m, code, args) : 0xc00000bb; // STATUS_NOT_SUPPORTED
    } else if (handle === AUDIO_UNIXLIB && this.audio) {
      // The audio driver's timer and main loops block in the scheduler.
      this.sys = { name: 'audio', ret, esp, espAfter: esp + 20 };
      status = this.audio.call(code, args);
      if (status && typeof status === 'object') {
        if (status.yield) return this.threads.yieldAddr;
        status = status.status;
      }
    } else if (handle === WS2_32_UNIXLIB) {
      status = this.winsockCall(code, args);
    } else if (handle === GL_UNIXLIB) {
      // opengl32 without OpenGL: it attaches (wined3d imports it), and
      // every GL call fails.
      if (code <= 2) status = STATUS.SUCCESS;
      else if (!this.glWarned) {
        this.glWarned = true;
        this.log(`opengl32: no OpenGL (unix call ${code})`);
      }
    } else {
      if (handle !== STUB_UNIXLIB) this.log(`unix call to unknown library ${hex(handle)} code ${code}`);
    }
    m.setReg(cpu, EAX, status >>> 0);
    m.setReg(cpu, ESP, esp + 20);
    return ret;
  }

  /**
   * ws2_32's Unix side: its name lookups (dlls/ws2_32/unixlib.c), which
   * return Winsock error codes. There is no network: the machine is named,
   * it and localhost resolve to the loopback address (as on a Windows
   * machine without a network), and every other lookup fails, so games
   * fall back to their local play. Sockets themselves go through ntdll's
   * AFD device, which does not exist, so creating one fails.
   */
  winsockCall(code, args) {
    const WSAEFAULT = 10014;
    const WSAHOST_NOT_FOUND = 11001;
    const ERROR_INSUFFICIENT_BUFFER = 122;
    if (code === 2) {
      // gethostbyname({const char *name, WS_hostent *host, unsigned *size}),
      // laid out as hostent_from_unix does: the hostent, the alias and
      // address lists, the address, the name.
      const name = this.m.readCString(this.u32(args)).toLowerCase();
      if (name !== 'localhost' && name !== 'webwindows') return WSAHOST_NOT_FOUND;
      const host = this.u32(args + 4);
      const psize = this.u32(args + 8);
      const needed = 16 + 4 + 8 + 4 + name.length + 1;
      if (this.u32(psize) < needed) return (this.w32(psize, needed), ERROR_INSUFFICIENT_BUFFER);
      this.m.u8.fill(0, host, host + needed);
      const aliases = host + 16;
      const list = aliases + 4;
      const addr = list + 8;
      const str = addr + 4;
      this.w32(host + 4, aliases);
      this.w16(host + 8, 2); // AF_INET
      this.w16(host + 10, 4);
      this.w32(host + 12, list);
      this.w32(list, addr);
      this.m.u8.set([127, 0, 0, 1], addr);
      for (let i = 0; i < name.length; i++) this.m.u8[str + i] = name.charCodeAt(i);
      this.w32(host, str);
      return 0;
    }
    if (code === 3) {
      // gethostname({char *name, unsigned size})
      const name = 'webwindows';
      const buf = this.u32(args);
      if (this.u32(args + 4) <= name.length) return WSAEFAULT;
      for (let i = 0; i < name.length; i++) this.m.u8[buf + i] = name.charCodeAt(i);
      this.m.u8[buf + name.length] = 0;
      return 0;
    }
    return WSAHOST_NOT_FOUND;
  }

  /**
   * A 64-bit system-call argument as a number. Arguments are pointers,
   * handles and 32-bit integers, and a 32-bit argument's slot may carry
   * garbage in its upper half (a stack slot written with a 32-bit store), so
   * a value with upper bits set counts as a pointer only when it points into
   * mapped memory. Sign-extended 32-bit values (pseudo-handles such as -1
   * and -2) read as their 32-bit forms, as the i386 path sees them.
   */
  arg64(v) {
    const hi = Number(v >> 32n);
    const lo = Number(v & 0xffff_ffffn);
    if (hi === 0) return lo;
    if (hi === 0xffff_ffff && lo >= 0x8000_0000) return lo;
    const full = Number(v);
    return this.vm.regionAt(full) ? full : lo;
  }

  /** __wine_unix_call_dispatcher(handle, code, args) in the x64 convention. */
  unixCall64(cpu) {
    const m = this.m;
    const rsp = m.sp(cpu);
    const ret = m.ptr(rsp);
    const handle = Number(m.reg64(cpu, ECX));
    const code = m.reg(cpu, EDX);
    const args = Number(m.reg64(cpu, 8));
    let status = STATUS.NOT_IMPLEMENTED;
    if (handle === 0x1000) status = this.ntdllUnixCall(code, args);
    else if (handle === WIN32U_UNIXLIB && this.unix) {
      const t = this.threads.current;
      t.nest++;
      try {
        status = this.unix.win32uUnixCall(code, args);
      } finally {
        t.nest--;
      }
    } else if (handle === WINED3D_UNIXLIB) {
      // wined3d's WebGPU bridge (./d3d.mjs) serves i386 so far: x86-64
      // DirectDraw runs without 3D (WINE_D3D_CONFIG, d3d is null here).
      status = 0xc00000bb; // STATUS_NOT_SUPPORTED
    } else if (handle === GL_UNIXLIB) {
      // opengl32 without OpenGL, as on i386: it attaches, GL calls fail.
      if (code <= 2) status = STATUS.SUCCESS;
    } else if (handle === AUDIO_UNIXLIB) {
      // The audio driver (./audio.mjs) reads i386 structures so far: x86-64
      // programs see no audio device.
      if (!this.audioWarned) this.log('audio: no x86-64 audio driver yet');
      this.audioWarned = true;
    } else if (handle !== STUB_UNIXLIB) this.log(`unix call to unknown library ${hex(handle)} code ${code}`);
    m.setReg(cpu, EAX, status >>> 0);
    m.setSp(cpu, rsp + 8);
    return ret;
  }

  ntdllUnixCall(code, args) {
    // enum ntdll_unix_funcs (dlls/ntdll/unixlib.h)
    switch (code) {
      case 2: {
        // wine_dbg_write(const char *str, ULONG len)
        const str = this.ptr(args);
        const len = this.u32(args + this.ps);
        this.stderr(this.m.u8.slice(str, str + len));
        return len;
      }
      case 7: {
        // system_time_precise(LONGLONG *time)
        const now = BigInt(Math.floor((Date.now() + (performance.now() % 1)) * 10000)) + 116444736000000000n;
        this.w64(this.ptr(args), now);
        return 0;
      }
      default:
        this.fixme(`ntdll unix call ${code} not implemented`);
        return STATUS.NOT_IMPLEMENTED;
    }
  }
}

/** A 64-bit value from the wasm64 module as a number; sign-extended 32-bit values (pseudo-handles) read as their 32-bit forms. */
function exactArg64(v) {
  v = BigInt.asUintN(64, v);
  const lo = Number(v & 0xffff_ffffn);
  return v >> 32n === 0xffff_ffffn && lo >= 0x8000_0000 ? lo : Number(v);
}

export { EAX, ECX, EDX, EBX, ESP, EBP, ESI, EDI, MEM_PRIVATE };

/**
 * The directories of an in-memory file system (path -> bytes): every
 * ancestor of every file, kept up to date as files are added. Directories
 * stay when their files are deleted, as on Windows. Programs probe for
 * missing files often (SQLite checks for its journal on every transaction),
 * and each probe asks whether the path is a directory.
 */
function indexDirectories(files) {
  const dirs = new Set();
  const add = (path) => {
    for (let i = path.lastIndexOf('\\'); i > 0; i = path.lastIndexOf('\\', i - 1)) {
      const d = path.slice(0, i);
      if (dirs.has(d)) break;
      dirs.add(d);
    }
  };
  for (const k of files.keys()) add(k);
  const set = files.set.bind(files);
  files.set = (k, v) => {
    if (!files.has(k)) add(k);
    return set(k, v);
  };
  return dirs;
}

/** Records the case of each name in a DOS path in `map` (lower-case path -> name). */
export function recordCase(map, path) {
  const parts = path.split('\\');
  for (let i = 1; i < parts.length; i++) map.set(parts.slice(0, i + 1).join('\\').toLowerCase(), parts[i]);
}
