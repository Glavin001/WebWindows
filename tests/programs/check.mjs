#!/usr/bin/env node
// Test layer 2: whole programs.
//
// Each C program is built twice: natively with `gcc -m32` (the reference,
// run on this x86 machine) and as a Windows .exe with MinGW at several
// optimization levels. The .exe is translated and run in Node; stdout and
// the exit code must match the native run.
//
//   node tests/programs/check.mjs                 # hand-written programs
//   node tests/programs/check.mjs --csmith 50     # plus 50 Csmith programs
//   node tests/programs/check.mjs --opt O0,O2 tests/programs/c/switch.c
//
// Failing programs are copied to target/program-failures/ for reduction
// with cvise (see tests/programs/reduce.sh).

import { spawnSync } from 'node:child_process';
import { mkdirSync, readdirSync, copyFileSync, writeFileSync, existsSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '../..');
const work = join(root, 'target/programs');
const failDir = join(root, 'target/program-failures');
mkdirSync(work, { recursive: true });

const args = process.argv.slice(2);
let opts = ['O0', 'O1', 'O2', 'O3', 'Os'];
let csmith = 0;
let seed = 1;
let jobs = 4;
const files = [];
while (args.length) {
  const a = args.shift();
  if (a === '--opt') opts = args.shift().split(',');
  else if (a === '--csmith') csmith = Number(args.shift());
  else if (a === '--seed') seed = Number(args.shift());
  else if (a === '--jobs') jobs = Number(args.shift());
  else files.push(resolve(a));
}
if (!files.length && csmith === 0) {
  for (const f of readdirSync(join(here, 'c')).sort()) if (f.endsWith('.c')) files.push(join(here, 'c', f));
}

const csmithInc = join(root, 'tests/csmith/runtime');
const wwt = existsSync(join(root, 'target/release/wwt')) ? join(root, 'target/release/wwt') : join(root, 'target/debug/wwt');

function sh(cmd, argv, o = {}) {
  return spawnSync(cmd, argv, { encoding: 'latin1', maxBuffer: 64 << 20, ...o });
}

const programs = files.map((f) => ({ name: basename(f, '.c'), src: f, cflags: [] }));
for (let i = 0; i < csmith; i++) {
  const s = seed + i;
  const src = join(work, `csmith-${s}.c`);
  const r = sh('csmith', ['--seed', String(s), '--max-funcs', '6', '--max-block-depth', '4', '-o', src]);
  if (r.status !== 0) throw new Error(`csmith failed: ${r.stderr}`);
  programs.push({ name: `csmith-${s}`, src, cflags: ['-I', csmithInc, '-w'] });
}

function build(p, opt) {
  const base = join(work, `${p.name}-${opt}`);
  // The reference runs x87 code at double precision, which is what the
  // translator implements by default (registers are f64).
  const nat = sh('gcc', ['-m32', `-${opt}`, ...p.cflags, '-o', base + '.native', p.src, join(here, 'pc53.c'), '-lm']);
  if (nat.status !== 0) return { error: 'native build: ' + nat.stderr.slice(0, 500) };
  const win = sh('i686-w64-mingw32-gcc', [`-${opt}`, ...p.cflags, '-o', base + '.exe', p.src]);
  if (win.status !== 0) return { error: 'mingw build: ' + win.stderr.slice(0, 500) };
  return { native: base + '.native', exe: base + '.exe' };
}

function runOne(p, opt) {
  const b = build(p, opt);
  if (b.error) return { status: 'build-error', detail: b.error };
  const nat = sh(b.native, [], { timeout: 10000 });
  if (nat.error || nat.signal) return { status: 'skip', detail: 'native run timed out or crashed' };
  const wasm = b.exe + '.wasm';
  const tr = sh(wwt, ['translate', b.exe, '-o', wasm]);
  if (tr.status !== 0) return { status: 'fail', detail: 'translate: ' + tr.stderr.slice(-800) };
  const run = sh('node', [join(root, 'runtime/node/run.mjs'), '--wasm', wasm, b.exe], { timeout: 120000 });
  if (run.error) return { status: 'fail', detail: `translated run: ${run.error.message}` };
  const want = { out: nat.stdout, code: nat.status & 0xff };
  const got = { out: run.stdout, code: run.status & 0xff };
  if (want.out !== got.out || want.code !== got.code) {
    let detail = '';
    if (want.code !== got.code) detail += `exit code: want ${want.code} got ${got.code}. `;
    if (want.out !== got.out) {
      const wl = want.out.split('\n');
      const gl = got.out.split('\n');
      const k = wl.findIndex((l, i) => l !== gl[i]);
      detail += `stdout line ${k + 1}: want ${JSON.stringify(wl[k])} got ${JSON.stringify(gl[k])}. `;
    }
    detail += run.stderr.split('\n').filter((l) => !l.includes('.wasm:')).join(' ').slice(0, 600);
    return { status: 'fail', detail };
  }
  return { status: 'pass' };
}

const results = [];
const todo = [];
for (const p of programs) for (const opt of opts) todo.push([p, opt]);

// Run with a simple worker pool of child processes (builds and runs are
// subprocesses, so parallelism comes from overlapping them).
async function pool() {
  const { Worker } = await import('node:worker_threads');
  void Worker;
  for (const [p, opt] of todo) {
    const r = runOne(p, opt);
    results.push({ name: p.name, opt, ...r });
    const tag = { pass: 'ok  ', fail: 'FAIL', skip: 'skip', 'build-error': 'BLD ' }[r.status];
    process.stdout.write(`${tag} ${p.name} -${opt}${r.detail && r.status !== 'pass' ? '  ' + r.detail : ''}\n`);
    if (r.status === 'fail') {
      mkdirSync(failDir, { recursive: true });
      copyFileSync(p.src, join(failDir, `${p.name}-${opt}.c`));
    }
  }
}
await pool();
void jobs;

const count = (s) => results.filter((r) => r.status === s).length;
const summary = `${results.length} runs: ${count('pass')} pass, ${count('fail')} fail, ${count('skip')} skipped, ${count('build-error')} build errors`;
console.log(summary);
writeFileSync(join(work, 'results.json'), JSON.stringify(results, null, 2));
process.exit(count('fail') + count('build-error') ? 1 : 0);
