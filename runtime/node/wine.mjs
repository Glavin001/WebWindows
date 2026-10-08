#!/usr/bin/env node
// Runs a Windows program on translated Wine (Milestone 2).
//
//   node runtime/node/wine.mjs [--trace] program.exe [args...]
//
// GUI programs draw on a virtual 800x600 screen (runtime/wine/display.mjs):
//   --screenshot F    save it as a PNG when the program goes idle or exits
//   --folder          the program's directory as C:\app (games with data files)
//   --audio-out F     save what the program played as a WAV file (48 kHz stereo)
//   --run-for MS      ... or after this long (running: translating with an
//                     empty cache does not count)
//   --input SCRIPT    scripted input, timed from the first frame:
//                     "500:click 20,80; 900:text Hi; 1200:key Enter"
//                     (click/rclick/move X,Y; nudge DX,DY moves by that much;
//                     key CODE as in KeyboardEvent.code,
//                     keydown/keyup CODE to hold it;
//                     text types letters, digits and spaces)
//   --mem32           a 64-bit program on a 32-bit memory, below 4 GB, with the
//                     lowered Unix side (target/wine-unix64-m32), as in browsers
//                     without 64-bit WebAssembly memory; the default when this
//                     engine has none
//   --dir DIR         DIR is C:\app (as a folder chosen in the browser): the
//                     program runs from there (from C:\app if it lives
//                     elsewhere), and files it creates or changes there are
//                     written back to DIR when it exits
//   --file HOST[=DOS] put a host file in the guest's file system (default
//                     C:\<name>), e.g. a script or document the program reads
//
// Wine's PE DLLs come from WINE_BUILD (default /opt/wine-build) and its NLS
// files from WINE_SRC; translations are cached in target/wine-cache.
// 64-bit programs run on Wine's x86_64 DLLs from WINE_BUILD64 (default
// /opt/wine-build64, tools/wine/build.sh with ARCH=x86_64) on a 64-bit
// (memory64) memory, at their own addresses.
//
// When a run goes idle (with --screenshot), each thread's wait and EBP
// backtrace are printed. Diagnostics in the environment:
//   WWT_SYSCALL_COUNTS=1   the most frequent system calls, at idle
//   WWT_TRACE_CALLS=A,B    the first calls of these system calls, with
//                          arguments and status (WWT_TRACE_LIMIT, default 40)
//   WWT_FAST_LOG=1         fast mode's run-time translations
//   WWT_TRACE_FAULTS=N     the first N faults (default 10 when set) with
//                          where they happened and the frames above
//   WWT_THREAD_DUMP=MS     the scheduler's thread states after MS, and when
//                          nothing can run

import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, readdirSync, renameSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { basename, dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { Machine, GuestFault, hex, hasMemory64 } from '../runtime.mjs';
import { peArch } from '../pe.mjs';
import { WineHost } from '../wine/host.mjs';
import { loadWineUnix } from '../wine/unix.mjs';
import { Display } from '../wine/display.mjs';
import { D3DRecorder } from '../wine/d3d.mjs';
import { windowsKey, KEYEVENTF_KEYUP } from '../web/keys.mjs';

/** Thrown out of a wait to stop a GUI program that went idle (--screenshot). */
class ProgramIdle extends Error {}
import { FastTranslator, enableFastMode } from '../fastmode.mjs';
import { NATIVE_HEAP_FLAG, compileNativeHeap } from '../wine/heap.mjs';
import { NATIVE_STRINGS_DLLS, NATIVE_STRINGS_FLAG, compileNativeStrings } from '../wine/strings.mjs';
import { stdout, stderr } from './output.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
let wineBuild = process.env.WINE_BUILD ?? '/opt/wine-build';
const wineSrc = process.env.WINE_SRC ?? '/opt/wine-src/wine-11.0';
const cacheDir = join(root, 'target/wine-cache');
// Wine's address space; translations compile it into their memory checks.
const GUEST_LIMIT = 0x8000_0000;
// ntdll's heap as native WebAssembly (crates/wwt-heap), when it is built:
// ntdll is then translated with --native-heap. WWT_NATIVE_HEAP=0 keeps
// Wine's own heap.
const heapWasm = join(root, 'target/wasm32-unknown-unknown/release-wasm/wwt_heap.wasm');
const nativeHeap =
  process.env.WWT_NATIVE_HEAP !== '0' && existsSync(heapWasm) ? compileNativeHeap(readFileSync(heapWasm)) : null;
// String and locale functions as native WebAssembly (crates/wwt-strings),
// when built: the DLLs that have them are then translated with
// --native-strings. WWT_NATIVE_STRINGS=0 keeps Wine's.
const stringsWasm = join(root, 'target/wasm32-unknown-unknown/release-wasm/wwt_strings.wasm');
const nativeStrings =
  process.env.WWT_NATIVE_STRINGS !== '0' && existsSync(stringsWasm) ? compileNativeStrings(readFileSync(stringsWasm)) : null;

function wwt() {
  if (process.env.WWT) return process.env.WWT;
  return ['target/release/wwt', 'target/debug/wwt']
    .map((p) => join(root, p))
    .filter((p) => existsSync(p))
    .sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs)[0];
}

