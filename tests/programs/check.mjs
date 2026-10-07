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
//   node tests/programs/check.mjs --torture DIR   # gcc.c-torture/execute
//   node tests/programs/check.mjs --arch x64      # 64-bit programs (64-bit memory)
//   node tests/programs/check.mjs --arch x64 --mem32   # on a 32-bit memory
//   node tests/programs/check.mjs --mem64         # 32-bit programs, 64-bit memory
//
// With --arch x64 the .exe is built with x86_64-w64-mingw32-gcc and the
// reference with the native x86-64 gcc. Linux is LP64 and Windows LLP64, so
// a program whose output depends on sizeof(long) would differ; the
// hand-written programs and Csmith's fixed-width types do not, and torture
// tests check themselves.
//
// Torture tests check themselves (abort or exit 0). One that MinGW cannot
// build, or that fails natively (target-specific or extended-precision
// tests), is skipped.
//
// Failing programs are copied to target/program-failures/ for reduction
// with cvise (see tests/programs/reduce.sh).

import { spawn } from 'node:child_process';
import { mkdirSync, readdirSync, readFileSync, copyFileSync, writeFileSync, existsSync, statSync } from 'node:fs';
import { basename, dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '../..');

const args = process.argv.slice(2);
let opts = ['O0', 'O1', 'O2', 'O3', 'Os'];
let csmith = 0;
let seed = 1;
let jobs = 4;
let wine = false;
let arch = 'x86';
let mem64 = null;
let torture = null;
let filter = null;
const files = [];
while (args.length) {
  const a = args.shift();
  if (a === '--opt') opts = args.shift().split(',');
  else if (a === '--csmith') csmith = Number(args.shift());
  else if (a === '--seed') seed = Number(args.shift());
  else if (a === '--jobs') jobs = Number(args.shift());
  else if (a === '--wine') wine = true;
  else if (a === '--arch') arch = args.shift();
  else if (a === '--mem64') mem64 = true;
  else if (a === '--mem32') mem64 = false;
  else if (a === '--torture') torture = resolve(args.shift());
  else if (a === '--filter') filter = new RegExp(args.shift());
  else files.push(resolve(a));
}
// 64-bit programs run on a 64-bit memory unless told otherwise.
const defaultMem64 = arch === 'x64';
mem64 ??= defaultMem64;
// 64-bit builds go to their own directory (same program names).
const work = join(root, arch === 'x64' ? 'target/programs-x64' : 'target/programs');
const failDir = join(root, 'target/program-failures');
mkdirSync(join(work, 'torture'), { recursive: true });
if (torture) {
  for (const f of readdirSync(torture).sort()) if (f.endsWith('.c')) files.push(join(torture, f));
} else if (!files.length && csmith === 0) {
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

/** Options a torture test asks for with dg-options / dg-additional-options. */
function dgOptions(src) {
  const text = readFileSync(src, 'latin1');
  const flags = [];
  for (const m of text.matchAll(/\{\s*dg-(?:additional-)?options\s+"([^"]*)"(\s*\{[^}]*\})?\s*\}/g)) {
    // Options restricted to other targets are ignored.
    if (m[2] && !/i\?86|x86|ia32|\*-\*-\*/.test(m[2])) continue;
    flags.push(...m[1].split(/\s+/).filter(Boolean));
  }
  return flags;
}

/** Expected torture failures: name -> { opts, reason }. */
const xfail = new Map();
if (torture) {
  for (const line of readFileSync(join(here, 'torture-xfail.txt'), 'utf8').split('\n')) {
    const m = line.match(/^(\S+)\s+(\S+)\s+(.*)$/);
    if (m && !line.startsWith('#')) xfail.set(m[1], { opts: m[2] === '*' ? null : m[2].split(','), reason: m[3] });
  }
}
const expectFail = (name, opt) => {
  const x = xfail.get(name);
  return x && (!x.opts || x.opts.includes(opt)) ? x.reason : null;
};

const programs = files
  .filter((f) => !filter || filter.test(basename(f)))
  .map((f) => ({ name: basename(f, '.c'), src: f, cflags: torture ? ['-w', ...dgOptions(f)] : [] }));
for (let i = 0; i < csmith; i++) {
  const s = seed + i;
  const src = join(work, `csmith-${s}.c`);
  const r = await sh('csmith', ['--seed', String(s), '--max-funcs', '6', '--max-block-depth', '4', '-o', src]);
  if (r.status !== 0) throw new Error(`csmith failed: ${r.stderr}`);
  programs.push({ name: `csmith-${s}`, src, cflags: ['-I', csmithInc, '-w'] });
}

async function build(p, opt) {
  const base = join(torture ? join(work, 'torture') : work, `${p.name}-${opt}`);
  // The reference runs x87 code at double precision, which is what the
  // translator implements by default (registers are f64).
  const m32 = arch === 'x64' ? [] : ['-m32'];
  const nat = await sh('gcc', [...m32, `-${opt}`, ...p.cflags, '-o', base + '.native', p.src, join(here, 'pc53.c'), '-lm']);
  if (nat.status !== 0) return { error: 'native build: ' + nat.stderr.slice(0, 500) };
  // The Windows build gets the same precision setting: MinGW's start-up
  // leaves the x87 at 64-bit precision, so a native Windows run would not
  // compute what the reference computes (windows-check.mjs).
  const cc = arch === 'x64' ? 'x86_64-w64-mingw32-gcc' : 'i686-w64-mingw32-gcc';
  const win = await sh(cc, [`-${opt}`, ...p.cflags, '-o', base + '.exe', p.src, join(here, 'pc53.c')]);
  if (win.status !== 0) return { error: 'mingw build: ' + win.stderr.slice(0, 500) };
  return { native: base + '.native', exe: base + '.exe' };
}

async function runOne(p, opt) {
  const b = await build(p, opt);
  if (b.error) return torture ? { status: 'skip', detail: b.error.split('\n')[0] } : { status: 'build-error', detail: b.error };
  const nat = await sh(b.native, [], { timeout: 10000 });
  if (nat.error || nat.signal) return { status: 'skip', detail: 'native run timed out or crashed' };
  if (torture && nat.status !== 0) return { status: 'skip', detail: `native exit ${nat.status}` };
  // msvcrt.dll is not a C99 runtime (no %hhd, for example); Wine's matches it.
  if (torture && wine && /dg-require-effective-target\s+c99_runtime/.test(readFileSync(p.src, 'latin1'))) {
    return { status: 'skip', detail: 'needs a C99 runtime (msvcrt.dll is not one)' };
  }
  const wasm = b.exe + '.wasm';
  const tr = await sh(wwt, ['translate', b.exe, '-o', wasm, ...(mem64 ? ['--mem64'] : [])]);
  if (tr.status !== 0) return { status: 'fail', detail: 'translate: ' + tr.stderr.slice(-800) };
  const t0 = performance.now();
  // With --wine the program runs on translated Wine DLLs (Milestone 2)
  // instead of the JavaScript Win32 shims.
  const runner = wine
    ? [join(root, 'runtime/node/wine.mjs'), b.exe]
    : [join(root, 'runtime/node/run.mjs'), '--wasm', wasm, mem64 ? '--mem64' : '--mem32', b.exe];
  // Earlier Node 22 releases have 64-bit memory behind a flag (Node 24
  // rejects the flag).
  const nodeFlags = mem64 && process.version.startsWith('v22.') ? ['--experimental-wasm-memory64'] : [];
  const run = await sh('node', [...nodeFlags, ...runner], { timeout: 120000 });
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
  return { status: 'pass', exe: relative(work, b.exe), exitCode: got.code, stdout: got.out, ms: Math.round(run.ms ?? 0) };
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
      let r = await runOne(p, opt);
      const why = expectFail(p.name, opt);
      if (why && r.status === 'fail') r = { status: 'xfail', detail: why };
      else if (why && r.status === 'pass') r = { status: 'fail', detail: `expected to fail (${why}) but passed` };
      // The M1 JavaScript shims cover only part of msvcrt; translated Wine
      // (--wine) is where the full API lives.
      if (torture && !wine && r.status === 'fail' && /unimplemented API/.test(r.detail)) {
        r = { status: 'skip', detail: r.detail.match(/unimplemented API \S+/)[0] };
      }
      results.push({ name: p.name, opt, ...r });
      const tag = { pass: 'ok  ', fail: 'FAIL', skip: 'skip', xfail: 'xfail', 'build-error': 'BLD ' }[r.status];
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
const summary = `${results.length} runs: ${count('pass')} pass, ${count('fail')} fail, ${count('skip')} skipped, ${count('xfail')} expected failures, ${count('build-error')} build errors`;
console.log(summary);
writeFileSync(join(work, `results${csmith ? '-csmith' : ''}${torture ? '-torture' : ''}${wine ? '-wine' : ''}${mem64 !== defaultMem64 ? (mem64 ? '-mem64' : '-mem32') : ''}.json`), JSON.stringify(results, null, 2));
process.exit(count('fail') + count('build-error') ? 1 : 0);
