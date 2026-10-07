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

import layout from './layout.json' with { type: 'json' };

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
   * @param {(path: string, bytes: Uint8Array, base: number) => Uint8Array} opts.translate
   *        returns the translated module for an image file
   * @param {Map<string, Uint8Array>} opts.files  DOS paths (c:/...) -> contents
   */
  constructor(machine, opts) {
    this.m = machine;
    this.translate = opts.translate;
    this.files = opts.files;
    this.stdout = opts.stdout ?? (() => {});
    this.stderr = opts.stderr ?? (() => {});
    this.trace = opts.trace ?? false;
    this.vm = new VirtualMemory(machine, 0x10000, machine.thunkBase);
    this.handles = new Map();
    this.nextHandle = 0x20;
    this.images = new Map(); // base -> {path, info}
    this.modulesByPath = new Map();
    this.unimplemented = new Map();
    this.counts = new Map();
    this.argv = opts.argv;
    this.exePath = opts.exePath; // DOS path, e.g. c:\hello.exe
    this.env = opts.env ?? {};
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
    const info = parsePe(bytes);
    const base = info.imageBase;
    if (!this.vm.reserve(base, info.sizeOfImage, { type: MEM_IMAGE, prot: 0x80, name: dosPath })) {
      return { status: STATUS.CONFLICTING_ADDRESSES };
    }
    this.vm.commit(base, info.sizeOfImage, PAGE_EXECUTE_READ);
    // The translation: module bytes, or an already compiled module (Wine's
    // DLLs are translated ahead of time and compiled by the host).
    const wasm = this.translate(dosPath, bytes, base);
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

    // The main executable, mapped by the "Unix side" as Wine does.
    const exe = this.mapImageFile(exeDosPath, this.fileAt(exeDosPath));
    if (exe.status) throw new Error(`cannot map ${exeDosPath} at its base`);
    this.exe = exe;

    // PEB, TEB, process parameters.
    this.peb = this.alloc(0x1000, PAGE_READWRITE, 'PEB');
    this.buildPeb(exe);
    this.teb = this.alloc(0x2000, PAGE_READWRITE, 'TEB');
    this.cpu = m.newCpu();
    this.buildTeb(this.teb, exe.info);
    m.dv.setUint16(this.cpu + m.abi.cpu.FPU_CW, 0x27f, true);
    // Windows starts threads with IF set.
    m.u32[(this.cpu + m.abi.cpu.EFLAGS_SYS) >>> 2] = 0x200;
    const sel = this.cpu + m.abi.cpu.SEG_SEL;
    [0x2b, 0x23, 0x2b, 0x2b, 0x53, 0x2b].forEach((v, i) => m.dv.setUint16(sel + i * 2, v, true));
    m.u32[(this.cpu + m.abi.cpu.FS_BASE) >>> 2] = this.teb;

    // Initial context for LdrInitializeThunk, as signal_i386.c builds it.
    const C = L.CONTEXT;
    const stackTop = this.u32(this.teb + L.TEB['Tib.StackBase']);
    const ctx = ((stackTop - 16) & ~3) - C.__size;
    m.u8.fill(0, ctx, ctx + C.__size);
    this.w32(ctx + C.ContextFlags, 0x1003f); // CONTEXT_ALL for i386
    this.w32(ctx + C.SegCs, 0x23);
    this.w32(ctx + C.SegDs, 0x2b);
    this.w32(ctx + C.SegEs, 0x2b);
    this.w32(ctx + C.SegFs, 0x53);
    this.w32(ctx + C.SegSs, 0x2b);
    this.w32(ctx + C.EFlags, 0x202);
    this.w32(ctx + C.Eax, exe.base + exe.info.entryRva);
    this.w32(ctx + C.Ebx, this.peb);
    this.w32(ctx + C.Esp, stackTop - 16);
    this.w32(ctx + C.Eip, ex('RtlUserThreadStart'));
    this.w16(ctx + C.FloatSave, 0x27f);
    this.w16(ctx + C.ExtendedRegisters, 0x27f);
    this.w32(ctx + C.ExtendedRegisters + 24, 0x1f80);
    let sp = ctx;
    for (const v of [0, 0, 0, ctx, 0xdeadbabe]) {
      sp -= 4;
      this.w32(sp, v);
    }
    m.setReg(this.cpu, ESP, sp);
    this.entry = ex('LdrInitializeThunk');
  }

  run() {
    try {
      this.m.run(this.cpu, this.entry);
      return { exitCode: null, error: new Error('initial thread returned') };
    } catch (e) {
      if (e instanceof ProcessExit) return { exitCode: e.exitCode };
      return { exitCode: null, error: e };
    }
  }

  initSharedData() {
    const K = L.KUSER_SHARED_DATA;
    const b = USER_SHARED_DATA;
    this.w32(b + K.TickCountMultiplier, 0x0fa00000);
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
    const curdir = 'C:\\';
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

  buildTeb(teb, exeInfo) {
    const T = L.TEB;
    const stackSize = Math.max(exeInfo.stackReserve || 0x100000, 0x100000);
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
    const name = this.syscallNames.get(id) ?? `syscall_${id.toString(16)}`;
    const argBase = esp + 8; // [esp] -> stub, [esp+4] -> caller
    const a = (i) => this.u32(argBase + i * 4);
    this.counts.set(name, (this.counts.get(name) ?? 0) + 1);
    const impl = SYSCALLS[name];
    let status;
    if (!impl) {
      if (!this.unimplemented.has(name)) this.log(`UNIMPLEMENTED ${name}`);
      this.unimplemented.set(name, (this.unimplemented.get(name) ?? 0) + 1);
      status = STATUS.NOT_IMPLEMENTED;
    } else {
      status = impl.call(this, a, cpu, argBase);
    }
    if (status && typeof status === 'object' && status.jump !== undefined) {
      if (this.trace) this.log(`${name} -> jump ${hex(status.jump)}`);
      return status.jump;
    }
    if (this.trace) this.log(`${name}(${[0, 1, 2, 3].map((i) => hex(a(i))).join(', ')}) = ${hex(status >>> 0)}`);
    m.setReg(cpu, EAX, status >>> 0);
    m.setReg(cpu, ESP, esp + 4);
    return ret;
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