/** 64-bit programs: x86_64 Wine, on a 64-bit memory unless --mem32. */
let x64 = false;
let mem64 = false;
let mem32 = !hasMemory64();

/** Time spent translating images, which --run-for does not count. */
let translateMs = 0;

/** Translates an image with the CLI, caching by content hash. */
function translate(path, bytes) {
  mkdirSync(cacheDir, { recursive: true });
  // Keyed by the image and the translator build, so a rebuilt translator
  // never serves stale translations.
  const t = statSync(wwt());
  // Per-DLL translator flags, as in wine-bundle.mjs (TRANSLATE_FLAGS), and
  // WWT_TRANSLATE_FLAGS: extra `wwt translate` options, for A/B tests
  // (tools/bench/ab.mjs --wine). All are part of the cache key. With a
  // 32-bit memory the guest limit is a constant in the memory checks.
  const flags = { 'wined3d.dll': ['--no-smc-checks'], 'd3d8.dll': ['--no-smc-checks'], 'd3d9.dll': ['--no-smc-checks'] }[path.split('\\').pop().toLowerCase()] ?? [];
  const extra = [
    ...flags,
    ...(mem64 ? ['--mem64'] : ['--guest-limit-mb', String(GUEST_LIMIT >>> 20)]),
    ...(process.env.WWT_TRANSLATE_FLAGS ?? '').split(/\s+/).filter(Boolean),
  ];
  if (nativeHeap && !x64 && path.toLowerCase().endsWith('\\ntdll.dll')) extra.push(NATIVE_HEAP_FLAG);
  if (nativeStrings && !x64 && NATIVE_STRINGS_DLLS.includes(path.split('\\').pop().toLowerCase())) extra.push(NATIVE_STRINGS_FLAG);
  const hash = createHash('sha256').update(bytes).update(`${t.size}:${t.mtimeMs}:${extra.join(' ')}`).digest('hex').slice(0, 16);
  const out = join(cacheDir, `${path.split('\\').pop()}-${hash}.wasm`);
  if (!existsSync(out)) {
    // Several runners share the cache: write under a per-process name and
    // rename, so a reader never sees a partial file.
    const tmp = join(cacheDir, `${hash}.${process.pid}`);
    writeFileSync(`${tmp}.bin`, bytes);
    const t0 = performance.now();
    execFileSync(wwt(), ['translate', `${tmp}.bin`, '-o', `${tmp}.wasm`, ...extra], {
      stdio: ['ignore', 'ignore', 'inherit'],
    });
    translateMs += performance.now() - t0;
    renameSync(`${tmp}.wasm`, out);
    rmSync(`${tmp}.bin`);
  }
  return readFileSync(out);
}

