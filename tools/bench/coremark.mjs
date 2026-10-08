#!/usr/bin/env node
// CoreMark across tiers: native, Emscripten (C source straight to
// WebAssembly) and WebWindows (C source -> MinGW .exe -> wwt -> WebAssembly).
// Emscripten shows what WebAssembly itself can do; the gap between it and
// the translated .exe is the translator's overhead.
//
//   node tools/bench/coremark.mjs [options]
//
//   --tiers a,b,...   native, clang, emcc, wwt, wine (default: every tier whose
//                     tools are installed; clang only when asked)
//   --runs N          runs per tier; the table shows the best and median
//   --check           only check that every tier prints the same checksums
//                     (fixed iteration count, so crcfinal is comparable too)
//   --no-check        skip the checksum check before benchmarking
//   --variants        also run wwt with checks off, to attribute their cost
//   --translate "..." extra `wwt translate` options for the wwt tier
//   --json FILE       write the results as JSON (for tracking over time)
//   --build-only      build the calibrated benchmarks and stop
//   --emcc-crc        let Emscripten's LLVM turn CoreMark's bit-by-bit CRC
//                     loops into table lookups (off by default: GCC doesn't,
//                     so it would make the WebAssembly ceiling unfair)
//
// Needs gcc-multilib and gcc-mingw-w64-i686; Emscripten from PATH or $EMSDK
// (e.g. ~/emsdk); `cargo build --release -p wwt-cli`. CoreMark calibrates
// each performance run to at least 10 seconds.

