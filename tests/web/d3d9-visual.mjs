// Wine's Direct3D 9 rendering tests (dlls/d3d9/tests/visual.c: about 130
// test functions that draw and read pixels back) on translated Wine in
// headless Chromium, whose WebGPU draws the frames.
//
//   node tests/web/d3d9-visual.mjs [--batch N] [--timeout S] [--only A-B] [--present gdi|canvas]
//        [--native] [--baseline FILE] [--write-baseline FILE] [--build-only]
//
// visual.c runs every function from one START_TEST, and some hang or crash
// on this backend, so the test is rebuilt with its calls numbered: argv
// "visual A-B" runs functions A to B-1 (target/d3d9-visual/d3d9_test.exe,
// from the Wine build in /opt/wine-build). Each batch of N (default 10) runs
// in its own page; a batch that hangs or crashes is recorded with the
// function it was in. Results, failures per function, go to
// target/d3d9-visual/results.json (each batch's output in batch-A-B.txt); with --baseline the run fails when a
// function has more failures than recorded (or now hangs or crashes).
//
// --native runs the same program on native Wine instead ($WINE, default
// /opt/wine-native/build/wine, on $DISPLAY: Xvfb with Mesa's llvmpipe here),
// for what Wine's own OpenGL backend gets (tests/web/baseline/d3d9_visual_native.json).
// The page's GPU self-test runs it on the viewer's GPU and compares with both;
// --build-only builds it (and d3d9_test.json, its functions' names) for the site.

import { execFileSync, spawn } from 'node:child_process';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseVisual, totals } from '../../runtime/web/d3d9-visual.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const argv = process.argv.slice(2);
const opt = (name, def) => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 ? argv.splice(i, 2)[1] : def;
};
const batch = Number(opt('batch', 10));
const timeout = Number(opt('timeout', 300)) * 1000;
const only = opt('only', '');
// How frames are shown (?d3dpresent=): gdi reads them back into the window;
// canvas presents them to a WebGPU canvas, as the page does by default.
const present = opt('present', 'gdi');
const baseline = opt('baseline', '');
const writeBaseline = opt('write-baseline', '');
const native = argv.includes('--native');
const WINE_SRC = process.env.WINE_SRC ?? '/opt/wine-src/wine-11.0';
const WINE_BUILD = process.env.WINE_BUILD ?? '/opt/wine-build';
const out = join(root, native ? 'target/d3d9-visual-native' : 'target/d3d9-visual');
// The program and its functions' names (the page's self-test reads both).
const appDir = join(root, 'target/d3d9-visual/app');
mkdirSync(appDir, { recursive: true });
mkdirSync(out, { recursive: true });

// ---- The numbered test -----------------------------------------------------

const src = readFileSync(join(WINE_SRC, 'dlls/d3d9/tests/visual.c'), 'utf8');
const start = src.indexOf('START_TEST(visual)');
const body = src.slice(start);
const afterRelease = body.indexOf('IDirect3D9_Release(d3d);');
const names = [];
const numbered = body.slice(0, afterRelease) + body.slice(afterRelease).replace(/^ {4}(\w+)\(\);$/gm, (_, name) => {
  names.push(name);
  return `    if (wwt_run(${names.length - 1}, "${name}")) ${name}();`;
});
const helper = `
/* Runs functions A to B-1 of START_TEST when argv[2] is "A-B" (tests/web/d3d9-visual.mjs). */
static BOOL wwt_run(int i, const char *name)
{
    static int lo = -1, hi;
    if (lo < 0)
    {
        char **argv;
        int argc = winetest_get_mainargs(&argv);
        lo = 0;
        hi = 1 << 30;
        if (argc > 2) sscanf(argv[2], "%d-%d", &lo, &hi);
    }
    if (i < lo || i >= hi) return FALSE;
    trace("wwt function %d %s\\n", i, name);
    return TRUE;
}

`;
const patched = join(root, 'target/d3d9-visual/visual.c');
writeFileSync(patched, src.slice(0, start) + helper + numbered);
const exe = join(appDir, 'd3d9_test.exe');
const tdir = 'dlls/d3d9/tests';
const obj = join(root, 'target/d3d9-visual/visual.o');
execFileSync('i686-w64-mingw32-gcc', ['-c', '-o', obj, patched, `-I${WINE_BUILD}/${tdir}`, `-I${WINE_SRC}/${tdir}`,
  `-I${WINE_BUILD}/include`, `-I${WINE_SRC}/include`, `-I${WINE_SRC}/include/msvcrt`, '-D_MSVCR_VER=0',
  '-D__WINESRC__', '-D__WINE_PE_BUILD', '-fno-strict-aliasing', '-fno-omit-frame-pointer',
  '-mpreferred-stack-boundary=2', '-O2', '-w'], { stdio: 'inherit' });
const o = (n) => `${tdir}/i386-windows/${n}.o`;
execFileSync('tools/winegcc/winegcc', ['-o', exe, '--wine-objdir', '.', '-b', 'i686-w64-mingw32',
  o('d3d9ex'), o('device'), o('stateblock'), obj, o('testlist'), 'dlls/d3d9/i386-windows/libd3d9.a',
  'dlls/user32/i386-windows/libuser32.a', 'dlls/gdi32/i386-windows/libgdi32.a',
  'dlls/winecrt0/i386-windows/libwinecrt0.a', 'dlls/msvcrt/i386-windows/libmsvcrt.a',
  'dlls/kernel32/i386-windows/libkernel32.a', 'dlls/ntdll/i386-windows/libntdll.a',
  '-Wl,--disable-stdcall-fixup'], { cwd: WINE_BUILD, stdio: 'inherit' });
