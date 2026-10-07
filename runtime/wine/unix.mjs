// Wine's Unix side compiled with Emscripten (native/wine-unix, Milestone 4):
// wineserver in-process, plus the parts of ntdll's Unix side that talk to it
// (synchronization objects, waits, atoms, the registry).
//
// The module shares the machine's memory: its data, stack and heap sit
// above the runtime's native area, at the base it was linked for, and guest
// pointers passed to it are plain addresses. System calls whose names it
// exports are routed to it by the Wine host; handle-based calls go to it
// for handles the server created.

import { hex } from '../runtime.mjs';

/**
 * Loads the module into `machine`'s memory.
 * @param {import('../runtime.mjs').Machine} machine
 * @param {object} opts
 * @param {() => Promise<Function>} opts.factory  imports wine_unix.mjs and returns its default export
 * @param {object} opts.layout  wine_unix.json from the build
 * @param {Map<string, Uint8Array>} opts.dataFiles  /wine/share/wine/... paths -> contents
 * @param {(ms: number) => number} [opts.wait]  blocks up to ms (-1: no limit); returns -1 if
 *        nothing could ever wake the thread
 */
export async function loadWineUnix(machine, opts) {
  const { layout } = opts;
  if (machine.guestLimit !== layout.guestLimit || machine.nativeEnd !== layout.globalBase) {
    throw new Error(`wine_unix was linked at ${hex(layout.globalBase)}; the machine's native area ends at ${hex(machine.nativeEnd)}`);
  }
  const create = await opts.factory();
  const stderrLine = [];
  const M = await create({
    wasmMemory: machine.memory,
    hostWait: opts.wait ?? defaultWait,
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
  return new WineUnix(machine, M);
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

// Calls that take a handle first: they go to the module only for handles
// the server created (the host's own handles are in a separate range).
export const HANDLE_ROUTED = new Set(['NtClose', 'NtDuplicateObject', 'NtWaitForSingleObject']);

export class WineUnix {
  constructor(machine, M) {
    this.m = machine;
    this.M = M;
    /** name -> function(args...) for each routed NT call */
    this.syscalls = new Map();
    for (const key of Object.keys(M)) {
      const name = key.slice(1);
      if (key.startsWith('_Nt') && typeof M[key] === 'function' && !KEEP_IN_HOST.has(name)) this.syscalls.set(name, M[key]);
    }
  }

  /** Starts the server and registers the process (after the host built the TEB and PEB). */
  initProcess(teb, peb, pid = 0x20) {
    const status = this.M._wasm_init_process(teb, peb, pid) >>> 0;
    if (status) throw new Error(`wineserver process init failed: ${hex(status)}`);
  }

  setTrace(on) {
    this.M._wasm_set_trace(on ? 1 : 0);
  }
}
