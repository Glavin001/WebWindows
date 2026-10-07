#!/usr/bin/env node
// Self time per function, from V8's sampling profiler, for CoreMark's
// translated build next to its Emscripten build (or for any Node command).
// Translated functions are named `symbol@address` (or `x86_address`) by
// the translator's name section, so hot spots map straight back to x86.
//
//   node tools/bench/profile.mjs [--top N] [--tier wwt|emcc|both] [--translate "..."]
//   node tools/bench/profile.mjs [--top N] -- node runtime/node/run.mjs prog.exe
//   node tools/bench/profile.mjs --perf [--annotate core_state] [--tier wwt|emcc]
//
// --perf samples with Linux `perf` and V8's jitdump instead, which works at
// machine-code level: `--annotate NAME` lists the hottest x86-64
// instructions V8 generated for the functions matching NAME (spills to the
// stack frame show up as `mov ...,-0x58(%rbp)`). Needs `perf` (linux-tools;
// $PERF or on PATH).
//
// The CoreMark builds come from tools/bench/coremark.mjs (built on demand).
// The translated module is produced ahead of time, so translation does not
// show up in the profile.

import { execFileSync, spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const bench = join(root, 'target/bench');

let top = 25;
let tier = 'both';
let translate = [];
let command = null;
let usePerf = false;
let annotate = null;
const argv = process.argv.slice(2);
while (argv.length) {
  const a = argv.shift();
  if (a === '--top') top = Number(argv.shift());
  else if (a === '--tier') tier = argv.shift();
  else if (a === '--translate') translate = argv.shift().split(/\s+/).filter(Boolean);
  else if (a === '--perf') usePerf = true;
  else if (a === '--annotate') {
    usePerf = true;
    annotate = argv.shift();
  }
  else if (a === '--') {
    command = argv.splice(0);
  } else {
    console.error(`unknown option ${a}`);
    process.exit(2);
  }
}

/** Runs `node --cpu-prof <args>` and returns the parsed profile. */
function profile(args) {
  const dir = mkdtempSync(join(tmpdir(), 'wwt-prof-'));
  const r = spawnSync(process.execPath, ['--cpu-prof', `--cpu-prof-dir=${dir}`, ...args], {
    encoding: 'utf8',
    maxBuffer: 64 << 20,
  });
  const score = (r.stdout ?? '').match(/^CoreMark 1\.0 : ([0-9.]+)/m);
  const files = readdirSync(dir).filter((f) => f.endsWith('.cpuprofile'));
  if (!files.length) throw new Error(`no profile written\n${r.stderr}`);
  const p = JSON.parse(readFileSync(join(dir, files[0]), 'utf8'));
  rmSync(dir, { recursive: true, force: true });
  return { p, score: score ? Number(score[1]) : null };
}

/** Classifies a frame: translated x86, other WebAssembly, JavaScript, V8. */
function category(frame) {
  const n = frame.functionName;
  if (/^\(/.test(n)) return n; // (garbage collector), (program), (idle)
  if (/@[0-9a-f]+$|^x86_[0-9a-f]+$/.test(n)) return 'translated x86';
  if (/^helper_/.test(n)) return 'translator helpers';
  if (/wasm-function|\.wasm/.test(frame.url) || /^\$?wasm-function/.test(n)) return 'WebAssembly';
  return 'JavaScript';
}

function summarize(title, { p, score }) {
  const byId = new Map(p.nodes.map((n) => [n.id, n]));
  const self = new Map();
  const cats = new Map();
  let total = 0;
  p.samples.forEach((id, i) => {
    const n = byId.get(id);
    const dt = p.timeDeltas[i] || 0;
    const f = n.callFrame;
    let name = f.functionName || '(anonymous)';
    if (category(f) === 'JavaScript') name += `  ${f.url.split('/').pop()}:${f.lineNumber + 1}`;
    self.set(name, (self.get(name) || 0) + dt);
    const c = category(f);
    cats.set(c, (cats.get(c) || 0) + dt);
    total += dt;
  });
  const pct = (t) => `${((t / total) * 100).toFixed(1).padStart(5)}%`;
  console.log(`\n== ${title}${score ? ` (${score.toFixed(0)} iterations/s)` : ''}, ${(total / 1e6).toFixed(1)} s sampled`);
  for (const [c, t] of [...cats].sort((a, b) => b[1] - a[1])) console.log(`${pct(t)}  [${c}]`);
  console.log('');
  for (const [k, t] of [...self].sort((a, b) => b[1] - a[1]).slice(0, top)) console.log(`${pct(t)}  ${k}`);
}

function findPerf() {
  if (process.env.PERF) return process.env.PERF;
  if (spawnSync('sh', ['-c', 'command -v perf && perf --version']).status === 0) return 'perf';
  // Ubuntu's linux-tools for another kernel than the running one.
  const dirs = existsSync('/usr/lib') ? readdirSync('/usr/lib').filter((d) => d.startsWith('linux-tools')) : [];
  for (const d of dirs.sort().reverse()) if (existsSync(`/usr/lib/${d}/perf`)) return `/usr/lib/${d}/perf`;
  throw new Error('perf not found (apt-get install linux-tools-generic, or set PERF)');
}

/** Samples `node args` with perf; prints hot functions and annotations. */
function perfProfile(title, args) {
  const perf = findPerf();
  const dir = mkdtempSync(join(tmpdir(), 'wwt-perf-'));
  const raw = join(dir, 'raw.data');
  const jit = join(dir, 'jit.data');
  const r = spawnSync(perf, ['record', '-k', 'mono', '-e', 'cpu-clock', '-F', '2000', '-o', raw, '--', process.execPath, '--perf-prof', ...args], {
    cwd: dir,
    encoding: 'utf8',
    maxBuffer: 64 << 20,
  });
  if (r.status !== 0) throw new Error(`perf record failed\n${r.stderr}`);
  execFileSync(perf, ['inject', '--jit', '-i', raw, '-o', jit], { cwd: dir, stdio: 'ignore' });
  const report = execFileSync(perf, ['report', '-i', jit, '--stdio', '--sort', 'sym'], { cwd: dir, maxBuffer: 256 << 20 })
    .toString()
    .split('\n')
    .filter((l) => /^\s+[0-9.]+%/.test(l));
  const score = (r.stdout ?? '').match(/^CoreMark 1\.0 : ([0-9.]+)/m);
  console.log(`\n== ${title} (perf)${score ? ` (${Number(score[1]).toFixed(0)} iterations/s)` : ''}`);
  for (const l of report.slice(0, top)) console.log(l.replace(/\[\.\] (JS:)?/, '').replace(/-\d+-turbofan$/, ''));
  if (annotate) {
    const syms = report.map((l) => l.match(/\[\.\] (\S+)/)?.[1]).filter((s) => s && s.includes(annotate));
    for (const sym of syms.slice(0, 3)) {
      const text = execFileSync(perf, ['annotate', '-i', jit, '--stdio', '-s', sym], { cwd: dir, maxBuffer: 256 << 20 }).toString();
      const lines = text.split('\n').filter((l) => /^\s+[0-9]+\.[0-9]+ :/.test(l));
      const hot = lines.map((l) => [parseFloat(l), l]).filter(([p]) => p >= 0.5);
      console.log(`\n-- ${sym}: instructions with >= 0.5% of its samples, in address order`);
      for (const [, l] of hot) console.log(l.replace(/<JS:[^>]*>/, ''));
    }
  }
  rmSync(dir, { recursive: true, force: true });
}

function ensureBuilds() {
  const need = [join(bench, 'coremark.exe'), join(bench, 'coremark.emcc.js')];
  if (need.every(existsSync)) return;
  execFileSync(process.execPath, [join(root, 'tools/bench/coremark.mjs'), '--build-only'], { stdio: 'inherit' });
}

const run = (title, args) => (usePerf ? perfProfile(title, args) : summarize(title, profile(args)));
if (command) {
  const args = command[0] === 'node' || command[0] === process.execPath ? command.slice(1) : command;
  run(args.join(' '), args);
} else {
  ensureBuilds();
  if (tier === 'emcc' || tier === 'both') {
    run('Emscripten', [join(bench, 'coremark.emcc.js')]);
  }
  if (tier === 'wwt' || tier === 'both') {
    const exe = join(bench, 'coremark.exe');
    const wasm = join(bench, 'coremark.prof.wasm');
    execFileSync(join(root, 'target/release/wwt'), ['translate', exe, '-o', wasm, ...translate], { stdio: 'ignore' });
    run('wwt, M1 shims', [join(root, 'runtime/node/run.mjs'), '--wasm', wasm, exe]);
  }
}
