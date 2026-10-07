#!/usr/bin/env node
// Builds the Wine bundle the browser runtime loads: Wine's i386 PE DLLs,
// their ahead-of-time translations, and the NLS tables, plus a manifest.
// When Wine's Unix side has been built (native/wine-unix), the bundle also
// carries it with the fonts and the DLLs windowed programs use, and Wine's
// own programs (winemine, notepad) to try.
//
//   node runtime/node/wine-bundle.mjs [out dir]     (default: target/wine-bundle)
//   node runtime/node/wine-bundle.mjs --arch x64 [out dir]
//                                    (default: target/wine-bundle64)
//
// With --arch x64 the bundle holds Wine's x86_64 DLLs (WINE_BUILD64,
// default /opt/wine-build64), translated for a 64-bit memory at their own
// addresses, and the wasm64 Unix side (ARCH=x86_64 native/wine-unix/build.sh).
//
// Wine's DLLs are translated once here (as on CI) and shipped, so browsers
// compile them with streaming compilation and can cache the compiled code.
// Most of Wine's DLLs are linked at the same default base (0x10000000), so
// all but one would be relocated at load time and translated again; the
// bundle gives each its own base first (prelinking), from PRELINK_BASE up.

import { execFileSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { parsePe, rebaseImage } from '../wine/host.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const argv = process.argv.slice(2);
const x64 = argv[0] === '--arch' && argv[1] === 'x64';
if (argv[0] === '--arch') argv.splice(0, 2);
const out = resolve(argv[0] ?? join(root, x64 ? 'target/wine-bundle64' : 'target/wine-bundle'));
const wineBuild = x64 ? (process.env.WINE_BUILD64 ?? '/opt/wine-build64') : (process.env.WINE_BUILD ?? '/opt/wine-build');
const peDir = x64 ? 'x86_64-windows' : 'i386-windows';
const wineSrc = process.env.WINE_SRC ?? '/opt/wine-src/wine-11.0';
const DLLS = ['ntdll', 'kernelbase', 'kernel32', 'msvcrt', 'ucrtbase'];
// Windowed programs (user32 and what Wine's programs load with it).
const GUI_DLLS = [
  'advapi32', 'sechost', 'user32', 'gdi32', 'win32u', 'imm32', 'combase', 'comctl32', 'coml2', 'cryptbase',
  'ole32', 'rpcrt4', 'uxtheme', 'comdlg32', 'shcore', 'shell32', 'shlwapi', 'comctl32_v6', 'oleaut32',
];
// MinGW's default DLL base, and where the bundle moves those DLLs to.
const DEFAULT_BASE = x64 ? 0x1_8000_0000 : 0x10000000;
const PRELINK_BASE = x64 ? 0x1_9000_0000 : 0x60000000;
const PROGRAMS = ['winemine', 'notepad'];
const NLS = ['locale', 'l_intl', 'sortdefault', 'normnfc', 'normnfd', 'normnfkc', 'normnfkd', 'c_1252', 'c_437', 'c_850', 'c_20127'];

const wwt = ['target/release/wwt', 'target/debug/wwt']
  .map((p) => join(root, p))
  .filter((p) => existsSync(p))
  .sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs)[0];

const unixDir = join(root, x64 ? 'target/wine-unix64' : 'target/wine-unix');
const withUnix = existsSync(join(unixDir, 'wine_unix.mjs'));

mkdirSync(out, { recursive: true });
const manifest = { wine: '11.0', arch: x64 ? 'x64' : 'x86', dlls: {}, nls: [] };
const dllPath = (d) => join(wineBuild, 'dlls', d, peDir, `${d}.dll`);
let nextBase = PRELINK_BASE;
for (const d of [...DLLS, ...(withUnix ? GUI_DLLS : [])]) {
  const pe = dllPath(d);
  if (!existsSync(pe)) {
    console.error(`missing ${pe}; run tools/wine/build.sh ${d}`);
    process.exit(1);
  }
  let bytes = readFileSync(pe);
  const info = parsePe(bytes);
  if (info.imageBase === DEFAULT_BASE) {
    const moved = rebaseImage(bytes, info, nextBase);
    if (moved) {
      bytes = moved;
      nextBase += Math.ceil(info.sizeOfImage / 0x10000) * 0x10000;
    }
  }
  writeFileSync(join(out, `${d}.dll`), bytes);
  execFileSync(wwt, ['translate', join(out, `${d}.dll`), '-o', join(out, `${d}.dll.wasm`), ...(x64 ? ['--mem64'] : [])], {
    stdio: ['ignore', 'ignore', 'inherit'],
  });
  manifest.dlls[`${d}.dll`] = { pe: `${d}.dll`, wasm: `${d}.dll.wasm` };
}
for (const n of NLS) {
  copyFileSync(join(wineSrc, 'nls', `${n}.nls`), join(out, `${n}.nls`));
  manifest.nls.push(`${n}.nls`);
}

if (withUnix) {
  // Wine's Unix side and the files it reads from its data directory.
  const unix = {
    module: 'wine_unix.mjs',
    wasm: 'wine_unix.wasm',
    layout: 'wine_unix.json',
    syscalls: 'win32u_syscalls.json',
    ntCalls: 'nt_calls.json',
    data: {},
  };
  for (const f of [unix.module, unix.wasm, unix.layout, unix.syscalls, unix.ntCalls]) copyFileSync(join(unixDir, f), join(out, f));
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
    const exe = join(wineBuild, 'programs', p, peDir, `${p}.exe`);
    if (!existsSync(exe)) {
      console.warn(`skipping ${p}: ${exe} not built (${x64 ? 'ARCH=x86_64 ' : ''}tools/wine/build.sh programs/${p})`);
      continue;
    }
    copyFileSync(exe, join(out, 'programs', `${p}.exe`));
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
console.log(`Wine bundle in ${out}${withUnix ? ' (with the Unix side, for windowed programs)' : ''}`);

/** Names (lowercase) of the DLLs a PE image imports. */
function imports(bytes) {
  const info = parsePe(bytes);
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const off = (rva) => {
    const sec = info.sections.find((x) => rva >= x.virtual_address && rva < x.virtual_address + Math.max(x.virtual_size, x.raw_size));
    return sec ? rva - sec.virtual_address + sec.raw_offset : -1;
  };
  const opt = dv.getUint32(0x3c, true) + 24;
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
