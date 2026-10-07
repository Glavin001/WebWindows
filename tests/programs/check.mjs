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
//   node tests/programs/check.mjs --wine          # on translated Wine DLLs
//
// Failing programs are copied to target/program-failures/ for reduction
// with cvise (see tests/programs/reduce.sh).

import { spawn } from 'node:child_process';
import { mkdirSync, readdirSync, copyFileSync, writeFileSync, existsSync, statSync } from 'node:fs';
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
let wine = false;
const files = [];
while (args.length) {
  const a = args.shift();
  if (a === '--opt') opts = args.shift().split(',');
  else if (a === '--csmith') csmith = Number(args.shift());
  else if (a === '--seed') seed = Number(args.shift());
  else if (a === '--jobs') jobs = Number(args.shift());
  else if (a === '--wine') wine = true;
  else files.push(resolve(a));
}
if (!files.length && csmith === 0) {
  for (const f of readdirSync(join(here, 'c')).sort()) if (f.endsWith('.c')) files.push(join(here, 'c', f));
}

const csmithInc = join(root, 'tests/csmith/runtime');
// The most recently built translator.
const wwt = ['target/release/wwt', 'target/debug/wwt']
  .map((p) => join(root, p))
  .filter((p) => existsSync(p))
  .sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs)[0];

/** Runs a command, resolving with { status, stdout, stderr, error, signal }. */
function sh(cmd, argv, { timeout = 0 } = {}) {
  return new Promise((resolveP) => {
    const child = spawn(cmd, argv, { stdio: ['ignore', 'pipe', 'pipe'] });
    const out = [];
    const err = [];
    let timedOut = false;
    const timer = timeout ? setTimeout(() => { timedOut = true; child.kill('SIGKILL'); }, timeout) : null;
    child.stdout.on('data', (d) => out.push(d));
    child.stderr.on('data', (d) => err.push(d));
    child.on('error', (e) => resolveP({ status: -1, stdout: '', stderr: String(e), error: e }));
    child.on('close', (status, signal) => {
      if (timer) clearTimeout(timer);
      resolveP({
        status,
        signal,
        error: timedOut ? new Error('timed out') : null,
        stdout: Buffer.concat(out).toString('latin1'),
        stderr: Buffer.concat(err).toString('latin1'),
      });
    });
  });
}

const programs = files.map((f) => ({ name: basename(f, '.c'), src: f, cflags: [] }));
for (let i = 0; i < csmith; i++) {
  const s = seed + i;
  const src = join(work, `csmith-${s}.c`);
  const r = await sh('csmith', ['--seed', String(s), '--max-funcs', '6', '--max-block-depth', '4', '-o', src]);
  if (r.status !== 0) throw new Error(`csmith failed: ${r.stderr}`);
  programs.push({ name: `csmith-${s}`, src, cflags: ['-I', csmithInc, '-w'] });
}

async function build(p, opt) {
  const base = join(work, `${p.name}-${opt}`);
  // The reference runs x87 code at double precision, which is what the
  // translator implements by default (registers are f64).
  const nat = await sh('gcc', ['-m32', `-${opt}`, ...p.cflags, '-o', base + '.native', p.src, join(here, 'pc53.c'), '-lm']);
  if (nat.status !== 0) return { error: 'native build: ' + nat.stderr.slice(0, 500) };
  const win = await sh('i686-w64-mingw32-gcc', [`-${opt}`, ...p.cflags, '-o', base + '.exe', p.src]);
  if (win.status !== 0) return { error: 'mingw build: ' + win.stderr.slice(0, 500) };
  return { native: base + '.native', exe: base + '.exe' };
}

async function runOne(p, opt) {
  const b = await build(p, opt);
  if (b.error) return { status: 'build-error', detail: b.error };
  const nat = await sh(b.native, [], { timeout: 10000 });
  if (nat.error || nat.signal) return { status: 'skip', detail: 'native run timed out or crashed' };
  const wasm = b.exe + '.wasm';
  const tr = await sh(wwt, ['translate', b.exe, '-o', wasm]);
  if (tr.status !== 0) return { status: 'fail', detail: 'translate: ' + tr.stderr.slice(-800) };
  const t0 = performance.now();
  // With --wine the program runs on translated Wine DLLs (Milestone 2)
  // instead of the JavaScript Win32 shims.
  const runner = wine
    ? [join(root, 'runtime/node/wine.mjs'), b.exe]
    : [join(root, 'runtime/node/run.mjs'), '--wasm', wasm, b.exe];
  const run = await sh('node', runner, { timeout: 120000 });
  run.ms = performance.now() - t0;
  if (run.error) return { status: 'fail', detail: `translated run: ${run.error.message}` };
  const want = { out: nat.stdout, code: nat.status & 0xff };
  // Windows C runtimes write text-mode stdout with CRLF line endings.
  const got = { out: run.stdout.replace(/\r\n/g, '\n'), code: run.status & 0xff };
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
  return { status: 'pass', exitCode: got.code, stdout: got.out, ms: Math.round(run.ms ?? 0) };
}

const results = [];
const todo = [];
for (const p of programs) for (const opt of opts) todo.push([p, opt]);

// Builds and runs are subprocesses; overlap `jobs` of them.
async function pool() {
  let next = 0;
  async function worker() {
    while (next < todo.length) {
      const [p, opt] = todo[next++];
      const r = await runOne(p, opt);
      results.push({ name: p.name, opt, ...r });
      const tag = { pass: 'ok  ', fail: 'FAIL', skip: 'skip', 'build-error': 'BLD ' }[r.status];
      process.stdout.write(`${tag} ${p.name} -${opt}${r.detail && r.status !== 'pass' ? '  ' + r.detail : ''}\n`);
      if (r.status === 'fail') {
        mkdirSync(failDir, { recursive: true });
        copyFileSync(p.src, join(failDir, `${p.name}-${opt}.c`));
      }
    }
  }
  await Promise.all(Array.from({ length: jobs }, worker));
}
await pool();

const count = (s) => results.filter((r) => r.status === s).length;
const summary = `${results.length} runs: ${count('pass')} pass, ${count('fail')} fail, ${count('skip')} skipped, ${count('build-error')} build errors`;
console.log(summary);
writeFileSync(join(work, `results${csmith ? '-csmith' : ''}${wine ? '-wine' : ''}.json`), JSON.stringify(results, null, 2));
process.exit(count('fail') + count('build-error') ? 1 : 0);
