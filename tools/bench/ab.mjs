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
// or `name=@file.wasm` for a module translated elsewhere.
//
// --wine "prog.exe args" A/B tests a Windows program on translated Wine
// instead (with --file HOST=DOS as for wine.mjs): each variant's options go
// to the translator through WWT_TRANSLATE_FLAGS, and the program's output
// lines "name checksum seconds" (tools/bench/workloads) are compared per
// line; the first run of each variant, which translates, is not timed. With a fixed
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
let wineCmd = null;
const wineFiles = [];
const variants = [];
const argv = process.argv.slice(2);
while (argv.length) {
  const a = argv.shift();
  if (a === '--rounds') rounds = Number(argv.shift());
  else if (a === '--iterations') iterations = Number(argv.shift());
  else if (a === '--wine') wineCmd = argv.shift().split(/\s+/).filter(Boolean);
  else if (a === '--file') wineFiles.push('--file', argv.shift());
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

const median = (xs) => {
  const s = [...xs].sort((a, b) => a - b);
  return s.length ? (s[(s.length - 1) >> 1] + s[s.length >> 1]) / 2 : NaN;
};

if (wineCmd) {
  abWine();
  process.exit(0);
}

/** Variants of a Windows program on translated Wine. */
function abWine() {
  const run = (v) => {
    const env = { ...process.env, WWT_TRANSLATE_FLAGS: '' };
    const flags = [];
    for (const tok of v.spec.split(/\s+/).filter(Boolean)) {
      const m = tok.match(/^([A-Z_][A-Z0-9_]*)=(.*)$/);
      if (m) env[m[1]] = m[2];
      else flags.push(tok);
    }
    env.WWT_TRANSLATE_FLAGS = flags.join(' ');
    const p = spawnSync(process.execPath, [join(root, 'runtime/node/wine.mjs'), ...wineFiles, ...wineCmd], { env, encoding: 'utf8', maxBuffer: 64 << 20 });
    const rows = [...(p.stdout ?? '').matchAll(/^(\w+)\s+(\S+)\s+([0-9.]+)\s*$/gm)].map((m) => ({ name: m[1], sum: m[2], time: Number(m[3]) }));
    if (!rows.length) v.failed = (p.stderr || p.stdout || '').trim().split('\n').slice(-2).join(' | ');
    return rows;
  };
  for (const v of variants) {
    v.times = {};
    v.sums = {};
    run(v); // translate (cached for the timed runs)
  }
  for (let r = 0; r < rounds; r++) {
    for (const v of variants) {
      for (const row of run(v)) {
        (v.times[row.name] ??= []).push(row.time);
        v.sums[row.name] = row.sum;
      }
    }
    process.stderr.write(`round ${r + 1}/${rounds}\r`);
  }
  process.stderr.write('\n');
  const names = Object.keys(variants[0].times);
  console.log(`${wineCmd.join(' ')} on translated Wine, ${rounds} rounds; median seconds`);
  console.log(`${'bench'.padEnd(14)}${variants.map((v) => v.name.padStart(16)).join('')}`);
  const total = variants.map(() => 0);
  for (const n of names) {
    const meds = variants.map((v) => median(v.times[n] ?? []));
    meds.forEach((m, i) => (total[i] += m));
    const cells = meds.map((m, i) => {
      const rel = i === 0 ? '' : ` ${((meds[0] / m - 1) * 100).toFixed(0).padStart(4)}%`;
      const bad = variants[i].sums[n] !== variants[0].sums[n] ? '!' : '';
      return `${m.toFixed(3)}${rel}${bad}`.padStart(16);
    });
    console.log(`${n.padEnd(14)}${cells.join('')}`);
  }
  console.log(`${'total'.padEnd(14)}${total.map((t, i) => `${t.toFixed(3)}${i ? ` ${((total[0] / t - 1) * 100).toFixed(0).padStart(4)}%` : ''}`.padStart(16)).join('')}`);
  for (const v of variants) if (v.failed) console.log(`${v.name}: FAILED ${v.failed}`);
  console.log('(percentages: speed relative to the first variant; ! marks a checksum that differs from it)');
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
