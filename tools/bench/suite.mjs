#!/usr/bin/env node
// The benchmark suite: real programs and Windows API workloads, each run on
// every tier that can run it, with checksums compared across tiers.
//
//   node tools/bench/suite.mjs [--rounds N] [--only a,b] [--tiers t,u] [--json F] [--pin CPU]
//
// Workloads (built from pinned sources, checked by SHA-256):
//   coremark   CoreMark, a fixed iteration count     (computation)
//   sqlite     SQLite's speedtest1 with --verify     (database engine + file I/O)
//   lua        Lua 5.4 running bench.lua            (interpreter, allocation)
//   apibench   apibench.c: heap, files, strings, sync, callbacks, registry
//
// Tiers:
//   native     the C source built with gcc -m32 for Linux (portable workloads)
//   emcc       the C source built with Emscripten, in Node: the WebAssembly ceiling
//   wine       the .exe on Wine running natively (`wine` on PATH): Wine without translation
//   wwt-wine   the .exe translated, on translated Wine, in Node: what we ship
//   qemu       the Linux build on qemu-i386 (user mode): a native software
//              translator, so how much of our gap is WebAssembly itself
//   wine-assembly  the .exe on Wine-Assembly (an x86 interpreter written in
//              WebAssembly), in Node: the emulation baseline. Opt-in: set
//              WINE_ASSEMBLY to a checkout of github.com/vgrichina/wine-assembly
//              with tools/bench/wine-assembly-console.patch applied (its
//              console output is read at exit). CoreMark only: its C runtime
//              cannot print the other workloads' checksums yet.
//
// --pin CPU runs every measurement on one core (taskset). The JSON records
// the machine and every engine's version.
//
// Every number is a time in seconds (lower is better); a tier's speed is the
// geometric mean over all measurements of native time / its time (or of the
// `wine` tier's time for Windows-only workloads). The translation cache is
// warmed by one untimed run first, so these are warm-start numbers.

