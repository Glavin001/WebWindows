#!/usr/bin/env node
// Builds the Wine bundle the browser runtime loads: Wine's i386 PE DLLs,
// their ahead-of-time translations, and the NLS tables, plus a manifest.
//
//   node runtime/node/wine-bundle.mjs [out dir]     (default: target/wine-bundle)
//
// Wine's DLLs are translated once here (as on CI) and shipped, so browsers
// compile them with streaming compilation and can cache the compiled code.

import { execFileSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const out = resolve(process.argv[2] ?? join(root, 'target/wine-bundle'));
const wineBuild = process.env.WINE_BUILD ?? '/opt/wine-build';
const wineSrc = process.env.WINE_SRC ?? '/opt/wine-src/wine-11.0';
const DLLS = ['ntdll', 'kernelbase', 'kernel32', 'msvcrt', 'ucrtbase'];
const NLS = ['locale', 'l_intl', 'sortdefault', 'normnfc', 'normnfd', 'normnfkc', 'normnfkd', 'c_1252', 'c_437', 'c_850', 'c_20127'];

const wwt = ['target/release/wwt', 'target/debug/wwt']
  .map((p) => join(root, p))
  .filter((p) => existsSync(p))
  .sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs)[0];

mkdirSync(out, { recursive: true });
const manifest = { wine: '11.0', dlls: {}, nls: [] };
for (const d of DLLS) {
  const pe = join(wineBuild, 'dlls', d, 'i386-windows', `${d}.dll`);
  if (!existsSync(pe)) {
    console.error(`missing ${pe}; run tools/wine/build.sh`);
    process.exit(1);
  }
  copyFileSync(pe, join(out, `${d}.dll`));
  execFileSync(wwt, ['translate', pe, '-o', join(out, `${d}.dll.wasm`)], { stdio: ['ignore', 'ignore', 'inherit'] });
  manifest.dlls[`${d}.dll`] = { pe: `${d}.dll`, wasm: `${d}.dll.wasm` };
}
for (const n of NLS) {
  copyFileSync(join(wineSrc, 'nls', `${n}.nls`), join(out, `${n}.nls`));
  manifest.nls.push(`${n}.nls`);
}
writeFileSync(join(out, 'manifest.json'), JSON.stringify(manifest, null, 2));
console.log(`Wine bundle in ${out}`);
