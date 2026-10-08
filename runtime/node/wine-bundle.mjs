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
//   node runtime/node/wine-bundle.mjs --arch x64 --mem32 [out dir]
//                                    (default: target/wine-bundle64-m32)
//
// With --arch x64 the bundle holds Wine's x86_64 DLLs (WINE_BUILD64,
// default /opt/wine-build64), translated for a 64-bit memory at their own
// addresses, and the wasm64 Unix side (ARCH=x86_64 native/wine-unix/build.sh).
// With --mem32 as well, the same DLLs for browsers without 64-bit
// WebAssembly memory: every DLL moved below 2 GB and translated for a 32-bit
// memory, and the lowered Unix side (ARCH=x86_64 MEM32=1).
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

import { parsePe, peImports as imports, rebaseImage } from '../wine/host.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const argv = process.argv.slice(2);
const x64 = argv[0] === '--arch' && argv[1] === 'x64';
if (argv[0] === '--arch') argv.splice(0, 2);
const mem32 = x64 && argv[0] === '--mem32';
if (argv[0] === '--mem32') argv.shift();
const mem64 = x64 && !mem32;
const out = resolve(argv[0] ?? join(root, mem32 ? 'target/wine-bundle64-m32' : x64 ? 'target/wine-bundle64' : 'target/wine-bundle'));
const wineBuild = x64 ? (process.env.WINE_BUILD64 ?? '/opt/wine-build64') : (process.env.WINE_BUILD ?? '/opt/wine-build');
const peDir = x64 ? 'x86_64-windows' : 'i386-windows';
const wineSrc = process.env.WINE_SRC ?? '/opt/wine-src/wine-11.0';
const DLLS = ['ntdll', 'kernelbase', 'kernel32', 'msvcrt', 'ucrtbase'];
// Windowed programs (user32 and what Wine's programs load with it).
const GUI_DLLS = [
  'advapi32', 'sechost', 'user32', 'gdi32', 'win32u', 'imm32', 'combase', 'comctl32', 'coml2', 'cryptbase',
  'ole32', 'rpcrt4', 'uxtheme', 'comdlg32', 'shcore', 'shell32', 'shlwapi', 'comctl32_v6', 'oleaut32',
];
// Sound, DirectDraw and DirectInput (Milestone 5): fetched only for programs that
// import one of them (wined3d alone is megabytes), see runtime/web/worker.mjs.
const MEDIA_DLLS = [
  'version', 'winmm', 'msacm32', 'dsound', 'mmdevapi', 'winepulse.drv', 'ddraw', 'wined3d', 'opengl32',
  'dinput', 'dinput8', 'hid', 'setupapi',
];
// Networking and cryptography (PuTTY, curl): likewise fetched only for
// programs that import one of them.
const NET_DLLS = ['ws2_32', 'crypt32', 'dnsapi', 'nsi', 'iphlpapi', 'secur32', 'bcrypt', 'normaliz', 'wldap32'];
// MinGW's default DLL base, and where the bundle moves those DLLs to.
const DEFAULT_BASE = x64 ? 0x1_8000_0000 : 0x10000000;
const PRELINK_BASE = mem64 ? 0x1_9000_0000 : 0x60000000;
const PROGRAMS = ['winemine', 'notepad'];
const NLS = ['locale', 'l_intl', 'sortdefault', 'normnfc', 'normnfd', 'normnfkc', 'normnfkd', 'c_1252', 'c_437', 'c_850', 'c_20127'];

const wwt = ['target/release/wwt', 'target/debug/wwt']
  .map((p) => join(root, p))
  .filter((p) => existsSync(p))
  .sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs)[0];

const unixDir = join(root, mem32 ? 'target/wine-unix64-m32' : x64 ? 'target/wine-unix64' : 'target/wine-unix');
const withUnix = existsSync(join(unixDir, 'wine_unix.mjs'));

mkdirSync(out, { recursive: true });
const manifest = { wine: '11.0', arch: x64 ? 'x64' : 'x86', mem64, dlls: {}, nls: [] };
const fileName = (d) => (d.includes('.') ? d : `${d}.dll`);
const dllPath = (d) => join(wineBuild, 'dlls', d, peDir, fileName(d));
let nextBase = PRELINK_BASE;
for (const d of [...DLLS, ...(withUnix ? [...GUI_DLLS, ...MEDIA_DLLS, ...NET_DLLS] : [])]) {
  const pe = dllPath(d);
  const name = fileName(d);
  if (!existsSync(pe)) {
    console.error(`missing ${pe}; run tools/wine/build.sh ${d}`);
    process.exit(1);
  }
  let bytes = readFileSync(pe);
  const info = parsePe(bytes);
  // On a 32-bit memory every x86_64 DLL moves into the guest region (Wine
  // links its core DLLs above 4 GB).
  if (info.imageBase === DEFAULT_BASE || (mem32 && info.imageBase + info.sizeOfImage > 0x8000_0000)) {
    const moved = rebaseImage(bytes, info, nextBase);
    if (moved) {
      bytes = moved;
      nextBase += Math.ceil(info.sizeOfImage / 0x10000) * 0x10000;
    }
  }
  writeFileSync(join(out, name), bytes);
  const group = MEDIA_DLLS.includes(d) ? 'media' : NET_DLLS.includes(d) ? 'network' : null;
  // Without debug information (most of wined3d's size), when MinGW is here.
  if (group === 'media') {
    try {
      execFileSync(x64 ? 'x86_64-w64-mingw32-strip' : 'i686-w64-mingw32-strip', ['--strip-debug', join(out, name)]);
    } catch {}
  }
  execFileSync(wwt, ['translate', join(out, name), '-o', join(out, `${name}.wasm`), ...(mem64 ? ['--mem64'] : [])], {
    stdio: ['ignore', 'ignore', 'inherit'],
  });
  manifest.dlls[name] = { pe: name, wasm: `${name}.wasm`, ...(group && { group }) };
}
for (const n of NLS) {
  copyFileSync(join(wineSrc, 'nls', `${n}.nls`), join(out, `${n}.nls`));
  manifest.nls.push(`${n}.nls`);
}
// The API set map (api-ms-win-crt-* and the like to their DLLs), read as a
// file at boot, not loaded as code.
manifest.system32 = [];
if (existsSync(dllPath('apisetschema'))) {
  copyFileSync(dllPath('apisetschema'), join(out, 'apisetschema.dll'));
  manifest.system32.push('apisetschema.dll');
} else {
  console.warn(`no API set map: ${dllPath('apisetschema')} not built (tools/wine/build.sh apisetschema)`);
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
