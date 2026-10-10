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
// WWT_MEM_TRAPS=1 translates the DLLs with bounds traps instead of memory
// checks (wwt translate --mem-traps).
//
// Wine's DLLs are translated once here (as on CI) and shipped, so browsers
// compile them with streaming compilation and can cache the compiled code.
// Most of Wine's DLLs are linked at the same default base (0x10000000), so
// all but one would be relocated at load time and translated again; the
// bundle gives each its own base first (prelinking), from PRELINK_BASE up.
// Their debug information is stripped first (most of each file; the browser
// would download it and map it for nothing).

import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { parsePe, peImports as imports, rebaseImage } from '../wine/host.mjs';
import { NATIVE_HEAP_FLAG } from '../wine/heap.mjs';
import { NATIVE_STRINGS_DLLS, NATIVE_STRINGS_FLAG } from '../wine/strings.mjs';

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
// Sound, DirectDraw, Direct3D, DirectInput and Winsock (games): fetched
// only for programs that use one of them (wined3d alone is megabytes), see
// runtime/web/worker.mjs. wined3d draws Direct3D with its WebGPU backend
// and needs no opengl32; opengl32 is native/opengl32-webgl (OpenGL 2.1 on
// WebGL 2, below); d3dcompiler_47 compiles HLSL for programs that do it at run
// time. Winsock has no network behind it (single-player games
// talk to their own server in memory).
const MEDIA_DLLS = [
  'version', 'winmm', 'msacm32', 'dsound', 'mmdevapi', 'winepulse.drv', 'ddraw', 'wined3d', 'd3d8', 'd3d9',
  'd3dcompiler_47', 'opengl32', 'dinput', 'dinput8', 'hid', 'setupapi', 'msvfw32', 'avifil32',
  'ws2_32', 'wsock32', 'iphlpapi', 'dnsapi', 'nsi',
];
// Networking and cryptography (PuTTY, curl): likewise fetched only for
// programs that import one of them.
const NET_DLLS = ['ws2_32', 'crypt32', 'dnsapi', 'nsi', 'iphlpapi', 'secur32', 'bcrypt', 'normaliz', 'wldap32', 'wininet', 'mpr'];
// Rich edit controls: loaded by name (Unreal's Window.dll loads RICHED32.DLL
// for its RICHEDIT class), so fetched for programs whose files name them.
const RICHEDIT_DLLS = ['riched20', 'riched32'];
// Translator flags per DLL. The Direct3D DLLs never write code, so their
// stores skip the self-modifying-code check (the C runtime's memcpy, which
// could copy code for a program, keeps it); the same list is in wine.mjs.
const TRANSLATE_FLAGS = { wined3d: ['--no-smc-checks'], d3d8: ['--no-smc-checks'], d3d9: ['--no-smc-checks'] };
// MinGW's default DLL base, and where the bundle moves those DLLs to.
const DEFAULT_BASE = x64 ? 0x1_8000_0000 : 0x10000000;
const PRELINK_BASE = mem64 ? 0x1_9000_0000 : 0x60000000;
const PROGRAMS = ['winemine', 'notepad'];
const NLS = ['locale', 'l_intl', 'sortdefault', 'normnfc', 'normnfd', 'normnfkc', 'normnfkd', 'c_1252', 'c_437', 'c_850', 'c_20127'];

// WWT=path: another translator build (an experiment's, in its own target dir).
const wwt =
  process.env.WWT ??
  ['target/release/wwt', 'target/debug/wwt']
    .map((p) => join(root, p))
    .filter((p) => existsSync(p))
    .sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs)[0];
// The translator as part of each translation's key: by content, since CI
// builds the same translator again on every run.
const wwtHash = wwt && existsSync(wwt) ? createHash('sha256').update(readFileSync(wwt)).digest('hex') : '';

/** The image without its debug sections (binutils' strip, when installed;
 * loaded sections keep their addresses). */
function stripped(path) {
  const tmp = join(out, '.strip.tmp');
  const strip = x64 ? 'x86_64-w64-mingw32-strip' : 'i686-w64-mingw32-strip';
  try {
    execFileSync(strip, ['--strip-debug', '-o', tmp, path], { stdio: 'ignore' });
    return readFileSync(tmp);
  } catch {
    if (!stripped.warned) console.warn(`${strip} not found: the bundle keeps debug information`);
    stripped.warned = true;
    return readFileSync(path);
  }
}

