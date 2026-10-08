#!/usr/bin/env node
// Size and heat map of one benchmark's WebAssembly, Emscripten's build next
// to the translated .exe: every function's code size, the CPU time it took
// (V8's sampling profiler, self time), and the two matched by name. Writes
// the data as JSON and a self-contained HTML page with both modules as
// treemaps (area = bytes, color = time) and a sortable comparison table.
//
//   node tools/bench/wasm-map.mjs lua|sqlite|coremark [--out DIR] [--no-profile]
//
// Needs the suite's builds (node tools/bench/suite.mjs builds them) and the
// translator. The translated side runs on translated Wine (wine.mjs), so its
// time includes Wine's DLLs; their sizes are listed apart from the .exe's.

import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const bench = join(root, 'target/bench');
const workloads = join(root, 'tools/bench/workloads');

const argv = process.argv.slice(2);
let workload = null;
let out = join(root, 'target/wasm-map');
let profile = true;
while (argv.length) {
  const a = argv.shift();
  if (a === '--out') out = resolve(argv.shift());
  else if (a === '--no-profile') profile = false;
  else workload = a;
}
const RUNS = {
  lua: { emcc: (d) => [join(workloads, 'bench.lua'), '4'], wine: ['--file', `${join(workloads, 'bench.lua')}=C:\\bench.lua`], args: ['C:\\bench.lua', '4'] },
  sqlite: { emcc: (d) => ['--size', '50', join(d, 'speedtest.db')], wine: [], args: (d) => ['--size', '50', join(d, 'speedtest.db')] },
  coremark: { emcc: () => [], wine: [], args: [] },
};
if (!RUNS[workload]) {
  console.error('usage: node tools/bench/wasm-map.mjs lua|sqlite|coremark [--out DIR] [--no-profile]');
  process.exit(2);
}
const run = RUNS[workload];
const exe = join(bench, `suite-${workload}.exe`);
const emccJs = join(bench, `suite-${workload}.emcc.js`);
const emccWasm = join(bench, `suite-${workload}.emcc.wasm`);
for (const f of [exe, emccJs, emccWasm]) {
  if (!existsSync(f)) {
    console.error(`missing ${f}: run node tools/bench/suite.mjs --only ${workload} first`);
    process.exit(1);
  }
}

// ---- WebAssembly: function sizes and names ----

function leb(b, p) {
  let r = 0;
  let s = 0;
  for (;;) {
    const x = b[p.i++];
    r += (x & 0x7f) * 2 ** s;
    s += 7;
    if (!(x & 0x80)) return r;
  }
}
function str(b, p) {
  const n = leb(b, p);
  const s = new TextDecoder().decode(b.subarray(p.i, p.i + n));
  p.i += n;
  return s;
}

/** [{name, size}] for the module's own functions, plus totals by section. */
function functionSizes(bytes) {
  const b = new Uint8Array(bytes);
  const p = { i: 8 };
  let imported = 0;
  const sizes = [];
  const names = new Map();
  const sections = {};
  while (p.i < b.length) {
    const id = b[p.i++];
    const len = leb(b, p);
    const end = p.i + len;
    const q = { i: p.i };
    if (id === 2) {
      for (let n = leb(b, q); n > 0; n--) {
        str(b, q);
        str(b, q);
        const kind = b[q.i++];
        if (kind === 0) {
          leb(b, q);
          imported++;
        } else if (kind === 1) {
          q.i++;
          const f = leb(b, q);
          leb(b, q);
          if (f & 1) leb(b, q);
        } else if (kind === 2) {
          const f = leb(b, q);
          leb(b, q);
          if (f & 1) leb(b, q);
        } else if (kind === 3) {
          q.i += 2;
        } else if (kind === 4) {
          q.i++;
          leb(b, q);
        }
      }
    } else if (id === 10) {
      for (let n = leb(b, q); n > 0; n--) {
        const size = leb(b, q);
        sizes.push(size);
        q.i += size;
      }
    } else if (id === 0) {
      const name = str(b, q);
      sections[`custom:${name}`] = len;
      if (name === 'name') {
        while (q.i < end) {
          const sub = b[q.i++];
          const sl = leb(b, q);
          const se = q.i + sl;
          if (sub === 1) {
            for (let n = leb(b, q); n > 0; n--) {
              const idx = leb(b, q);
              names.set(idx, str(b, q));
            }
          }
          q.i = se;
        }
      }
    }
    if (id !== 0) sections[['custom', 'type', 'import', 'function', 'table', 'memory', 'global', 'export', 'start', 'element', 'code', 'data', 'datacount', 'tag'][id] ?? `section ${id}`] = len;
    p.i = end;
  }
  return {
    total: b.length,
    sections,
    funcs: sizes.map((size, i) => ({ name: names.get(imported + i) ?? `func${imported + i}`, size })),
  };
}

/** One name for the same C function in both builds. */
function key(name) {
  return name
    .replace(/^\$/, '')
    .replace(/@[0-9a-f]+$/, '') // translated: symbol@address
    .replace(/@\d+$/, '') // stdcall decoration
    .replace(/^_/, '') // MinGW's leading underscore
    .replace(/\.(isra|part|constprop|cold|lto_priv)\.\d+/g, '');
}

// ---- Profiles ----

