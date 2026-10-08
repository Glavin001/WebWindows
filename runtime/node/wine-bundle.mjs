#!/usr/bin/env node
// Builds the Wine bundle the browser runtime loads: Wine's i386 PE DLLs,
// their ahead-of-time translations, and the NLS tables, plus a manifest.
// When Wine's Unix side has been built (native/wine-unix), the bundle also
// carries it with the fonts and the DLLs windowed programs use, and Wine's
// own programs (winemine, notepad) to try.
//
//   node runtime/node/wine-bundle.mjs [out dir]     (default: target/wine-bundle)
//
// Wine's DLLs are translated once here (as on CI) and shipped, so browsers
// compile them with streaming compilation and can cache the compiled code.
// Most of Wine's DLLs are linked at the same default base (0x10000000), so
// all but one would be relocated at load time and translated again; the
// bundle gives each its own base first (prelinking), from PRELINK_BASE up.
// Their debug information is stripped first (most of each file; the browser
// would download it and map it for nothing).

import { execFileSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { parsePe, peImports as imports, rebaseImage } from '../wine/host.mjs';
import { NATIVE_HEAP_FLAG } from '../wine/heap.mjs';
import { NATIVE_STRINGS_DLLS, NATIVE_STRINGS_FLAG } from '../wine/strings.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const out = resolve(process.argv[2] ?? join(root, 'target/wine-bundle'));
const wineBuild = process.env.WINE_BUILD ?? '/opt/wine-build';
const wineSrc = process.env.WINE_SRC ?? '/opt/wine-src/wine-11.0';
const DLLS = ['ntdll', 'kernelbase', 'kernel32', 'msvcrt', 'ucrtbase'];
// Windowed programs (user32 and what Wine's programs load with it).
const GUI_DLLS = [
  'advapi32', 'sechost', 'user32', 'gdi32', 'win32u', 'imm32', 'combase', 'comctl32', 'coml2', 'cryptbase',
  'ole32', 'rpcrt4', 'uxtheme', 'comdlg32', 'shcore', 'shell32', 'shlwapi', 'comctl32_v6', 'oleaut32',
];
// Sound, DirectDraw, Direct3D and DirectInput (Milestone 5): fetched only
// for programs that use one of them (wined3d alone is megabytes), see
// runtime/web/worker.mjs. wined3d draws Direct3D with its WebGPU backend
// and needs no opengl32.
const MEDIA_DLLS = [
  'version', 'winmm', 'msacm32', 'dsound', 'mmdevapi', 'winepulse.drv', 'ddraw', 'wined3d', 'd3d9',
  'dinput', 'dinput8', 'hid', 'setupapi',
];
// Translator flags per DLL. The Direct3D DLLs never write code, so their
// stores skip the self-modifying-code check (the C runtime's memcpy, which
// could copy code for a program, keeps it); the same list is in wine.mjs.
const TRANSLATE_FLAGS = { wined3d: ['--no-smc-checks'], d3d9: ['--no-smc-checks'] };
const DEFAULT_BASE = 0x10000000;
const PRELINK_BASE = 0x60000000;
const PROGRAMS = ['winemine', 'notepad'];
const NLS = ['locale', 'l_intl', 'sortdefault', 'normnfc', 'normnfd', 'normnfkc', 'normnfkd', 'c_1252', 'c_437', 'c_850', 'c_20127'];

const wwt = ['target/release/wwt', 'target/debug/wwt']
  .map((p) => join(root, p))
  .filter((p) => existsSync(p))
  .sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs)[0];

/** The image without its debug sections (binutils' strip, when installed;
 * loaded sections keep their addresses). */
function stripped(path) {
  const tmp = join(out, '.strip.tmp');
  try {
    execFileSync('i686-w64-mingw32-strip', ['--strip-debug', '-o', tmp, path], { stdio: 'ignore' });
    return readFileSync(tmp);
  } catch {
    if (!stripped.warned) console.warn('i686-w64-mingw32-strip not found: the bundle keeps debug information');
    stripped.warned = true;
    return readFileSync(path);
  }
}

const unixDir = join(root, 'target/wine-unix');
const withUnix = existsSync(join(unixDir, 'wine_unix.mjs'));
// ntdll's heap as native WebAssembly (crates/wwt-heap), when built: the
// bundle's ntdll then imports it (--native-heap), and the bundle ships it.
const heapWasm = join(root, 'target/wasm32-unknown-unknown/release-wasm/wwt_heap.wasm');
const withHeap = existsSync(heapWasm);
// Likewise the native string and locale functions (crates/wwt-strings).
const stringsWasm = join(root, 'target/wasm32-unknown-unknown/release-wasm/wwt_strings.wasm');
const withStrings = existsSync(stringsWasm);