const unixDir = join(root, mem32 ? 'target/wine-unix64-m32' : x64 ? 'target/wine-unix64' : 'target/wine-unix');
const withUnix = existsSync(join(unixDir, 'wine_unix.mjs'));
// ntdll's heap as native WebAssembly (crates/wwt-heap), when built: the
// bundle's ntdll then imports it (--native-heap), and the bundle ships it.
const heapWasm = join(root, 'target/wasm32-unknown-unknown/release-wasm/wwt_heap.wasm');
const withHeap = existsSync(heapWasm);
// Likewise the native string and locale functions (crates/wwt-strings).
const stringsWasm = join(root, 'target/wasm32-unknown-unknown/release-wasm/wwt_strings.wasm');
const withStrings = existsSync(stringsWasm);

mkdirSync(out, { recursive: true });
const manifest = { wine: '11.0', arch: x64 ? 'x64' : 'x86', mem64, dlls: {}, nls: [] };
const fileName = (d) => (d.includes('.') ? d : `${d}.dll`);
// opengl32 is the build's WebGL 2 one where there is one (gl4es,
// native/opengl32-webgl): Node keeps the one over Direct3D 9.
const webglOpengl = join(wineBuild, 'dlls', 'opengl32', peDir, 'opengl32-webgl.dll');
const dllPath = (d) =>
  d === 'opengl32' && existsSync(webglOpengl) ? webglOpengl : join(wineBuild, 'dlls', d, peDir, fileName(d));
let nextBase = PRELINK_BASE;
for (const d of [...DLLS, ...(withUnix ? [...GUI_DLLS, ...MEDIA_DLLS, ...NET_DLLS, ...RICHEDIT_DLLS] : [])]) {
  const pe = dllPath(d);
  const name = fileName(d);
  if (!existsSync(pe)) {
    console.error(`missing ${pe}; run tools/wine/build.sh ${d}`);
    process.exit(1);
  }
  let bytes = stripped(pe);
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
  const group = MEDIA_DLLS.includes(d) ? 'media' : NET_DLLS.includes(d) ? 'network' : RICHEDIT_DLLS.includes(d) ? 'richedit' : null;
  // The browser runs Wine with a 2 GB guest on a 32-bit memory
  // (runtime/web/worker.mjs), a constant in the memory checks; a 64-bit
  // memory reads its limit at run time. The native heap and string
  // functions are i386 code's.
  const flags = [...(TRANSLATE_FLAGS[d] ?? []), ...(mem64 ? ['--mem64'] : ['--guest-limit-mb', '2048'])];
  if (process.env.WWT_MEM_TRAPS === '1') flags.push('--mem-traps');
  if (withHeap && !x64 && d === 'ntdll') flags.push(NATIVE_HEAP_FLAG);
  if (withStrings && !x64 && NATIVE_STRINGS_DLLS.includes(name)) flags.push(NATIVE_STRINGS_FLAG);
  // Translated again only when the DLL, the flags or the translator changed
  // (the key file next to the translation records them).
  const key = createHash('sha256').update(bytes).update(JSON.stringify(flags)).update(wwtHash).digest('hex');
  const keyFile = join(out, `${name}.wasm.key`);
  if (!existsSync(join(out, `${name}.wasm`)) || !existsSync(keyFile) || readFileSync(keyFile, 'utf8') !== key) {
    execFileSync(wwt, ['translate', ...flags, join(out, name), '-o', join(out, `${name}.wasm`)], {
      stdio: ['ignore', 'ignore', 'inherit'],
    });
    writeFileSync(keyFile, key);
  }
  manifest.dlls[name] = { pe: name, wasm: `${name}.wasm`, ...(group && { group }) };
}
if (withHeap && !x64) {
  copyFileSync(heapWasm, join(out, 'wwt_heap.wasm'));
  manifest.heap = 'wwt_heap.wasm';
}
if (withStrings && !x64) {
  copyFileSync(stringsWasm, join(out, 'wwt_strings.wasm'));
  manifest.strings = 'wwt_strings.wasm';
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