const args = process.argv.slice(2);
let trace = false;
let unixTrace = false;
let screenshot = null;
let audioOut = null;
let folder = false;
let runFor = Infinity;
let script = [];
let appDir = null;
const extraFiles = [];
// --d3d-record FILE: wined3d's WebGPU command stream, recorded (no GPU in Node).
let d3dRecord = null;
// Wine's Unix side compiled with Emscripten (native/wine-unix): on when built,
// unless --no-unix. 64-bit programs use its wasm64 build (ARCH=x86_64).
let unixDir = join(root, 'target/wine-unix');
let useUnix = null;
while (args[0]?.startsWith('--')) {
  const a = args.shift();
  if (a === '--trace') trace = true;
  else if (a === '--trace-unix') unixTrace = args.shift();
  else if (a === '--no-unix') useUnix = false;
  else if (a === '--unix') useUnix = true;
  // GUI programs: when the program waits with nothing left to wake it (no
  // timer, no input), save the screen as a PNG and stop.
  else if (a === '--screenshot') screenshot = args.shift();
  else if (a === '--audio-out') audioOut = args.shift();
  // The program's whole directory as C:\app, as the page's folder picker
  // gives it (games with their data files).
  else if (a === '--folder') folder = true;
  // ... or once it has run this long (programs with timers never go idle).
  else if (a === '--run-for') runFor = Number(args.shift());
  else if (a === '--input') script = parseInput(args.shift());
  else if (a === '--dir') appDir = resolve(args.shift());
  else if (a === '--mem32') mem32 = true;
  else if (a === '--file') {
    const [host, dos] = args.shift().split('=');
    extraFiles.push([host, (dos ?? `C:\\${basename(host)}`).toLowerCase()]);
  } else if (a === '--d3d-record') d3dRecord = args.shift();
}

/** "ms:action args; ..." -> [{at, push(display)}], in time order. */
function parseInput(text) {
  const steps = [];
  for (const part of text.split(';').map((p) => p.trim()).filter(Boolean)) {
    const m = part.match(/^(\d+):\s*(\w+)\s*(.*)$/);
    if (!m) throw new Error(`--input: cannot parse "${part}"`);
    const [, at, action, arg] = m;
    const xy = () => arg.split(',').map(Number);
    const tap = (d, k) => {
      d.key(k.vk, k.scan, k.flags);
      d.key(k.vk, k.scan, k.flags | KEYEVENTF_KEYUP);
    };
    const shift = windowsKey('ShiftLeft');
    let push;
    if (action === 'move') push = (d) => d.mouse(...xy());
    // A movement, as from a locked pointer (games that read relative motion).
    else if (action === 'nudge') push = (d) => d.mouse(...xy(), Display.RELATIVE);
    else if (action === 'click' || action === 'rclick') {
      const [down, up] = action === 'click' ? [0x2, 0x4] : [0x8, 0x10];
      push = (d) => {
        d.mouse(...xy());
        d.mouse(...xy(), down);
        d.mouse(...xy(), up);
      };
    } else if (action === 'key' || action === 'keydown' || action === 'keyup') {
      // keydown/keyup: held keys, for games that read the key state each frame.
      const k = windowsKey(arg);
      if (!k) throw new Error(`--input: unknown key ${arg}`);
      if (action === 'key') push = (d) => tap(d, k);
      else push = (d) => d.key(k.vk, k.scan, k.flags | (action === 'keyup' ? KEYEVENTF_KEYUP : 0));
    } else if (action === 'text') {
      push = (d) => {
        for (const c of arg) {
          const code = c === ' ' ? 'Space' : /\d/.test(c) ? `Digit${c}` : `Key${c.toUpperCase()}`;
          const k = windowsKey(code);
          if (!k) throw new Error(`--input: cannot type "${c}"`);
          const upper = c !== c.toLowerCase();
          if (upper) d.key(shift.vk, shift.scan, shift.flags);
          tap(d, k);
          if (upper) d.key(shift.vk, shift.scan, shift.flags | KEYEVENTF_KEYUP);
        }
      };
    } else throw new Error(`--input: unknown action ${action}`);
    steps.push({ at: Number(at), push });
  }
  return steps.sort((a, b) => a.at - b.at);
}
const exe = args.shift();
if (!exe) {
  console.error('usage: wine.mjs [--trace] [--trace-unix CHANNELS] [--no-unix] [--mem32] [--folder] [--dir DIR] [--audio-out F.wav] [--screenshot F [--run-for MS]] program.exe [args...]');
  process.exit(2);
}

