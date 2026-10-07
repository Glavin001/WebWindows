// Wine's Unix side compiled with Emscripten (native/wine-unix, Milestone 4):
// wineserver in-process, plus the parts of ntdll's Unix side that talk to it
// (synchronization objects, waits, atoms, the registry).
//
// The module shares the machine's memory: its data, stack and heap sit
// above the runtime's native area, at the base it was linked for, and guest
// pointers passed to it are plain addresses. System calls it implements
// (nt_calls.json) are routed to it by the Wine host; handle-based calls go
// to it for handles the server created.
//
// 64-bit Wine uses a wasm64 build (native/wine-unix with ARCH=x86_64):
// pointers cross into it as BigInts, and arguments are 64-bit slots.

import { hex } from '../runtime.mjs';

/**
 * Loads the module into `machine`'s memory.
 * @param {import('../runtime.mjs').Machine} machine
 * @param {object} opts
 * @param {() => Promise<Function>} opts.factory  imports wine_unix.mjs and returns its default export
 * @param {object} opts.layout  wine_unix.json from the build
 * @param {Map<string, Uint8Array>} opts.dataFiles  /wine/share/wine/... paths -> contents
 * @param {(ms: number) => number} [opts.wait]  blocks up to ms (-1: no limit); returns 1 when
 *        input is waiting, 0 when the time passed, -1 if nothing could ever wake the thread
 * @param {import('./display.mjs').Display} [opts.display]  the screen for the browser display driver
 * @param {string[]} opts.ntCalls  nt_calls.json from the build: the NT calls it implements, by index
 */
export async function loadWineUnix(machine, opts) {
  const { layout } = opts;
  if (machine.guestLimit !== layout.guestLimit || machine.nativeEnd !== layout.globalBase) {
    throw new Error(`wine_unix was linked at ${hex(layout.globalBase)}; the machine's native area ends at ${hex(machine.nativeEnd)}`);
  }
  opts.display?.attach(machine);
  const create = await opts.factory();
  const stderrLine = [];
  // The Wine host attaches itself once it exists (WineUnix.attach); calls
  // from the module into the host go through it.
  let unix = null;
  const M = await create({
    wasmMemory: machine.memory,
    hostWait: opts.wait ?? defaultWait,
    display: opts.display,
    hostNtCall: (name, args) => unix.host.hostNtCall(name, args),
    hostUserCallback: (id, args, len, retPtr, retLen) => unix.host.userCallback(id, args, len, retPtr, retLen),
    print: (s) => machine.log(`[unix] ${s}`),
    printErr: (s) => (opts.stderr ? opts.stderr(s + '\n') : stderrLine.push(s)),
    locateFile: opts.locateFile,
    wasmBinary: opts.wasmBinary,
    preRun: [
      (mod) => {
        for (const [path, bytes] of opts.dataFiles ?? []) {
          mod.FS.mkdirTree(path.replace(/\/[^/]*$/, ''));
          mod.FS.writeFile(path, bytes);
        }
      },
    ],
  });
  unix = new WineUnix(machine, M, opts.win32uNames ?? {}, opts.ntCalls, layout.arch === 'x86_64');
  return unix;
}

/** Blocks the thread without a browser event loop (Node, a worker). */
function defaultWait(ms) {
  if (ms < 0) return -1;
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
  return 0;
}

// System calls that stay in the JavaScript host even though the module
// exports them: sections and image mapping need the host's loader and
// virtual memory, and time is cheaper in JavaScript.
const KEEP_IN_HOST = new Set([
  'NtCreateSection',
  'NtCreateSectionEx',
  'NtOpenSection',
  'NtQueryPerformanceCounter',
  'NtQuerySystemTime',
  'NtSetSystemTime',
  'NtQueryTimerResolution',
  'NtSetTimerResolution',
]);

/** The handle the host gives win32u.dll for its Unix calls. */
export const WIN32U_UNIXLIB = 0x2000;

// Calls that take a handle first: they go to the module only for handles
// the server created (the host's own handles are in a separate range).
export const HANDLE_ROUTED = new Set(['NtClose', 'NtDuplicateObject', 'NtWaitForSingleObject']);

export class WineUnix {
  constructor(machine, M, win32uNames, ntCalls, wide = false) {
    this.m = machine;
    this.M = M;
    this.host = null;
    /** A wasm64 module: pointers are BigInts, argument slots 8 bytes. */
    this.wide = wide;
    this.slot = wide ? 8 : 4;
    /** win32u system call number -> name */
    this.win32uNames = new Map(Object.entries(win32uNames).map(([id, name]) => [Number(id), name]));
    // Argument slots for calls into the module (wasm_nt_call and win32u).
    this.args = Number(M._malloc(32 * this.slot));
    /** name -> function(slots) for each routed NT call */
    this.syscalls = new Map();
    (ntCalls ?? []).forEach((name, index) => {
      if (!KEEP_IN_HOST.has(name)) this.syscalls.set(name, (slots) => this.ntCall(index, slots));
    });
  }

  /** A pointer for the module (a BigInt on wasm64). */
  p(addr) {
    return this.wide ? BigInt(addr) : addr;
  }

  /** Writes argument slots (numbers or BigInts) to the module's argument area. */
  putSlots(slots) {
    const dv = this.m.dv;
    slots.forEach((v, i) => {
      if (this.wide) dv.setBigUint64(this.args + i * 8, BigInt.asUintN(64, BigInt(v)), true);
      else dv.setUint32(this.args + i * 4, Number(v) >>> 0, true);
    });
  }

  /** Runs the routed NT call `index` (gen-ntcalls.py) with the guest's argument slots. */
  ntCall(index, slots) {
    this.putSlots(slots);
    return this.M._wasm_nt_call(index, this.p(this.args)) >>> 0;
  }

  /** Starts the server and registers the process (after the host built the TEB and PEB). */
  initProcess(teb, peb, pid = 0x20) {
    const status = this.M._wasm_init_process(this.p(teb), this.p(peb), pid) >>> 0;
    if (status) throw new Error(`wineserver process init failed: ${hex(status)}`);
  }

  attach(host) {
    this.host = host;
  }

  /** A win32u system call (number 0x1000 and up) with its arguments at `args` on the guest stack (i386). */
  win32uSyscall(id, args) {
    return this.M._wasm_win32u_syscall(id, args) >>> 0;
  }

  /**
   * x86-64: a win32u system call with its argument slots (the four register
   * arguments, then the stack ones). Returns the 64-bit result.
   */
  win32uSyscall64(id, slots) {
    this.putSlots(slots);
    return BigInt.asUintN(64, BigInt(this.M._wasm_win32u_syscall(id, this.p(this.args))));
  }

  /** win32u's Unix calls (__wine_unix_call from win32u.dll). */
  win32uUnixCall(code, args) {
    return this.M._wasm_win32u_unix_call(code, this.p(args)) >>> 0;
  }

  /** Wine debug channels to trace: "all" or "chan1,chan2" (errors always print). */
  setTrace(channels) {
    const bytes = new TextEncoder().encode(channels + '\0');
    const p = Number(this.M._malloc(bytes.length));
    this.m.u8.set(bytes, p);
    this.M._wasm_set_trace(this.p(p));
    this.M._free(p);
  }
}