mkdirSync(out, { recursive: true });
const manifest = { wine: '11.0', dlls: {}, nls: [] };
const fileName = (d) => (d.includes('.') ? d : `${d}.dll`);
const dllPath = (d) => join(wineBuild, 'dlls', d, 'i386-windows', fileName(d));
let nextBase = PRELINK_BASE;
for (const d of [...DLLS, ...(withUnix ? [...GUI_DLLS, ...MEDIA_DLLS] : [])]) {
  const pe = dllPath(d);
  const name = fileName(d);
  if (!existsSync(pe)) {
    console.error(`missing ${pe}; run tools/wine/build.sh ${d}`);
    process.exit(1);
  }
  let bytes = stripped(pe);
  const info = parsePe(bytes);
  if (info.imageBase === DEFAULT_BASE) {
    const moved = rebaseImage(bytes, info, nextBase);
    if (moved) {
      bytes = moved;
      nextBase += (info.sizeOfImage + 0xffff) & ~0xffff;
    }
  }
  writeFileSync(join(out, name), bytes);
  const media = MEDIA_DLLS.includes(d);
  // The browser runs Wine with a 2 GB guest (runtime/web/worker.mjs).
  const flags = [...(TRANSLATE_FLAGS[d] ?? []), '--guest-limit-mb', '2048'];
  if (withHeap && d === 'ntdll') flags.push(NATIVE_HEAP_FLAG);
  if (withStrings && NATIVE_STRINGS_DLLS.includes(name)) flags.push(NATIVE_STRINGS_FLAG);
  execFileSync(wwt, ['translate', ...flags, join(out, name), '-o', join(out, `${name}.wasm`)], {
    stdio: ['ignore', 'ignore', 'inherit'],
  });
  manifest.dlls[name] = { pe: name, wasm: `${name}.wasm`, ...(media && { group: 'media' }) };
}
if (withHeap) {
  copyFileSync(heapWasm, join(out, 'wwt_heap.wasm'));
  manifest.heap = 'wwt_heap.wasm';
}
if (withStrings) {
  copyFileSync(stringsWasm, join(out, 'wwt_strings.wasm'));
  manifest.strings = 'wwt_strings.wasm';
}
for (const n of NLS) {
  copyFileSync(join(wineSrc, 'nls', `${n}.nls`), join(out, `${n}.nls`));
  manifest.nls.push(`${n}.nls`);
}

if (withUnix) {
  // Wine's Unix side and the files it reads from its data directory.
  const unix = { module: 'wine_unix.mjs', wasm: 'wine_unix.wasm', layout: 'wine_unix.json', syscalls: 'win32u_syscalls.json', data: {} };
  for (const f of [unix.module, unix.wasm, unix.layout, unix.syscalls]) copyFileSync(join(unixDir, f), join(out, f));
  mkdirSync(join(out, 'fonts'), { recursive: true });
  const data = [['nls/l_intl.nls', join(wineSrc, 'nls/l_intl.nls')]];
  for (const f of readdirSync(join(wineSrc, 'fonts')).filter((f) => f.endsWith('.ttf'))) data.push([`fonts/${f}`, join(wineSrc, 'fonts', f)]);
  const fon = join(wineBuild, 'fonts');
  for (const f of (existsSync(fon) ? readdirSync(fon) : []).filter((f) => f.endsWith('.fon'))) data.push([`fonts/${f}`, join(fon, f)]);
  for (const [rel, src] of data) {
    mkdirSync(dirname(join(out, rel)), { recursive: true });
    copyFileSync(src, join(out, rel));
    unix.data[`/wine/share/wine/${rel}`] = rel;
  }
  manifest.unix = unix;

  // Wine's own programs, to try without choosing a folder.
  mkdirSync(join(out, 'programs'), { recursive: true });
  manifest.programs = [];
  for (const p of PROGRAMS) {
    const exe = join(wineBuild, 'programs', p, 'i386-windows', `${p}.exe`);
    if (!existsSync(exe)) {
      console.warn(`skipping ${p}: ${exe} not built (tools/wine/build.sh programs/${p})`);
      continue;
    }
    writeFileSync(join(out, 'programs', `${p}.exe`), stripped(exe));
    manifest.programs.push(`programs/${p}.exe`);
  }
}
// Every DLL the bundle's images import (statically) must be in it.
const have = new Set(Object.keys(manifest.dlls).map((n) => n.toLowerCase()));
const missing = new Set();
for (const f of [...Object.keys(manifest.dlls), ...(manifest.programs ?? [])]) {
  for (const imp of imports(readFileSync(join(out, f)))) {
    if (!have.has(imp) && !imp.startsWith('api-ms-') && !imp.startsWith('ext-ms-')) missing.add(`${imp} (imported by ${f})`);
  }
}
if (missing.size) {
  console.error(`the bundle lacks DLLs its images import:\n  ${[...missing].join('\n  ')}`);
  process.exit(1);
}
writeFileSync(join(out, 'manifest.json'), JSON.stringify(manifest, null, 2));
rmSync(join(out, '.strip.tmp'), { force: true });
console.log(`Wine bundle in ${out}${withUnix ? ' (with the Unix side, for windowed programs)' : ''}`);
