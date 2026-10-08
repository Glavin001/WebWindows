#!/usr/bin/env node
// Differential test of the native string and locale functions
// (crates/wwt-strings): builds tests/wine/strings.c with MinGW and runs it on
// translated Wine with the native functions and without them
// (WWT_NATIVE_STRINGS=0); the outputs must be the same. Then each of the
// program's faulting calls, one per run, must stop at the same instruction
// and address either way.
//
//   node tests/wine/strings.mjs [-v] [--no-faults]

import { execFileSync, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '../..');
const out = join(root, 'target/strings-test');
mkdirSync(out, { recursive: true });
const exe = join(out, 'strings.exe');
execFileSync('i686-w64-mingw32-gcc', ['-O2', join(here, 'strings.c'), '-o', exe]);
if (!existsSync(join(root, 'target/wasm32-unknown-unknown/release-wasm/wwt_strings.wasm'))) {
  console.error('wwt_strings.wasm is not built: cargo build -p wwt-strings --target wasm32-unknown-unknown --profile release-wasm');
  process.exit(1);
}

/** Runs the program; for a faulting call, its last output and the fault. */
function run(native, args = [], faults = false, locale = 0x409) {
  const r = spawnSync(process.execPath, [join(root, 'runtime/node/wine.mjs'), exe, ...args], {
    env: { ...process.env, WWT_NATIVE_STRINGS: native ? '1' : '0', WWT_LOCALE: String(locale) },
    encoding: 'latin1',
    maxBuffer: 64 << 20,
  });
  if (faults) return `${r.stdout.trim().split('\n').pop()} ${r.stderr.match(/guest fault: .*/)?.[0]}`;
  if (r.status !== 0) {
    console.error(r.stderr);
    throw new Error(`strings.exe exited with ${r.status} (native: ${native})`);
  }
  return r.stdout;
}

// A few user locales, which lstrcmp and CompareStringW pass on. (The user's
// sort is the default one whatever the locale, as there is no registry to
// choose another; strings.c tries every sort itself.)
const locales = [0x409, 0x40e, 0x41f, 0x411];
let failed = false;
for (const locale of locales) {
  const [wine, native] = [run(false, [], false, locale), run(true, [], false, locale)];
  const name = `locale ${locale.toString(16)}`;
  if (wine === native) {
    console.log(`${name}: same output (${wine.length} bytes)`);
    continue;
  }
  failed = true;
  writeFileSync(join(out, `wine-${locale.toString(16)}.txt`), wine);
  writeFileSync(join(out, `native-${locale.toString(16)}.txt`), native);
  const a = wine.split('\n');
  const b = native.split('\n');
  for (let i = 0; i < Math.max(a.length, b.length); i++) {
    if (a[i] !== b[i]) {
      const at = [...(a[i] ?? '')].findIndex((c, k) => c !== b[i]?.[k]);
      console.log(`${name}: line ${i + 1} differs at column ${at}:\n  wine:   ${a[i]?.slice(Math.max(0, at - 20), at + 40)}\n  native: ${b[i]?.slice(Math.max(0, at - 20), at + 40)}`);
    }
  }
  console.log(`${name}: outputs differ; see ${out}/wine-*.txt and native-*.txt`);
}
if (failed) process.exit(1);
if (process.argv.includes('--no-faults')) process.exit(0);

// Faulting calls (strings.c's fault()): case | 0x10 for address 1, | 0x100
// for an address above the guest limit (else NULL); | 0x20 for msvcrt
// (else ntdll).
const cases = [];
for (const at of [0, 0x10, 0x100]) {
  for (const dll of [0, 0x20]) for (let c = 0; c <= 7; c++) cases.push(at | dll | c);
  for (let c = 8; c <= 10; c++) cases.push(at | c);
}
let bad = 0;
for (const n of cases) {
  const args = ['fault', `0x${n.toString(16)}`];
  const [a, b] = [run(false, args, true), run(true, args, true)];
  if (a !== b) bad++;
  if (a !== b || process.argv.includes('-v')) console.log(`fault case 0x${n.toString(16)}: ${a === b ? a : `\n  wine:   ${a}\n  native: ${b}`}`);
}
console.log(`${cases.length - bad}/${cases.length} fault cases the same`);
process.exit(bad ? 1 : 0);
