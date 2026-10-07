#!/usr/bin/env node
// Runs a Windows program on translated Wine (Milestone 2).
//
//   node runtime/node/wine.mjs [--trace] program.exe [args...]
//
// Wine's PE DLLs come from WINE_BUILD (default /opt/wine-build) and its NLS
// files from WINE_SRC; translations are cached in target/wine-cache.

import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, readdirSync, renameSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { Machine, GuestFault, hex } from '../runtime.mjs';
import { WineHost } from '../wine/host.mjs';
import { loadWineUnix } from '../wine/unix.mjs';
import { Display } from '../wine/display.mjs';

/** Thrown out of a wait to stop a GUI program that went idle (--screenshot). */
class ProgramIdle extends Error {}
import { FastTranslator, enableFastMode } from '../fastmode.mjs';
import { stdout, stderr } from './output.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const wineBuild = process.env.WINE_BUILD ?? '/opt/wine-build';
const wineSrc = process.env.WINE_SRC ?? '/opt/wine-src/wine-11.0';
const cacheDir = join(root, 'target/wine-cache');

function wwt() {
  return ['target/release/wwt', 'target/debug/wwt']
    .map((p) => join(root, p))
    .filter((p) => existsSync(p))
    .sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs)[0];
}

/** Translates an image with the CLI, caching by content hash. */
function translate(path, bytes) {
  mkdirSync(cacheDir, { recursive: true });
  // Keyed by the image and the translator build, so a rebuilt translator
  // never serves stale translations.
  const t = statSync(wwt());
  const hash = createHash('sha256').update(bytes).update(`${t.size}:${t.mtimeMs}`).digest('hex').slice(0, 16);
  const out = join(cacheDir, `${path.split('\\').pop()}-${hash}.wasm`);
  if (!existsSync(out)) {
    // Several runners share the cache: write under a per-process name and
    // rename, so a reader never sees a partial file.
    const tmp = join(cacheDir, `${hash}.${process.pid}`);
    writeFileSync(`${tmp}.bin`, bytes);
    execFileSync(wwt(), ['translate', `${tmp}.bin`, '-o', `${tmp}.wasm`], { stdio: ['ignore', 'ignore', 'inherit'] });
    renameSync(`${tmp}.wasm`, out);
    rmSync(`${tmp}.bin`);
  }
  return readFileSync(out);
}

const args = process.argv.slice(2);
let trace = false;
let unixTrace = false;
let screenshot = null;
let runFor = Infinity;
// Wine's Unix side compiled with Emscripten (native/wine-unix): on when built,
// unless --no-unix.
const unixDir = join(root, 'target/wine-unix');
let useUnix = existsSync(join(unixDir, 'wine_unix.mjs'));
while (args[0]?.startsWith('--')) {
  const a = args.shift();
  if (a === '--trace') trace = true;
  else if (a === '--trace-unix') unixTrace = args.shift();
  else if (a === '--no-unix') useUnix = false;
  else if (a === '--unix') useUnix = true;
  // GUI programs: when the program waits with nothing left to wake it (no
  // timer, no input), save the screen as a PNG and stop.
  else if (a === '--screenshot') screenshot = args.shift();
  // ... or once it has run this long (programs with timers never go idle).
  else if (a === '--run-for') runFor = Number(args.shift());
}
const exe = args.shift();
if (!exe) {
  console.error('usage: wine.mjs [--trace] [--trace-unix CHANNELS] [--no-unix] [--screenshot F [--run-for MS]] program.exe [args...]');
  process.exit(2);
}

// The virtual C: drive.
const files = new Map();
const sys32 = 'c:\\windows\\system32';
for (const d of readdirSync(join(wineBuild, 'dlls'))) {
  for (const f of [join(wineBuild, 'dlls', d, 'i386-windows', `${d}.dll`), join(wineBuild, 'dlls', d, 'i386-windows', d)]) {
    if (existsSync(f) && statSync(f).isFile()) files.set(`${sys32}\\${basename(f).toLowerCase()}`, readFileSync(f));
  }
}
for (const f of readdirSync(join(wineSrc, 'nls'))) {
  if (f.endsWith('.nls')) files.set(`${sys32}\\${f.toLowerCase()}`, readFileSync(join(wineSrc, 'nls', f)));
}
files.set('c:\\windows\\globalization\\sorting\\sortdefault.nls', files.get(`${sys32}\\sortdefault.nls`));
const exeDos = `c:\\${basename(exe).toLowerCase()}`;
files.set(exeDos, readFileSync(exe));

mkdirSync(cacheDir, { recursive: true });
const abi = JSON.parse(execFileSync(wwt(), ['abi']).toString());
const kernelPath = join(cacheDir, `kernel.${process.pid}.wasm`);
execFileSync(wwt(), ['kernel', '-o', kernelPath]);
const kernel = readFileSync(kernelPath);
rmSync(kernelPath);
const layout = useUnix ? JSON.parse(readFileSync(join(unixDir, 'wine_unix.json'), 'utf8')) : null;
const machine = new Machine({
  abi,
  kernel,
  guestLimit: 0x8000_0000,
  ...(layout && { nativeSize: layout.nativeSize, extraSize: layout.extraSize }),
  log: trace ? (s) => stderr(`[machine] ${s}\n`) : undefined,
});
await machine.init();
let unix = null;
const display = new Display();
// Blocks the thread; a wait nothing can end means the program is idle.
const started = performance.now();
const wait = (ms) => {
  if (display.input.length) return 1;
  if (screenshot && performance.now() - started > runFor) throw new ProgramIdle();
  if (ms >= 0) {
    Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
    return display.input.length ? 1 : 0;
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
  });
  if (unixTrace) unix.setTrace(unixTrace);
}
// Fast mode: code the ahead-of-time pass missed is translated when reached.
const tw = join(root, 'target/wasm32-unknown-unknown/release-wasm/wwt_wasm.wasm');
if (existsSync(tw)) {
  enableFastMode(machine, await FastTranslator.load(readFileSync(tw)), {
    log: trace ? (s) => stderr(`[fast] ${s}\n`) : undefined,
  });
}
const host = new WineHost(machine, {
  translate,
  files,
  argv: [`C:\\${basename(exe)}`, ...args],
  exePath: `C:\\${basename(exe)}`,
  stdout: (b) => stdout(b),
  stderr: (b) => stderr(b),
  trace,
  unix,
});
host.boot(`${sys32}\\ntdll.dll`, exeDos);
const r = host.run();
if (host.unimplemented.size) {
  stderr(`unimplemented syscalls: ${[...host.unimplemented.keys()].join(', ')}\n`);
}
if (r.error instanceof ProgramIdle || r.error?.cause instanceof ProgramIdle) {
  writeFileSync(screenshot, await display.png());
  stderr(`idle; screenshot in ${screenshot}\n`);
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
