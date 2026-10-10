#!/usr/bin/env node
// Builds the benchmark programs the page offers under "Benchmarks", from
// their pinned sources (tools/bench/sources.mjs: SQLite's speedtest1, Lua,
// CoreMark) and tools/bench/workloads, as Windows programs (MinGW, -O2).
// Nothing built here is committed: CI runs this before deploying the site,
// and tools/site/build.sh copies the result next to the page.
//
//   node tools/site/apps.mjs [out dir]        (default: target/site-apps)
//
// The out dir gets apps/*.exe (and the files they read) and apps.json, the
// page's list of them (the format of runtime/web/samples.json, plus
// `files`: name -> path, put in C:\app next to the program). Needs curl,
// unzip, git and i686-w64-mingw32-gcc; sources are cached in
// target/bench-src and target/coremark-src.

import { execFileSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, readdirSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { COREMARK_FILES, SOURCES, fetchCoremark, fetchSource } from '../bench/sources.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const out = resolve(process.argv[2] ?? join(root, 'target/site-apps'));
const src = join(root, 'target/bench-src');
const coremark = join(root, 'target/coremark-src');
const workloads = join(root, 'tools/bench/workloads');
const apps = join(out, 'apps');
mkdirSync(apps, { recursive: true });

const cc = (sources, flags, exe, cwd, libs = []) => {
  const file = join(apps, exe);
  execFileSync('i686-w64-mingw32-gcc', ['-O2', ...flags, ...sources, '-o', file, ...libs, '-Wl,--no-insert-timestamp'], { cwd, stdio: ['ignore', 'ignore', 'inherit'] });
  console.log(`built apps/${exe}`);
};

// SQLite's speedtest1 on the amalgamation, as the suite builds it.
fetchSource('sqlite', src);
fetchSource('speedtest1', src);
const s = SOURCES.sqlite.dir;
cc(['speedtest1.c', `${s}/sqlite3.c`], ['-DSQLITE_THREADSAFE=0', '-DSQLITE_OMIT_LOAD_EXTENSION', '-DSQLITE_ENABLE_RTREE', `-I${s}`], 'speedtest1.exe', src);

// Lua 5.4 and the suite's Lua workloads.
fetchSource('lua', src);
const luaDir = join(src, SOURCES.lua.dir, 'src');
cc(readdirSync(luaDir).filter((f) => f.endsWith('.c') && f !== 'luac.c'), [], 'lua.exe', luaDir);
copyFileSync(join(workloads, 'bench.lua'), join(apps, 'bench.lua'));

// CoreMark, calibrated (ITERATIONS=0: it runs for at least 10 seconds).
fetchCoremark(coremark);
cc(COREMARK_FILES, ['-I.', '-Isimple', '-DPERFORMANCE_RUN=1', '-DITERATIONS=0', '-DFLAGS_STR="-O2"'], 'coremark.exe', coremark);

// The Win32 API microbenchmarks.
cc([join(workloads, 'apibench.c')], [], 'apibench.exe', root, ['-ladvapi32']);

const group = 'Benchmarks (built from source)';
const list = [
  {
    group,
    path: 'apps/speedtest1.exe',
    label: 'SQLite 3.50.4 speedtest1 (--size 50; about 20 s)',
    args: '--verify --size 50 C:\\app\\speedtest.db',
  },
  {
    group,
    path: 'apps/lua.exe',
    label: 'Lua 5.4.7: fib, tables, strings, sort, objects, float (about 10 s)',
    args: 'C:\\app\\bench.lua 4',
    files: { 'bench.lua': 'apps/bench.lua' },
  },
  { group, path: 'apps/coremark.exe', label: 'CoreMark (runs for 10 s or more)' },
  { group, path: 'apps/apibench.exe', label: 'apibench: heap, files, strings, sync, callbacks, registry' },
];
for (const a of list) {
  for (const p of [a.path, ...Object.values(a.files ?? {})]) if (!existsSync(join(out, p))) throw new Error(`missing ${p}`);
}
writeFileSync(join(out, 'apps.json'), JSON.stringify(list, null, 2) + '\n');
console.log(`wrote ${join(out, 'apps.json')}`);