x64 = peArch(readFileSync(exe)) === 'x64';
mem64 = x64 && !mem32;
if (x64) {
  wineBuild = process.env.WINE_BUILD64 ?? '/opt/wine-build64';
  unixDir = join(root, mem64 ? 'target/wine-unix64' : 'target/wine-unix64-m32');
}
useUnix ??= existsSync(join(unixDir, 'wine_unix.mjs'));
const peDir = x64 ? 'x86_64-windows' : 'i386-windows';

// The virtual C: drive.
const files = new Map();
const sys32 = 'c:\\windows\\system32';
for (const d of readdirSync(join(wineBuild, 'dlls'))) {
  for (const f of [join(wineBuild, 'dlls', d, peDir, `${d}.dll`), join(wineBuild, 'dlls', d, peDir, d)]) {
    if (existsSync(f) && statSync(f).isFile()) files.set(`${sys32}\\${basename(f).toLowerCase()}`, readFileSync(f));
  }
}
for (const f of readdirSync(join(wineSrc, 'nls'))) {
  if (f.endsWith('.nls')) files.set(`${sys32}\\${f.toLowerCase()}`, readFileSync(join(wineSrc, 'nls', f)));
}
files.set('c:\\windows\\globalization\\sorting\\sortdefault.nls', files.get(`${sys32}\\sortdefault.nls`));
// The program: at C:\, or with --dir in C:\app, next to the folder's files
// (--folder: the program's own directory, left as it is).
const appFiles = new Map(); // DOS path -> [host path, original bytes]
let exeWin = `C:\\${basename(exe)}`;
const syncDir = appDir;
if (folder && !appDir) appDir = dirname(resolve(exe));
if (appDir) {
  const walk = (dir, rel) => {
    for (const e of readdirSync(dir, { withFileTypes: true })) {
      const r = rel ? `${rel}\\${e.name}` : e.name;
      if (e.isDirectory()) walk(join(dir, e.name), r);
      else if (e.isFile()) {
        const bytes = readFileSync(join(dir, e.name));
        // A copy: the program's writes change the file's bytes in place.
        appFiles.set(`c:\\app\\${r.toLowerCase()}`, [join(dir, e.name), Buffer.from(bytes)]);
        files.set(`c:\\app\\${r.toLowerCase()}`, bytes);
      }
    }
  };
  walk(appDir, '');
  const rel = relative(appDir, resolve(exe));
  exeWin = `C:\\app\\${rel.startsWith('..') ? basename(exe) : rel.replaceAll('/', '\\')}`;
}
const exeDos = exeWin.toLowerCase();
files.set(exeDos, readFileSync(exe));
for (const [host, dos] of extraFiles) files.set(dos, readFileSync(host));

/** --dir: writes the files the program created or changed in C:\app back to the folder. */
function syncBack() {
  if (!syncDir) return;
  for (const [dos, bytes] of files) {
    if (!dos.startsWith('c:\\app\\') || dos === exeDos) continue;
    const old = appFiles.get(dos);
    if (old && Buffer.compare(old[1], Buffer.from(bytes.buffer, bytes.byteOffset, bytes.length)) === 0) continue;
    const host = old ? old[0] : join(syncDir, ...dos.slice('c:\\app\\'.length).split('\\'));
    mkdirSync(dirname(host), { recursive: true });
    writeFileSync(host, bytes);
  }
}