writeFileSync(join(appDir, 'd3d9_test.json'), JSON.stringify({ names }) + '\n');
console.log(`${names.length} test functions in visual.c`);
if (process.argv.includes('--build-only')) process.exit(0);

// ---- Running ---------------------------------------------------------------

let [lo, hi] = only ? only.split('-').map(Number) : [0, names.length];
const ranges = [];
for (let a = lo; a < hi; a += batch) ranges.push([a, Math.min(a + batch, hi)]);

const results = {};
// One batch: its output, and whether the program exited (not stopped for time).
let runBatch, finish = async () => {};
if (native) {
  const wine = process.env.WINE ?? '/opt/wine-native/build/wine';
  runBatch = (a, b) =>
    new Promise((res) => {
      const p = spawn(wine, ['d3d9_test.exe', 'visual', `${a}-${b}`], {
        cwd: appDir,
        env: { WINEDEBUG: '-all', WINEDLLOVERRIDES: 'mscoree,mshtml=', ...process.env },
        stdio: ['ignore', 'pipe', 'pipe'],
      });
      let text = '';
      p.stdout.on('data', (d) => (text += d));
      p.stderr.on('data', (d) => (text += d));
      const timer = setTimeout(() => p.kill('SIGKILL'), timeout);
      p.on('close', (code, signal) => (clearTimeout(timer), res({ text, exited: signal !== 'SIGKILL', log: '' })));
    });
} else {
  const port = 19611;
  const server = spawn(process.execPath, [join(root, 'runtime/web/serve.mjs'), String(port), root], { stdio: 'ignore' });
  await new Promise((r) => setTimeout(r, 500));
  const profile = join(out, 'profile');
  const require = createRequire(import.meta.url);
  let chromium;
  try {
    ({ chromium } = require('playwright'));
  } catch {
    ({ chromium } = require(join(process.execPath, '../../lib/node_modules/playwright')));
  }
  const launch = { args: ['--enable-unsafe-webgpu'], viewport: { width: 1000, height: 1000 } };
  const context = await chromium.launchPersistentContext(profile, launch);
  finish = async () => (await context.close(), server.kill());
  runBatch = async (a, b) => {
    const page = await context.newPage();
    await page.goto(`http://localhost:${port}/runtime/web/?wine=1&d3dpresent=${present}`);
    await page.setInputFiles('#fallback', appDir);
    await page.waitForFunction(() => !document.getElementById('run').disabled);
    await page.selectOption('#exe', 'd3d9_test.exe');
    await page.fill('#args', `visual ${a}-${b}`);
    await page.check('#wine');
    await page.click('#run');
    const exited = await page.waitForFunction(() => window.lastExit !== undefined, null, { timeout }).then(() => true, () => false);
    const rec = await page.evaluate(() => window.webwindows?.lastRun());
    const log = await page.textContent('#log');
    await page.close();
    return { text: rec?.out ?? '', exited, log };
  };
}
try {
  // What a batch did not get to runs after (see parseVisual).
  for (let k = 0; k < ranges.length; k++) {
    const [a, b] = ranges[k];
    const t0 = Date.now();
    const { text, exited, log } = await runBatch(a, b);
    writeFileSync(join(out, `batch-${a}-${b}.txt`), `${text}\n---- page log ----\n${log}`);
    const batchResults = parseVisual(text, a, b, names, exited);
    Object.assign(results, batchResults.results);
    ranges.push(...batchResults.next);
    const t = totals(Object.fromEntries(names.slice(a, b).map((n) => [n, results[n]])));
    const bad = names.slice(a, b).filter((k) => ['crash', 'timeout'].includes(results[k]?.status));
    console.log(`${String(a).padStart(3)}-${String(b).padEnd(3)} ${t.failures} failures${bad.length ? `, ${bad.map((n) => `${n} ${results[n].status}`).join(', ')}` : ''} (${((Date.now() - t0) / 1000).toFixed(0)} s)`);
  }
} finally {
  await finish();
}

writeFileSync(join(out, 'results.json'), JSON.stringify(results, null, 2) + '\n');
const t = totals(results);
console.log(`d3d9 visual${native ? ' (native Wine)' : ''}: ${t.functions} functions, ${t.failures} failures, ${t.broken} crashed, hung or not run`);
// The baseline keeps the counts, not the failure lines.
const counts = Object.fromEntries(Object.entries(results).map(([k, { failed, ...r }]) => [k, r]));
if (writeBaseline) writeFileSync(writeBaseline, JSON.stringify(counts, null, 2) + '\n');
if (baseline) {
  const base = JSON.parse(readFileSync(baseline, 'utf8'));
  let regressions = 0;
  for (const [name, r] of Object.entries(results)) {
    const b = base[name];
    if (!b) continue;
    const worse = (b.status === 'done' && r.status !== 'done') || (r.status === 'done' && b.status === 'done' && r.failures > b.failures);
    if (worse) {
      regressions++;
      console.log(`REGRESSION ${name}: ${r.status} ${r.failures ?? ''} (baseline ${b.status} ${b.failures ?? ''})`);
    } else if (r.status === 'done' && (b.status !== 'done' || r.failures < b.failures)) {
      console.log(`improved   ${name}: ${r.failures} failures (baseline ${b.status} ${b.failures ?? ''})`);
    }
  }
  if (regressions) process.exit(1);
}
