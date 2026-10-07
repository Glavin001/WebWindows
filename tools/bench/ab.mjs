#!/usr/bin/env node
// A/B test translator variants on CoreMark: each variant is translated once,
// then all variants run in turn, round after round, so drift on a shared
// machine hits them equally. Reports the median iterations/s and the ratio
// to the first variant.
//
//   node tools/bench/ab.mjs [--rounds 7] [--iterations 20000] VARIANT...
//
// A VARIANT is `name=options`: `wwt translate` options plus environment
// assignments (`NAME=value`) for the translator, e.g.
//
//   node tools/bench/ab.mjs base= nosmc=--no-smc-checks "nochecks=--no-mem-checks --no-smc-checks"
//
// or `name=@file.wasm` for a module translated elsewhere. With a fixed
// iteration count CoreMark runs for a couple of seconds and prints no
// "validated" line (it wants 10 s), but it still checks its CRCs: a variant
// with wrong results is reported as such.

import { execFileSync, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const out = join(root, 'target/bench/ab');
const wwt = join(root, 'target/release/wwt');

let rounds = 7;
let iterations = 20000;
const variants = [];
const argv = process.argv.slice(2);
while (argv.length) {
  const a = argv.shift();
  if (a === '--rounds') rounds = Number(argv.shift());
  else if (a === '--iterations') iterations = Number(argv.shift());
  else if (a.includes('=')) {
    const i = a.indexOf('=');
    variants.push({ name: a.slice(0, i), spec: a.slice(i + 1) });
  } else {
    console.error(`bad argument ${a} (variants are name=options)`);
    process.exit(2);
  }
}
if (!variants.length) {
  console.error('usage: node tools/bench/ab.mjs [--rounds N] [--iterations N] name=options...');
  process.exit(2);
}

// The fixed-iteration .exe (built like coremark.mjs's check builds).
mkdirSync(out, { recursive: true });
const exe = join(root, `target/bench/coremark.it${iterations}.exe`);
if (!existsSync(exe)) {
  const src = join(root, 'target/coremark-src');
  if (!existsSync(src)) execFileSync(process.execPath, [join(root, 'tools/bench/coremark.mjs'), '--build-only', '--tiers', 'wwt'], { stdio: 'inherit' });
  const files = ['core_list_join.c', 'core_main.c', 'core_matrix.c', 'core_state.c', 'core_util.c', 'simple/core_portme.c'];
  execFileSync(
    'i686-w64-mingw32-gcc',
    ['-O2', '-I.', '-Isimple', '-DPERFORMANCE_RUN=1', `-DITERATIONS=${iterations}`, '-DFLAGS_STR="-O2"', ...files, '-o', exe],
    { cwd: src, stdio: 'inherit' },
  );
}

for (const v of variants) {
  if (v.spec.startsWith('@')) {
    v.wasm = resolve(v.spec.slice(1));
    continue;
  }
  const env = { ...process.env };
  const args = [];
  for (const tok of v.spec.split(/\s+/).filter(Boolean)) {
    const m = tok.match(/^([A-Z_][A-Z0-9_]*)=(.*)$/);
    if (m) env[m[1]] = m[2];
    else args.push(tok);
  }
  v.wasm = join(out, `${v.name.replace(/[^\w.-]/g, '_')}.wasm`);
  execFileSync(wwt, ['translate', exe, '-o', v.wasm, '--guest-limit-mb', '1024', ...args], { env, stdio: ['ignore', 'ignore', 'inherit'] });
  v.scores = [];
}

for (let r = 0; r < rounds; r++) {
  for (const v of variants) {
    const p = spawnSync(process.execPath, [join(root, 'runtime/node/run.mjs'), '--wasm', v.wasm, exe], { encoding: 'utf8' });
    const text = p.stdout ?? '';
    const m = text.match(/^Iterations\/Sec\s*:\s*([0-9.]+)/m);
    if (!m || /ERROR! \w+ crc/.test(text)) {
      v.failed = (p.stderr || text).trim().split('\n').slice(-2).join(' | ');
      continue;
    }
    (v.scores ??= []).push(Number(m[1]));
  }
  process.stderr.write(`round ${r + 1}/${rounds}\r`);
}
process.stderr.write('\n');

const median = (xs) => {
  const s = [...xs].sort((a, b) => a - b);
  return s.length ? (s[(s.length - 1) >> 1] + s[s.length >> 1]) / 2 : NaN;
};
const base = median(variants[0].scores ?? []);
console.log(`CoreMark ${iterations} iterations, ${rounds} rounds, Node ${process.version}`);
console.log(`${'variant'.padEnd(20)} ${'median'.padStart(8)} ${'min'.padStart(8)} ${'max'.padStart(8)}  vs ${variants[0].name}`);
for (const v of variants) {
  const s = v.scores ?? [];
  const med = median(s);
  const rel = `${(((med / base) - 1) * 100).toFixed(1)}%`;
  console.log(
    `${v.name.padEnd(20)} ${med.toFixed(0).padStart(8)} ${Math.min(...s).toFixed(0).padStart(8)} ${Math.max(...s).toFixed(0).padStart(8)}  ${v === variants[0] ? '' : (med >= base ? '+' : '') + rel}` +
      (v.failed ? `  FAILED: ${v.failed}` : ''),
  );
}