import { execFileSync, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { COREMARK_FILES, fetchCoremark } from './sources.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
// The runtimes translate with the newest of the release and debug builds;
// benchmarks always use the release one.
process.env.WWT ??= join(root, 'target/release/wwt');
const src = join(root, 'target/coremark-src');
const out = join(root, 'target/bench');
const FILES = COREMARK_FILES;
const CHECK_ITERATIONS = 2000;
const CHECKSUMS = ['seedcrc', '[0]crclist', '[0]crcmatrix', '[0]crcstate', '[0]crcfinal'];

// ---- Options ----

const opts = { runs: 1, check: true, checkOnly: false, variants: false, translate: [], tiers: null, json: null };
const argv = process.argv.slice(2);
while (argv.length) {
  const a = argv.shift();
  if (a === '--tiers') opts.tiers = argv.shift().split(',');
  else if (a === '--runs') opts.runs = Number(argv.shift());
  else if (a === '--check') opts.checkOnly = true;
  else if (a === '--no-check') opts.check = false;
  else if (a === '--variants') opts.variants = true;
  else if (a === '--translate') opts.translate = argv.shift().split(/\s+/).filter(Boolean);
  else if (a === '--json') opts.json = argv.shift();
  else if (a === '--build-only') opts.buildOnly = true;
  else if (a === '--emcc-crc') opts.emccCrc = true;
  else {
    console.error(`unknown option ${a}`);
    process.exit(2);
  }
}

// ---- Tools ----

function which(cmd) {
  return spawnSync('sh', ['-c', `command -v ${cmd}`]).status === 0;
}

/** emcc on PATH, or from $EMSDK / ~/emsdk. */
function findEmcc() {
  if (which('emcc')) return 'emcc';
  for (const d of [process.env.EMSDK, join(homedir(), 'emsdk')].filter(Boolean)) {
    const e = join(d, 'upstream/emscripten/emcc');
    if (existsSync(e)) return e;
  }
  return null;
}

const wwt = join(root, 'target/release/wwt');
const emcc = findEmcc();
const wineBuild = process.env.WINE_BUILD ?? '/opt/wine-build';

const available = {
  native: which('gcc'),
  clang: which('clang'),
  emcc: !!emcc,
  wwt: which('i686-w64-mingw32-gcc') && existsSync(wwt),
  wine: which('i686-w64-mingw32-gcc') && existsSync(wwt) && existsSync(wineBuild),
};
const missing = {
  native: 'gcc (gcc-multilib) not found',
  clang: 'clang not found',
  emcc: 'Emscripten not found (PATH, $EMSDK or ~/emsdk)',
  wwt: 'needs i686-w64-mingw32-gcc and `cargo build --release -p wwt-cli`',
  wine: `needs Wine's PE DLLs in ${wineBuild} (tools/wine/build.sh)`,
};
const tiers = opts.tiers ?? ['native', 'emcc', 'wwt', 'wine'].filter((t) => available[t]);
for (const t of opts.tiers ?? ['native', 'emcc', 'wwt', 'wine']) {
  if (!available[t]) console.error(`skipping ${t}: ${missing[t]}`);
}

// ---- Builds ----

function fetchSource() {
  fetchCoremark(src);
}

/** Builds one tier; `iterations` 0 means a calibrated performance run. */
function build(tier, iterations) {
  const suffix = iterations ? `.it${iterations}` : '';
  const flags = ['-O2', '-I.', '-Isimple', '-DPERFORMANCE_RUN=1', `-DITERATIONS=${iterations}`, '-DFLAGS_STR="-O2"'];
  const cc = (cmd, extra, file, stdio = 'inherit') => {
    execFileSync(cmd, [...flags, ...extra, ...FILES, '-o', file], { cwd: src, stdio });
    return file;
  };
  switch (tier) {
    case 'native':
      return cc('gcc', ['-m32'], join(out, `coremark.native${suffix}`));
    case 'clang':
      return cc('clang', ['-m32'], join(out, `coremark.clang${suffix}`));
    case 'emcc': {
      // --profiling-funcs keeps names for tools/bench/profile.mjs and costs
      // nothing at run time. Recent LLVM recognizes CoreMark's CRC loops and
      // replaces them with a table lookup, which GCC (and so the .exe) does
      // not: an algorithmic change, not WebAssembly being faster, so it is
      // off unless asked for (older LLVM has no such option or transform).
      const file = join(out, `coremark.emcc${suffix}.js`);
      if (!opts.emccCrc) {
        try {
          return cc(emcc, ['--profiling-funcs', '-mllvm', '--loop-idiom-crc-strategy=disable'], file, 'pipe');
        } catch {}
      }
      return cc(emcc, ['--profiling-funcs'], file);
    }
    case 'wwt':
    case 'wine':
      return cc('i686-w64-mingw32-gcc', [], join(out, `coremark${suffix}.exe`));
  }
}

/** Command line that runs a tier's build. */
function command(tier, file, wasm) {
  switch (tier) {
    case 'native':
    case 'clang':
      return [file, []];
    case 'emcc':
      return [process.execPath, [file]];
    case 'wwt':
      return [process.execPath, [join(root, 'runtime/node/run.mjs'), ...(wasm ? ['--wasm', wasm] : []), file]];
    case 'wine':
      return [process.execPath, [join(root, 'runtime/node/wine.mjs'), file]];
  }
}

function run(tier, file, wasm) {
  const [cmd, args] = command(tier, file, wasm);
  const r = spawnSync(cmd, args, { encoding: 'utf8', maxBuffer: 64 << 20 });
  const text = r.stdout ?? '';
  const sums = {};
  for (const k of CHECKSUMS) {
    const m = text.match(new RegExp(`^${k.replace(/[[\]]/g, '\\$&')}\\s*:\\s*(0x[0-9a-f]+)`, 'm'));
    if (m) sums[k] = m[1];
  }
  const score = text.match(/^CoreMark 1\.0 : ([0-9.]+)/m);
  return {
    // Short fixed-count runs are not "validated" (CoreMark wants 10 s), but
    // CoreMark still says when a checksum is wrong.
    ok: /Correct operation validated/.test(text) || (/^\[0\]crcfinal/m.test(text) && !/ERROR! \w+ crc/.test(text)),
    score: score ? Number(score[1]) : null,
    sums,
    error:
      r.status !== 0 || r.signal
        ? `exit ${r.status ?? r.signal}: ${(r.stderr ?? '').trim().split('\n').slice(-3).join('\n')}`
        : null,
    // Why CoreMark did not validate the run (e.g. it ran under 10 s).
    complaint: text.split('\n').filter((l) => /ERROR|Errors detected/.test(l)).join(' ') || null,
  };
}

/** Translates the .exe ahead of time so translation is not part of the run. */
function translate(exe, extra, name, quiet = false) {
  const wasm = join(out, `${name}.wasm`);
  execFileSync(wwt, ['translate', exe, '-o', wasm, '--guest-limit-mb', '1024', ...extra], { stdio: ['ignore', quiet ? 'ignore' : 'inherit', quiet ? 'ignore' : 'inherit'] });
  return wasm;
}

// ---- Main ----

mkdirSync(out, { recursive: true });
fetchSource();

if (opts.buildOnly) {
  for (const t of tiers) build(t, 0);
  process.exit(0);
}

let checksOk = true;
if (opts.check || opts.checkOnly) {
  console.log(`== checksums, ${CHECK_ITERATIONS} iterations`);
  const ref = {};
  for (const t of tiers) {
    const file = build(t, CHECK_ITERATIONS);
    const r = run(t, file, t === 'wwt' ? translate(file, opts.translate, 'coremark.check', true) : null);
    const line = CHECKSUMS.map((k) => `${k.replace('[0]', '')}=${r.sums[k] ?? '?'}`).join(' ');
    let same = true;
    for (const k of CHECKSUMS) {
      if (!(k in ref)) ref[k] = r.sums[k];
      if (r.sums[k] === undefined || r.sums[k] !== ref[k]) same = false;
    }
    const verdict = !r.ok ? 'FAILED' : same ? 'ok' : 'MISMATCH';
    if (verdict !== 'ok') checksOk = false;
    console.log(`${t.padEnd(8)} ${verdict.padEnd(8)} ${line}${r.error ? `\n  ${r.error}` : ''}`);
  }
  if (!checksOk) console.log('checksums differ between tiers');
  if (opts.checkOnly) process.exit(checksOk ? 0 : 1);
}

const rows = [];
const bench = (label, tier, file, wasm) => {
  const scores = [];
  for (let i = 0, retried = false; i < opts.runs; i++) {
    const r = run(tier, file, wasm);
    if (!r.ok || r.score === null) {
      console.error(`${label}: run failed: ${r.error ?? r.complaint ?? 'no result printed'}`);
      // One retry: CoreMark rejects a run that finished in under 10 s,
      // which machine load changing after its calibration can cause.
      if (!retried) {
        retried = true;
        i--;
        continue;
      }
      break;
    }
    scores.push(r.score);
    console.error(`${label}: ${r.score.toFixed(0)} iterations/s`);
  }
  if (!scores.length) return;
  const sorted = [...scores].sort((a, b) => a - b);
  rows.push({ label, tier, best: sorted.at(-1), median: sorted[(sorted.length - 1) >> 1], scores });
};

const labels = {
  native: 'native, gcc -m32 -O2',
  clang: 'native, clang -m32 -O2',
  emcc: `Emscripten -O2, Node${opts.emccCrc ? ' (CRC idiom)' : ''}`,
  wwt: 'wwt, M1 shims, Node',
  wine: 'wwt, translated Wine, Node',
};
for (const t of tiers) {
  const file = build(t, 0);
  if (t === 'wwt') {
    bench(labels.wwt, t, file, translate(file, opts.translate, 'coremark'));
    if (opts.variants) {
      const v = [
        ['no SMC checks', ['--no-smc-checks']],
        ['no memory or SMC checks', ['--no-mem-checks', '--no-smc-checks']],
      ];
      for (const [name, extra] of v) {
        bench(`  ${name}`, t, file, translate(file, [...opts.translate, ...extra], `coremark.${extra.join('')}`));
      }
    }
  } else {
    bench(labels[t], t, file, null);
  }
}

// ---- Report ----

const nat = rows.find((r) => r.tier === 'native' && r.label === labels.native)?.median;
const em = rows.find((r) => r.tier === 'emcc')?.median;
const pct = (x, y) => (x && y ? `${((x / y) * 100).toFixed(0)}%` : '—');
const fmt = (x) => x.toFixed(0).padStart(7);
console.log(`\nCoreMark, Node ${process.version}${opts.runs > 1 ? `, median of ${opts.runs} (best)` : ''}`);
console.log(`${'tier'.padEnd(30)} ${'it/s'.padStart(7)}${opts.runs > 1 ? '         ' : ''}  vs native  vs Emscripten`);
for (const r of rows) {
  const best = opts.runs > 1 ? ` (${fmt(r.best).trim()})`.padEnd(9) : '';
  const vsEm = r.tier === 'native' || r.tier === 'clang' ? '—' : pct(r.median, em);
  console.log(`${r.label.padEnd(30)} ${fmt(r.median)}${best}  ${pct(r.median, nat).padStart(9)}  ${vsEm.padStart(13)}`);
}
if (opts.check) console.log(`checksums identical across tiers: ${checksOk ? 'yes' : 'NO'}`);
if (opts.json) {
  const result = { date: new Date().toISOString(), node: process.version, translate: opts.translate, checksOk, rows };
  writeFileSync(opts.json, JSON.stringify(result, null, 2) + '\n');
}
process.exit(checksOk ? 0 : 1);