function profileRun(args, cwd) {
  const dir = mkdtempSync(join(tmpdir(), 'wwt-map-'));
  const r = spawnSync(process.execPath, ['--cpu-prof', `--cpu-prof-dir=${dir}`, ...args], { cwd, encoding: 'utf8', maxBuffer: 256 << 20 });
  const files = readdirSync(dir).filter((f) => f.endsWith('.cpuprofile'));
  if (!files.length) throw new Error(`no profile written\n${r.stderr}`);
  const self = new Map();
  let total = 0;
  for (const file of files) {
    const p = JSON.parse(readFileSync(join(dir, file), 'utf8'));
    const byId = new Map(p.nodes.map((n) => [n.id, n]));
    p.samples.forEach((id, i) => {
      const f = byId.get(id).callFrame;
      const dt = (p.timeDeltas[i] || 0) / 1e6;
      const name = f.functionName || '(anonymous)';
      if (name === '(idle)') return;
      const wasm = /wasm/.test(f.url) || /@[0-9a-f]+$/.test(name);
      const k = wasm ? name : `(JavaScript) ${name}`;
      self.set(k, (self.get(k) || 0) + dt);
      total += dt;
    });
  }
  rmSync(dir, { recursive: true, force: true });
  return { total, self, stdout: r.stdout };
}

// ---- Collect ----

console.error(`${workload}: translating and warming the cache`);
const scratch = mkdtempSync(join(tmpdir(), 'wwt-map-run-'));
const wineArgs = [join(root, 'runtime/node/wine.mjs'), ...run.wine, exe, ...(typeof run.args === 'function' ? run.args(scratch) : run.args)];
spawnSync(process.execPath, wineArgs, { cwd: scratch, encoding: 'utf8', maxBuffer: 256 << 20 });
// The translated .exe as wine.mjs ran it: the newest cache entry for it.
const cache = join(root, 'target/wine-cache');
const mine = readdirSync(cache)
  .filter((f) => f.startsWith(`suite-${workload}.exe-`) && f.endsWith('.wasm'))
  .map((f) => join(cache, f))
  .sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs);
if (!mine.length) throw new Error('translated module not found in target/wine-cache');
const wwtMod = functionSizes(readFileSync(mine[0]));
const emccMod = functionSizes(readFileSync(emccWasm));

let emccProf = null;
let wwtProf = null;
if (profile) {
  console.error(`${workload}: profiling Emscripten's build`);
  emccProf = profileRun([emccJs, ...run.emcc(scratch)], scratch);
  console.error(`${workload}: profiling the translated build on translated Wine`);
  wwtProf = profileRun(wineArgs, scratch);
}
rmSync(scratch, { recursive: true, force: true });

function side(mod, prof) {
  const own = new Set(mod.funcs.map((f) => f.name));
  const funcs = mod.funcs.map((f) => ({ name: f.name, key: key(f.name), size: f.size, time: prof?.self.get(f.name) ?? 0 }));
  // Time in functions of other modules (Wine's DLLs, the runtime).
  const other = [];
  if (prof) {
    for (const [name, t] of prof.self) if (!own.has(name) && t > 0) other.push({ name, key: key(name), time: t });
    other.sort((a, b) => b.time - a.time);
  }
  return { total: mod.total, sections: mod.sections, code: mod.funcs.reduce((a, f) => a + f.size, 0), time: prof?.total ?? 0, funcs, other: other.slice(0, 200) };
}
const data = {
  workload,
  date: new Date().toISOString(),
  node: process.version,
  emcc: { file: emccWasm.slice(root.length + 1), ...side(emccMod, emccProf) },
  wwt: { file: mine[0].slice(root.length + 1), ...side(wwtMod, wwtProf) },
};
// Matched by name.
const byKey = (fs) => {
  const m = new Map();
  for (const f of fs) {
    const e = m.get(f.key) ?? { size: 0, time: 0, names: [] };
    e.size += f.size ?? 0;
    e.time += f.time;
    e.names.push(f.name);
    m.set(f.key, e);
  }
  return m;
};
const a = byKey(data.emcc.funcs);
const b = byKey([...data.wwt.funcs, ...data.wwt.other.filter((f) => !f.name.startsWith('('))]);
data.matched = [...a.keys()]
  .filter((k) => b.has(k))
  .map((k) => ({ key: k, emccSize: a.get(k).size, wwtSize: b.get(k).size, emccTime: a.get(k).time, wwtTime: b.get(k).time }));

mkdirSync(out, { recursive: true });
const jsonPath = join(out, `${workload}.json`);
writeFileSync(jsonPath, JSON.stringify(data));
const htmlPath = join(out, `${workload}.html`);
writeFileSync(htmlPath, readFileSync(join(root, 'tools/bench/wasm-map.html'), 'utf8').replace('/*DATA*/null', JSON.stringify(data)));
const kb = (n) => `${(n / 1024).toFixed(0)} KB`;
console.log(`${workload}: Emscripten ${kb(data.emcc.code)} of code in ${data.emcc.funcs.length} functions, translated .exe ${kb(data.wwt.code)} in ${data.wwt.funcs.length}; ${data.matched.length} matched by name`);
if (profile) console.log(`time: Emscripten ${data.emcc.time.toFixed(2)} s, translated ${data.wwt.time.toFixed(2)} s (sampled, idle excluded)`);
console.log(`wrote ${htmlPath}`);