mkdirSync(cacheDir, { recursive: true });
const abi = JSON.parse(execFileSync(wwt(), ['abi']).toString());
const kernelPath = join(cacheDir, `kernel.${process.pid}.wasm`);
execFileSync(wwt(), ['kernel', '-o', kernelPath, ...(mem64 ? ['--code64'] : [])]);
const kernel = readFileSync(kernelPath);
rmSync(kernelPath);
const layout = useUnix ? JSON.parse(readFileSync(join(unixDir, 'wine_unix.json'), 'utf8')) : null;
const machine = new Machine({
  abi,
  kernel,
  // x86_64 Wine's DLLs load at 0x1_7000_0000 and up; on a 32-bit memory the
  // host moves them into the guest region (2 GB, as for i386).
  ...(mem64 ? { arch: 'x64', mem64: true, guestLimit: 0x2_0000_0000 } : { arch: x64 ? 'x64' : 'x86', guestLimit: GUEST_LIMIT }),
  ...(layout && { nativeSize: layout.nativeSize, extraSize: layout.extraSize }),
  log: trace ? (s) => stderr(`[machine] ${s}\n`) : undefined,
});
await machine.init();
let unix = null;
// Scripted input is timed from the first frame on the screen.
let firstFrame = null;
const display = new Display({
  onChange: () => (firstFrame ??= performance.now()),
  inputSource: (d) => {
    while (script.length && firstFrame !== null && performance.now() - firstFrame >= script[0].at) {
      if (trace) stderr(`[input] step at ${script[0].at} ms\n`);
      script.shift().push(d);
    }
  },
});
const sleep = (ms) => Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
// Blocks the thread; a wait nothing can end means the program is idle.
const started = performance.now();
// How long the program has run, translation (an empty cache) not counted.
const ranFor = () => performance.now() - started - translateMs;
const wait = (ms) => {
  if (display.hasInput()) return 1;
  if (screenshot && ranFor() > runFor) throw new ProgramIdle();
  // Scripted input still to come wakes the program when it is due.
  const due = script.length && firstFrame !== null ? Math.max(0, firstFrame + script[0].at - performance.now()) : -1;
  if (due >= 0 && (ms < 0 || due < ms)) {
    sleep(due);
    return display.hasInput() ? 1 : 0;
  }
  if (ms >= 0) {
    sleep(ms);
    return display.hasInput() ? 1 : 0;
  }
  if (screenshot) throw new ProgramIdle();
  return -1;
};
if (useUnix) {
  unix = await loadWineUnix(machine, {
    factory: async () => (await import(join(unixDir, 'wine_unix.mjs'))).default,
    layout,
    // wineserver's case tables, and Wine's own fonts for win32u (FreeType).
    dataFiles: new Map([
      ['/wine/share/wine/nls/l_intl.nls', readFileSync(join(wineSrc, 'nls/l_intl.nls'))],
      ...readdirSync(join(wineSrc, 'fonts'))
        .filter((f) => f.endsWith('.ttf'))
        .map((f) => [`/wine/share/wine/fonts/${f}`, readFileSync(join(wineSrc, 'fonts', f))]),
      // Bitmap fonts (System, MS Sans Serif, ...) that Wine's build generates.
      ...(existsSync(join(wineBuild, 'fonts')) ? readdirSync(join(wineBuild, 'fonts')) : [])
        .filter((f) => f.endsWith('.fon'))
        .map((f) => [`/wine/share/wine/fonts/${f}`, readFileSync(join(wineBuild, 'fonts', f))]),
    ]),
    display,
    wait,
    stderr: (s) => stderr(s),
    win32uNames: JSON.parse(readFileSync(join(unixDir, 'win32u_syscalls.json'), 'utf8')),
    ntCalls: JSON.parse(readFileSync(join(unixDir, 'nt_calls.json'), 'utf8')),
  });
  if (unixTrace) unix.setTrace(unixTrace);
}
// Fast mode: code the ahead-of-time pass missed is translated when reached.
const tw = join(root, 'target/wasm32-unknown-unknown/release-wasm/wwt_wasm.wasm');
if (existsSync(tw)) {
  enableFastMode(machine, await FastTranslator.load(readFileSync(tw)), {
    log: trace || process.env.WWT_FAST_LOG ? (s) => stderr(`[fast] ${s}\n`) : undefined,
  });
}
const d3d = d3dRecord ? new D3DRecorder() : null;
const saveRecording = () => {
  if (!d3d) return;
  writeFileSync(d3dRecord, d3d.bytes());
  stderr(`recorded ${d3d.batches.length} Direct3D batches in ${d3dRecord}\n`);
};
/** What the program plays, kept for --audio-out (float stereo at 48 kHz). */
const audioCapture = {
  rate: 48000,
  chunks: [],
  write(src) {
    this.chunks.push(src);
  },
  wav() {
    const n = this.chunks.reduce((a, c) => a + c.length, 0);
    const out = Buffer.alloc(44 + n * 2);
    out.write('RIFF', 0);
    out.writeUInt32LE(36 + n * 2, 4);
    out.write('WAVEfmt ', 8);
    out.writeUInt32LE(16, 16);
    out.writeUInt16LE(1, 20);
    out.writeUInt16LE(2, 22);
    out.writeUInt32LE(this.rate, 24);
    out.writeUInt32LE(this.rate * 4, 28);
    out.writeUInt16LE(4, 32);
    out.writeUInt16LE(16, 34);
    out.write('data', 36);
    out.writeUInt32LE(n * 2, 40);
    let at = 44;
    for (const c of this.chunks) for (const v of c) out.writeInt16LE(Math.max(-32768, Math.min(32767, Math.round(v * 32767))), (at += 2) - 2);
    return out;
  },
};
const host = new WineHost(machine, {
  translate,
  d3d,
  files,
  argv: [exeWin, ...args],
  exePath: exeWin,
  stdout: (b) => stdout(b),
  stderr: (b) => stderr(b),
  trace,
  unix,
  debug: process.env.WINEDEBUG ?? '',
  // WWT_LOCALE: the user's default locale, an LCID (e.g. 0x40e for hu-HU).
  ...(process.env.WWT_LOCALE && { locale: Number(process.env.WWT_LOCALE) }),
  // i386 code's (see translate above).
  nativeHeap: x64 ? null : nativeHeap,
  aliasThunks: process.env.WWT_THUNK_ALIAS !== '0',
  audioSink: audioOut ? audioCapture : null,
  nativeStrings: x64 ? null : nativeStrings,
});
host.boot(`${sys32}\\ntdll.dll`, exeDos);
// A program that never waits (a game's busy frame loop) still stops on time.
if (screenshot) host.threads.onSlice = () => {
  if (ranFor() > runFor) throw new ProgramIdle();
};
await host.startClock();
const r = host.run();
syncBack();
if (audioOut) writeFileSync(audioOut, audioCapture.wav());
if (host.unimplemented.size) {
  stderr(`unimplemented syscalls: ${[...host.unimplemented.keys()].join(', ')}\n`);
}
saveRecording();
if (r.error instanceof ProgramIdle || r.error?.cause instanceof ProgramIdle) {
  writeFileSync(screenshot, await display.png());
  stderr(`idle; screenshot in ${screenshot}\n`);
  if (process.env.WWT_SYSCALL_COUNTS) stderr(`  system calls: ${[...host.counts].sort((x, y) => y[1] - x[1]).slice(0, 20).map(([n, c]) => `${n} ${c}`).join(', ')}\n`);
  // What each thread was waiting in, to tell a game waiting for input from
  // one stuck waiting for itself.
  for (const t of host.threads.threads) {
    if (t.state === 'dead') continue;
    const at = t.pending?.ret ? ` from ${host.describeAddress?.(t.pending.ret) ?? hex(t.pending.ret)}` : '';
    stderr(`  thread ${hex(t.tid)}: ${t.state}${t.suspend ? ` (suspended ${t.suspend})` : ""}${t.nest ? ` (nest ${t.nest})` : ""}${t.pending ? ` in ${t.pending.name}${at}` : ''}; frames ${host.backtrace(t).join(' < ')}\n`);
  }
  process.exit(0);
}
if (r.error) {
  stderr(`\n*** ${r.error instanceof GuestFault ? 'guest fault' : 'error'}: ${r.error.message}\n`);
  if (!(r.error instanceof GuestFault)) stderr(r.error.stack + '\n');
  process.exit(128);
}
if (screenshot) {
  writeFileSync(screenshot, await display.png());
  stderr(`exited; screenshot in ${screenshot}\n`);
}
process.exit(r.exitCode ?? 0);
