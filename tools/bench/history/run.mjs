// Runs the suite's wwt-wine workloads on each checkpoint's own translator
// and runtime (built by build.sh), or with ABL=1 the last checkpoint with
// one optimization off at a time.
//
//   node tools/bench/history/run.mjs [label ...]          (default: all)
//   SPLIT_API=1 ...   each Windows API benchmark in its own process
//   ABL=1 OUT=abl.json node tools/bench/history/run.mjs [ablation ...]
//
// The programs are the suite's (target/bench, from `node tools/bench/suite.mjs`),
// the same for every checkpoint. Results accumulate in $HIST_DIR/results.json
// (label -> times, checksums, cold and warm start, translated bytes).
import { spawnSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(here, '../../..');
const H = process.env.HIST_DIR ?? join(REPO, 'target/history');
const workloads = join(REPO, 'tools/bench/workloads');
const bench = join(REPO, 'target/bench');
const ROUNDS = Number(process.env.ROUNDS ?? 3);
const out = join(H, process.env.OUT ?? 'results.json');
const results = existsSync(out) ? JSON.parse(readFileSync(out)) : {};

const cps = readFileSync(join(here, 'checkpoints.txt'), 'utf8').trim().split('\n').map((l) => {
  const [commit, label, ...rest] = l.split(' ');
  return { commit, label, title: rest.join(' ') };
});
const want = process.argv.slice(2);
// Ablations run the last checkpoint's frozen build.
const ABL_ROOT = join(H, `wt-${cps.at(-1).label}`);

function columns(text) {
  const rows = [];
  for (const m of text.matchAll(/^(\w+)\s+(\S+)\s+([0-9.]+)\s*$/gm)) rows.push({ name: m[1], sum: m[2], time: Number(m[3]) });
  return rows;
}
const W = {
  coremark: {
    exe: 'suite-coremark.exe',
    args: () => [],
    parse(text) {
      const ips = Number(text.match(/^Iterations\/Sec\s*:\s*([0-9.]+)/m)?.[1]);
      const sum = text.match(/^\[0\]crcfinal\s*:\s*(0x[0-9a-f]+)/m)?.[1];
      return ips ? [{ name: 'coremark', time: 30000 / ips, sum }] : [];
    },
  },
  sqlite: {
    exe: 'suite-sqlite.exe',
    args: (dir) => ['--verify', '--size', '50', join(dir, 'speedtest.db')],
    parse(text) {
      const total = Number(text.match(/TOTAL\.+\s+([0-9.]+)s/)?.[1]);
      const sum = text.match(/^Verification Hash: (.*)$/m)?.[1]?.trim();
      return total ? [{ name: 'speedtest1', time: total, sum }] : [];
    },
  },
  lua: {
    exe: 'suite-lua.exe',
    args: () => ['C:\\bench.lua', '4'],
    files: [[join(workloads, 'bench.lua'), 'C:\\bench.lua']],
    parse: columns,
  },
  apibench: { exe: 'suite-apibench.exe', args: () => ['2'], parse: columns },
};
// Each Windows API benchmark on its own, so one that crashes an old runtime
// does not take the others with it.
if (process.env.SPLIT_API) {
  delete W.apibench;
  if (process.env.ONLY_API) for (const k of ['coremark', 'sqlite', 'lua']) delete W[k];
  for (const b of ['heap', 'malloc', 'files', 'seek', 'strings', 'sync', 'qsort']) {
    W[`apibench-${b}`] = { exe: 'suite-apibench.exe', args: () => ['2', b], parse: columns };
  }
}
if (process.env.ONLY) for (const k of Object.keys(W)) if (!process.env.ONLY.split(',').includes(k.replace(/-.*/, ''))) delete W[k];

let n = 0;
function run(wt, args, extraEnv = {}) {
  const dir = join(tmpdir(), `hist-run-${process.pid}-${n++}`);
  mkdirSync(dir, { recursive: true });
  const env = { ...process.env, ...extraEnv };
  for (const k of Object.keys(env)) if (k.startsWith('WWT') && !(k in extraEnv)) delete env[k];
  const t0 = performance.now();
  const r = spawnSync(process.execPath, [join(wt, 'runtime/node/wine.mjs'), ...args(dir)], {
    cwd: dir, env, encoding: 'utf8', maxBuffer: 256 << 20, timeout: 15 * 60 * 1000,
  });
  const wall = (performance.now() - t0) / 1000;
  rmSync(dir, { recursive: true, force: true });
  return { stdout: r.stdout ?? '', stderr: r.stderr ?? '', status: r.status, wall };
}

// Runtimes from before --file get the script from a line added to their
// wine.mjs for these measurements.
let fileFlag = true;
function wargs(w) {
  return (dir) => [...(fileFlag ? (w.files ?? []) : []).flatMap(([h, d]) => ['--file', `${h}=${d}`]), join(bench, w.exe), ...w.args(dir)];
}

function median(a) {
  const s = [...a].sort((x, y) => x - y);
  return s[Math.floor(s.length / 2)];
}

// Ablations: the current tree with one optimization off at a time.
const ABL = [
  ['full', 'Everything on', {}],
  ['noheap', 'Wine heap (no native heap)', { WWT_NATIVE_HEAP: '0' }],
  ['nostrings', 'Wine strings (no native strings)', { WWT_NATIVE_STRINGS: '0' }],
  ['nothunks', 'No thunk aliasing', { WWT_THUNK_ALIAS: '0' }],
  ['nocabi', 'No C call ABI (--c-abi off)', { WWT_TRANSLATE_FLAGS: '--c-abi off' }],
  ['noosr', 'No loop re-entry (--no-osr)', { WWT_TRANSLATE_FLAGS: '--no-osr' }],
  ['noinline', 'No inlining (--no-inline)', { WWT_TRANSLATE_FLAGS: '--no-inline' }],
  ['atomics', 'Atomic RMW (--atomics)', { WWT_TRANSLATE_FLAGS: '--atomics' }],
  ['storemap', 'Store map on every store', { WWT_STORE_MAP: 'always' }],
  ['nochecks', 'No memory checks (unsafe bound)', { WWT_TRANSLATE_FLAGS: '--no-mem-checks' }],
  ['faithful', 'Faithful memory checks (no bounds traps)', { WWT_MEM_TRAPS: '0' }],
];
if (process.env.ABL) {
  for (const [label, title, env] of ABL) {
    if (want.length && !want.includes(label)) continue;
    const res = { title, env, benches: {}, sums: {}, errors: {} };
    for (const [wn, w] of Object.entries(W)) {
      run(ABL_ROOT, wargs(w), env);
      const times = {};
      for (let i = 0; i < ROUNDS; i++) {
        const r = run(ABL_ROOT, wargs(w), env);
        const rows = w.parse(r.stdout);
        if (!rows.length) {
          res.errors[wn] = `exit ${r.status}: ${r.stderr.trim().split('\n').slice(-2).join(' | ').slice(0, 300)}`;
          break;
        }
        for (const row of rows) {
          (times[`${wn}/${row.name}`] ??= []).push(row.time);
          res.sums[`${wn}/${row.name}`] = row.sum;
        }
      }
      for (const [k, v] of Object.entries(times)) res.benches[k] = median(v);
      console.log(`${label} ${wn}: ${Object.entries(times).map(([k, v]) => `${k.split('/')[1]} ${median(v).toFixed(3)}`).join(', ') || res.errors[wn]}`);
    }
    results[label] = res;
    writeFileSync(out, JSON.stringify(results, null, 2));
  }
  console.log('ABL-DONE');
  process.exit(0);
}

for (const cp of cps) {
  if (want.length && !want.includes(cp.label)) continue;
  const wt = join(H, `wt-${cp.label}`);
  if (!existsSync(join(wt, 'target/release/wwt'))) {
    console.log(`skip ${cp.label}: not built`);
    continue;
  }
  fileFlag = readFileSync(join(wt, 'runtime/node/wine.mjs'), 'utf8').includes("'--file'");
  const cache = join(wt, 'target/wine-cache');
  if (!process.env.MERGE) rmSync(cache, { recursive: true, force: true });
  const res = { commit: cp.commit, title: cp.title, benches: {}, sums: {}, errors: {} };
  // Cold start: boot with nothing translated (apibench running no benchmark).
  const boot = (dir) => [join(bench, 'suite-apibench.exe'), '1', 'none'];
  if (!process.env.MERGE) {
    const cold = run(wt, boot);
    res.coldStart = cold.wall;
    const warm = [run(wt, boot), run(wt, boot), run(wt, boot)].map((r) => r.wall);
    res.warmStart = median(warm);
    console.log(`${cp.label}: cold ${cold.wall.toFixed(1)}s warm ${res.warmStart.toFixed(2)}s (exit ${cold.status})`);
  }
  for (const [wn, w] of Object.entries(W)) {
    run(wt, wargs(w)); // translate the program
    const times = {};
    for (let i = 0; i < ROUNDS; i++) {
      const r = run(wt, wargs(w));
      const rows = w.parse(r.stdout);
      if (!rows.length) {
        res.errors[wn] = `exit ${r.status}: ${r.stderr.trim().split('\n').slice(-2).join(' | ').slice(0, 300)}`;
        break;
      }
      for (const row of rows) {
        const key = `${wn.replace(/-.*/, '')}/${row.name}`;
        (times[key] ??= []).push(row.time);
        res.sums[key] = row.sum;
      }
    }
    for (const [k, v] of Object.entries(times)) res.benches[k] = median(v);
    console.log(`  ${wn}: ${Object.entries(times).map(([k, v]) => `${k.split('/')[1]} ${median(v).toFixed(3)}`).join(', ') || res.errors[wn]}`);
  }
  // Translated code size.
  res.sizes = {};
  if (!process.env.MERGE) for (const f of readdirSync(cache)) if (f.endsWith('.wasm')) res.sizes[f] = statSync(join(cache, f)).size;
  if (process.env.MERGE && results[cp.label]) {
    const old = results[cp.label];
    for (const [k, v] of Object.entries(res.benches)) old.benches[k] = v;
    for (const [k, v] of Object.entries(res.sums)) old.sums[k] = v;
    old.errors = { ...old.errors, ...res.errors };
  } else results[cp.label] = res;
  writeFileSync(out, JSON.stringify(results, null, 2));
}
console.log('HIST-DONE');