import { execFileSync, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
// The runtimes translate with the newest of the release and debug builds;
// benchmarks always use the release one.
process.env.WWT ??= join(root, 'target/release/wwt');
const src = join(root, 'target/bench-src');
const out = join(root, 'target/bench');
const workloads = join(root, 'tools/bench/workloads');

// ---- Options ----

const opts = { rounds: 3, only: null, tiers: null, json: null, pin: null };
const argv = process.argv.slice(2);
while (argv.length) {
  const a = argv.shift();
  if (a === '--rounds') opts.rounds = Number(argv.shift());
  else if (a === '--only') opts.only = argv.shift().split(',');
  else if (a === '--tiers') opts.tiers = argv.shift().split(',');
  else if (a === '--json') opts.json = argv.shift();
  else if (a === '--pin') opts.pin = argv.shift();
  else {
    console.error(`unknown option ${a}`);
    process.exit(2);
  }
}

// ---- Tools ----

const which = (cmd) => spawnSync('sh', ['-c', `command -v ${cmd}`]).status === 0;
function findEmcc() {
  if (which('emcc')) return 'emcc';
  for (const d of [process.env.EMSDK, join(homedir(), 'emsdk')].filter(Boolean)) {
    const e = join(d, 'upstream/emscripten/emcc');
    if (existsSync(e)) return e;
  }
  return null;
}
const emcc = findEmcc();
const wineBuild = process.env.WINE_BUILD ?? '/opt/wine-build';
const available = {
  native: which('gcc'),
  emcc: !!emcc,
  wine: which('wine'),
  'wwt-wine': which('i686-w64-mingw32-gcc') && existsSync(join(root, 'target/release/wwt')) && existsSync(wineBuild),
  qemu: which('gcc') && which('qemu-i386'),
  'wine-assembly': !!process.env.WINE_ASSEMBLY && existsSync(join(process.env.WINE_ASSEMBLY, 'test/run.js')) && which('i686-w64-mingw32-gcc'),
};
const wineAssembly = process.env.WINE_ASSEMBLY;

// ---- Sources ----

const SOURCES = {
  sqlite: {
    url: 'https://www.sqlite.org/2025/sqlite-amalgamation-3500400.zip',
    sha256: '1d3049dd0f830a025a53105fc79fd2ab9431aea99e137809d064d8ee8356b032',
    file: 'sqlite.zip',
    unpack: (f) => execFileSync('unzip', ['-qo', f], { cwd: src }),
  },
  speedtest1: {
    url: 'https://raw.githubusercontent.com/sqlite/sqlite/version-3.50.4/test/speedtest1.c',
    sha256: 'f495cd1c3f727ebf6270d967b43f11a14304053ae4532d6338dbfea65c1a5a78',
    file: 'speedtest1.c',
  },
  lua: {
    url: 'https://www.lua.org/ftp/lua-5.4.7.tar.gz',
    sha256: '9fbf5e28ef86c69858f6d3d34eccc32e911c1a28b4120ff3e84aaa70cfbf1e30',
    file: 'lua.tar.gz',
    unpack: (f) => execFileSync('tar', ['xzf', f], { cwd: src }),
  },
};

function fetchSource(name) {
  const s = SOURCES[name];
  const f = join(src, s.file);
  mkdirSync(src, { recursive: true });
  if (!existsSync(f)) execFileSync('curl', ['-sSfL', '-o', f, s.url], { stdio: 'inherit' });
  const got = createHash('sha256').update(readFileSync(f)).digest('hex');
  if (got !== s.sha256) throw new Error(`${s.file}: SHA-256 ${got}, expected ${s.sha256}`);
  if (s.unpack && !s.unpacked) {
    s.unpack(f);
    s.unpacked = true;
  }
}

// ---- Builds ----

const sh = (cmd, args, cwd) => execFileSync(cmd, args, { cwd, stdio: ['ignore', 'ignore', 'inherit'] });
const COMMON = ['-O2'];

/** Builds `name` for the native, emcc and .exe tiers; returns the outputs. */
function build(name) {
  const o = (suffix) => join(out, `suite-${name}${suffix}`);
  const files = { native: o('.native'), emcc: o('.emcc.js'), exe: o('.exe') };
  const emccFlags = ['--profiling-funcs', '-sALLOW_MEMORY_GROWTH', '-sSTACK_SIZE=1MB', '-mllvm', '--loop-idiom-crc-strategy=disable'];
  const make = (sources, flags, cwd, libs = {}) => {
    if (available.native && !existsSync(files.native)) sh('gcc', ['-m32', ...COMMON, ...flags, ...sources, '-o', files.native, ...(libs.native ?? [])], cwd);
    if (emcc && !existsSync(files.emcc)) sh(emcc, [...COMMON, ...flags, ...emccFlags, ...(libs.emcc ?? []), ...sources, '-o', files.emcc], cwd);
    if (!existsSync(files.exe)) sh('i686-w64-mingw32-gcc', [...COMMON, ...flags, ...sources, '-o', files.exe, ...(libs.exe ?? [])], cwd);
  };
  mkdirSync(out, { recursive: true });
  switch (name) {
    case 'coremark': {
      const cm = join(root, 'target/coremark-src');
      if (!existsSync(cm)) execFileSync(process.execPath, [join(root, 'tools/bench/coremark.mjs'), '--build-only', '--tiers', 'native'], { stdio: 'inherit' });
      const sources = ['core_list_join.c', 'core_main.c', 'core_matrix.c', 'core_state.c', 'core_util.c', 'simple/core_portme.c'];
      const flags = ['-I.', '-Isimple', '-DPERFORMANCE_RUN=1', '-DITERATIONS=30000', '-DFLAGS_STR="-O2"'];
      make(sources, flags, cm);
      // Wine-Assembly's C runtime: msvcrt's printf, not MinGW's (which
      // needs localeconv), and no printf-to-puts rewriting.
      files.waExe = o('.wa.exe');
      if (available['wine-assembly'] && !existsSync(files.waExe)) {
        sh('i686-w64-mingw32-gcc', [...COMMON, '-fno-builtin-printf', '-D__USE_MINGW_ANSI_STDIO=0', ...flags, ...sources, '-o', files.waExe], cm);
      }
      break;
    }
    case 'sqlite': {
      fetchSource('sqlite');
      fetchSource('speedtest1');
      const s = 'sqlite-amalgamation-3500400';
      make(['speedtest1.c', `${s}/sqlite3.c`], ['-DSQLITE_THREADSAFE=0', '-DSQLITE_OMIT_LOAD_EXTENSION', '-DSQLITE_ENABLE_RTREE', `-I${s}`], src, {
        native: ['-lm'],
      });
      break;
    }
    case 'lua': {
      fetchSource('lua');
      const dir = join(src, 'lua-5.4.7/src');
      const sources = readdirSync(dir).filter((f) => f.endsWith('.c') && f !== 'luac.c');
      make(sources, [], dir, { native: ['-lm'], emcc: ['-sNODERAWFS'] });
      break;
    }
    case 'apibench':
      if (!existsSync(files.exe)) sh('i686-w64-mingw32-gcc', [...COMMON, join(workloads, 'apibench.c'), '-o', files.exe, '-ladvapi32']);
      files.native = files.emcc = null;
      break;
  }
  return files;
}

// ---- Workloads ----
//
// Each returns { args, parse(stdout) -> [{ name, time, sum }] } for a tier.

const WORKLOADS = {
  coremark: {
    args: () => [],
    parse(text) {
      const ips = Number(text.match(/^Iterations\/Sec\s*:\s*([0-9.]+)/m)?.[1]);
      const sum = text.match(/^\[0\]crcfinal\s*:\s*(0x[0-9a-f]+)/m)?.[1];
      // Wine-Assembly's printf prints no %f: the tick count (ms) instead.
      const ticks = Number(text.match(/^Total ticks\s*:\s*([0-9]+)/m)?.[1]);
      const time = ips ? 30000 / ips : text.includes('=== console ===') && ticks ? ticks / 1000 : 0;
      return time ? [{ name: 'coremark', time, sum }] : [];
    },
  },
  sqlite: {
    args: (tier, dir) => ['--verify', '--size', '50', tier === 'emcc' ? 'speedtest.db' : join(dir, 'speedtest.db')],
    parse(text) {
      const total = Number(text.match(/TOTAL\.+\s+([0-9.]+)s/)?.[1]);
      const sum = text.match(/^Verification Hash: (.*)$/m)?.[1]?.trim();
      return total ? [{ name: 'speedtest1', time: total, sum }] : [];
    },
  },
  lua: {
    args: (tier) => [tier === 'native' || tier === 'emcc' ? join(workloads, 'bench.lua') : 'C:\\bench.lua', '4'],
    files: [[join(workloads, 'bench.lua'), 'C:\\bench.lua']],
    parse: (text) => columns(text),
  },
  apibench: {
    args: () => ['2'],
    parse: (text) => columns(text),
  },
};

/** Lines of "name checksum seconds". */
function columns(text) {
  const rows = [];
  for (const m of text.matchAll(/^(\w+)\s+(\S+)\s+([0-9.]+)\s*$/gm)) rows.push({ name: m[1], sum: m[2], time: Number(m[3]) });
  return rows;
}

// ---- Running ----

function command(tier, files, w, dir) {
  const args = w.args(tier, dir);
  switch (tier) {
    case 'native':
      return files.native && [files.native, args];
    case 'emcc':
      return files.emcc && [process.execPath, [files.emcc, ...args]];
    case 'wine':
      return [
        'wine',
        [files.exe.replace(/^\//, 'Z:\\').replaceAll('/', '\\'), ...args.map((a) => (a.startsWith('C:\\') ? `Z:${join(workloads, a.slice(3)).replaceAll('/', '\\')}` : a))],
      ];
    case 'qemu':
      return files.native && ['qemu-i386', [files.native, ...args]];
    case 'wine-assembly':
      return (
        files.waExe && [
          process.execPath,
          [
            join(wineAssembly, 'test/run.js'),
            `--exe=${files.waExe}`,
            ...(args.length ? [`--args=${args.join(' ')}`] : []),
            '--max-batches=1000000000',
            '--stuck-after=0',
            '--max-seconds=3600',
            '--quiet-api',
            '--quiet-blocks',
            '--no-renderer',
            // The program's clock in real time, not the batch clock.
            '--real-ticks',
          ],
        ]
      );
    case 'wwt-wine':
      return [
        process.execPath,
        [join(root, 'runtime/node/wine.mjs'), ...(w.files ?? []).flatMap(([h, d]) => ['--file', `${h}=${d}`]), files.exe, ...args],
      ];
  }
}

function runOnce(tier, files, w) {
  const dir = mkdtempDir();
  const cmd = command(tier, files, w, dir);
  if (!cmd) return null;
  const env = { ...process.env, WINEDEBUG: '-all', WINEPREFIX: process.env.WINEPREFIX ?? join(tmpdir(), 'wwt-bench-wineprefix'), WA_DUMP_CONSOLE: '80' };
  const [exe, args] = opts.pin ? ['taskset', ['-c', opts.pin, cmd[0], ...cmd[1]]] : cmd;
  const r = spawnSync(exe, args, { cwd: dir, env, encoding: 'utf8', maxBuffer: 256 << 20, timeout: 60 * 60 * 1000 });
  rmSync(dir, { recursive: true, force: true });
  const rows = w.parse(r.stdout ?? '');
  if (!rows.length) {
    console.error(`  ${tier}: no results (exit ${r.status ?? r.signal})\n${(r.stderr ?? '').trim().split('\n').slice(-3).join('\n')}`);
    return null;
  }
  return rows;
}

let tmpCount = 0;
function mkdtempDir() {
  const d = join(tmpdir(), `wwt-suite-${process.pid}-${tmpCount++}`);
  mkdirSync(d, { recursive: true });
  return d;
}

const median = (xs) => {
  const s = [...xs].sort((a, b) => a - b);
  return s.length ? (s[(s.length - 1) >> 1] + s[s.length >> 1]) / 2 : NaN;
};

const ALL_TIERS = ['native', 'emcc', 'wine', 'qemu', 'wine-assembly', 'wwt-wine'];
const TIERS = ALL_TIERS.filter((t) => available[t] && (!opts.tiers || opts.tiers.includes(t)));
for (const t of ALL_TIERS) if (!available[t]) console.error(`skipping tier ${t} (not installed${t === 'wine-assembly' ? ': set WINE_ASSEMBLY' : ''})`);

const results = [];
for (const name of Object.keys(WORKLOADS)) {
  if (opts.only && !opts.only.includes(name)) continue;
  const w = WORKLOADS[name];
  const files = build(name);
  const tiers = TIERS.filter((t) => command(t, files, w, '/tmp'));
  // Warm the translation cache (and the OS caches) with one untimed run.
  if (tiers.includes('wwt-wine')) runOnce('wwt-wine', files, w);
  const samples = {};
  for (let r = 0; r < opts.rounds; r++) {
    for (const t of tiers) {
      const rows = runOnce(t, files, w);
      for (const row of rows ?? []) ((samples[row.name] ??= {})[t] ??= []).push(row);
    }
    process.stderr.write(`${name}: round ${r + 1}/${opts.rounds}\r`);
  }
  process.stderr.write('\n');
  for (const [bench, byTier] of Object.entries(samples)) {
    const row = { workload: name, bench, times: {}, sums: {} };
    for (const [t, rs] of Object.entries(byTier)) {
      row.times[t] = median(rs.map((x) => x.time));
      row.sums[t] = rs[0].sum;
    }
    results.push(row);
  }
}

// ---- Report ----

const ref = (row) => (row.times.native !== undefined ? 'native' : 'wine');
const fmt = (x) => (x === undefined ? '—' : x < 10 ? x.toFixed(3) : x.toFixed(2));
console.log(`\nBenchmark suite, Node ${process.version}, median of ${opts.rounds}; seconds (lower is better)`);
const W = 14;
console.log(`${'workload/bench'.padEnd(22)}${TIERS.map((t) => t.padStart(W)).join('')}   wwt-wine vs ref   checksums`);
let mismatches = 0;
for (const row of results) {
  const r = ref(row);
  const sums = Object.values(row.sums);
  const same = sums.every((s) => s === sums[0]);
  if (!same) mismatches++;
  const ratio = row.times['wwt-wine'] && row.times[r] ? `${(row.times[r] / row.times['wwt-wine'] * 100).toFixed(0)}% of ${r}` : '';
  console.log(`${`${row.workload}/${row.bench}`.padEnd(22)}${TIERS.map((t) => fmt(row.times[t]).padStart(W)).join('')}   ${ratio.padEnd(16)}  ${same ? 'ok' : 'MISMATCH ' + JSON.stringify(row.sums)}`);
}
// Geometric mean speed of each tier relative to the reference tier.
console.log('');
for (const t of TIERS) {
  const logs = [];
  for (const row of results) {
    const r = ref(row);
    if (t === r || !(row.times[t] > 0) || !(row.times[r] > 0)) continue;
    // Only rows whose results agree: a tier that fails a workload is not fast.
    const sums = Object.values(row.sums);
    if (!sums.every((x) => x === sums[0])) continue;
    logs.push(Math.log(row.times[r] / row.times[t]));
  }
  if (logs.length) console.log(`${t.padEnd(W)} geometric mean ${(Math.exp(logs.reduce((a, b) => a + b, 0) / logs.length) * 100).toFixed(0)}% of reference speed over ${logs.length} measurements`);
}
// The machine and the engines, so runs on different days or machines can be
// told apart.
function machine() {
  const out = (cmd, args) => {
    const r = spawnSync(cmd, args, { encoding: 'utf8' });
    return r.status === 0 ? (r.stdout || r.stderr).trim().split('\n')[0] : null;
  };
  const cpu = readFileSync('/proc/cpuinfo', 'utf8').match(/^model name\s*:\s*(.*)$/m)?.[1];
  return {
    cpu,
    cpus: readFileSync('/proc/cpuinfo', 'utf8').match(/^processor/gm)?.length,
    kernel: out('uname', ['-r']),
    governor: existsSync('/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor') ? readFileSync('/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor', 'utf8').trim() : null,
    pinned: opts.pin,
    node: process.version,
    v8: process.versions.v8,
    gcc: out('gcc', ['--version']),
    mingw: out('i686-w64-mingw32-gcc', ['--version']),
    emcc: emcc && out(emcc, ['--version']),
    wine: out('wine', ['--version']),
    qemu: out('qemu-i386', ['--version']),
    wineAssembly: wineAssembly && out('git', ['-C', wineAssembly, 'rev-parse', '--short', 'HEAD']),
    wwt: out('git', ['-C', root, 'rev-parse', '--short', 'HEAD']),
  };
}
if (opts.json) writeFileSync(opts.json, JSON.stringify({ date: new Date().toISOString(), node: process.version, rounds: opts.rounds, machine: machine(), results }, null, 2) + '\n');
process.exit(mismatches ? 1 : 0);
