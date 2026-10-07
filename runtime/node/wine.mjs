#!/usr/bin/env node
// Runs a Windows program on translated Wine (Milestone 2).
//
//   node runtime/node/wine.mjs [--trace] program.exe [args...]
//
// Wine's PE DLLs come from WINE_BUILD (default /opt/wine-build) and its NLS
// files from WINE_SRC; translations are cached in target/wine-cache.

import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, readdirSync, statSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { Machine, GuestFault, hex } from '../runtime.mjs';
import { WineHost } from '../wine/host.mjs';

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
  const hash = createHash('sha256').update(bytes).digest('hex').slice(0, 16);
  const out = join(cacheDir, `${path.split('\\').pop()}-${hash}.wasm`);
  if (!existsSync(out)) {
    const tmp = join(cacheDir, `${hash}.bin`);
    execFileSync('sh', ['-c', `cat > ${tmp}`], { input: bytes });
    execFileSync(wwt(), ['translate', tmp, '-o', out], { stdio: ['ignore', 'ignore', 'inherit'] });
  }
  return readFileSync(out);
}

const args = process.argv.slice(2);
let trace = false;
while (args[0]?.startsWith('--')) {
  const a = args.shift();
  if (a === '--trace') trace = true;
}
const exe = args.shift();
if (!exe) {
  console.error('usage: wine.mjs [--trace] program.exe [args...]');
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
execFileSync(wwt(), ['kernel', '-o', join(cacheDir, 'kernel.wasm')]);
const machine = new Machine({
  abi,
  kernel: readFileSync(join(cacheDir, 'kernel.wasm')),
  guestLimit: 0x8000_0000,
  log: trace ? (s) => process.stderr.write(`[machine] ${s}\n`) : undefined,
});
await machine.init();
const host = new WineHost(machine, {
  translate,
  files,
  argv: [`C:\\${basename(exe)}`, ...args],
  exePath: `C:\\${basename(exe)}`,
  stdout: (b) => process.stdout.write(b),
  stderr: (b) => process.stderr.write(b),
  trace,
});
host.boot(`${sys32}\\ntdll.dll`, exeDos);
const r = host.run();
if (host.unimplemented.size) {
  process.stderr.write(`unimplemented syscalls: ${[...host.unimplemented.keys()].join(', ')}\n`);
}
if (r.error) {
  process.stderr.write(`\n*** ${r.error instanceof GuestFault ? 'guest fault' : 'error'}: ${r.error.message}\n`);
  if (!(r.error instanceof GuestFault)) process.stderr.write(r.error.stack + '\n');
  process.exit(128);
}
process.exit(r.exitCode ?? 0);
