// Wine's Direct3D 9 rendering tests (dlls/d3d9/tests/visual.c: about 130
// test functions that draw and read pixels back) on translated Wine in
// headless Chromium, whose WebGPU draws the frames.
//
//   node tests/web/d3d9-visual.mjs [--batch N] [--timeout S] [--only A-B]
//        [--baseline FILE] [--write-baseline FILE]
//
// visual.c runs every function from one START_TEST, and some hang or crash
// on this backend, so the test is rebuilt with its calls numbered: argv
// "visual A-B" runs functions A to B-1 (target/d3d9-visual/d3d9_test.exe,
// from the Wine build in /opt/wine-build). Each batch of N (default 10) runs
// in its own page; a batch that hangs or crashes is recorded with the
// function it was in. Results, failures per function, go to
// target/d3d9-visual/results.json (each batch's output in batch-A-B.txt); with --baseline the run fails when a
// function has more failures than recorded (or now hangs or crashes).

import { execFileSync, spawn } from 'node:child_process';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
let chromium;
try {
  ({ chromium } = require('playwright'));
} catch {
  ({ chromium } = require(join(process.execPath, '../../lib/node_modules/playwright')));
}
const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const argv = process.argv.slice(2);
const opt = (name, def) => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 ? argv.splice(i, 2)[1] : def;
};
const batch = Number(opt('batch', 10));
const timeout = Number(opt('timeout', 300)) * 1000;
const only = opt('only', '');
const baseline = opt('baseline', '');
const writeBaseline = opt('write-baseline', '');
const WINE_SRC = process.env.WINE_SRC ?? '/opt/wine-src/wine-11.0';
const WINE_BUILD = process.env.WINE_BUILD ?? '/opt/wine-build';
const out = join(root, 'target/d3d9-visual');
const appDir = join(out, 'app');
mkdirSync(appDir, { recursive: true });

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
const patched = join(out, 'visual.c');
writeFileSync(patched, src.slice(0, start) + helper + numbered);
const exe = join(appDir, 'd3d9_test.exe');
const tdir = 'dlls/d3d9/tests';
const obj = join(out, 'visual.o');
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
console.log(`${names.length} test functions in visual.c`);

// ---- Running ---------------------------------------------------------------

let [lo, hi] = only ? only.split('-').map(Number) : [0, names.length];
const ranges = [];
for (let a = lo; a < hi; a += batch) ranges.push([a, Math.min(a + batch, hi)]);

const port = 19611;
const server = spawn(process.execPath, [join(root, 'runtime/web/serve.mjs'), String(port), root], { stdio: 'ignore' });
await new Promise((r) => setTimeout(r, 500));
const profile = join(out, 'profile');
const launch = { args: ['--enable-unsafe-webgpu'], viewport: { width: 1000, height: 1000 } };
const context = await chromium.launchPersistentContext(profile, launch);
const results = {};
try {
  // A function that ran in a batch that finished, but whose trace line the
  // run record dropped (a long output keeps only its end), runs again alone.
  for (let k = 0; k < ranges.length; k++) {
    const [a, b] = ranges[k];
    const page = await context.newPage();
    const t0 = Date.now();
    await page.goto(`http://localhost:${port}/runtime/web/?wine=1&d3dpresent=gdi`);
    await page.setInputFiles('#fallback', appDir);
    await page.waitForFunction(() => !document.getElementById('run').disabled);
    await page.selectOption('#exe', 'd3d9_test.exe');
    await page.fill('#args', `visual ${a}-${b}`);
    await page.check('#wine');
    await page.click('#run');
    const exited = await page.waitForFunction(() => window.lastExit !== undefined, null, { timeout }).then(() => true, () => false);
    const rec = await page.evaluate(() => window.webwindows?.lastRun());
    const text = rec?.out ?? '';
    const log = await page.textContent('#log');
    await page.close();
    writeFileSync(join(out, `batch-${a}-${b}.txt`), `${text}\n---- page log ----\n${log}`);
    // Failures per function, from the order of the trace and failure lines.
    // (A batch of one function is credited with its failures even when the
    // run record dropped the start of a long output, trace line included.)
    let current = b - a === 1 ? names[a] : null;
    const seen = new Set(current ? [current] : []);
    if (current) results[current] = { status: 'done', failures: 0 };
    for (const line of text.split('\n')) {
      const f = /wwt function (\d+) (\w+)/.exec(line);
      if (f) {
        current = f[2];
        seen.add(current);
        results[current] = { status: 'done', failures: 0 };
      } else if (current && /Test failed:/.test(line)) results[current].failures++;
    }
    const summary = /visual: (\d+) tests executed .*?(\d+) failures?\b/.exec(text);
    // The function it was in hung or crashed; the rest of the batch did not run.
    if ((!exited || !summary) && current) results[current].status = exited ? 'crash' : 'timeout';
    for (let i = a; i < b; i++) if (!seen.has(names[i])) results[names[i]] = { status: 'not run' };
    const fails = names.slice(a, b).reduce((n, k) => n + (results[k]?.failures ?? 0), 0);
    const bad = names.slice(a, b).filter((k) => ['crash', 'timeout'].includes(results[k]?.status));
    console.log(`${String(a).padStart(3)}-${String(b).padEnd(3)} ${fails} failures${bad.length ? `, ${bad.map((n) => `${n} ${results[n].status}`).join(', ')}` : ''} (${((Date.now() - t0) / 1000).toFixed(0)} s)`);
    if (exited && summary && b - a > 1)
      for (let i = a; i < b; i++) if (results[names[i]].status === 'not run') ranges.push([i, i + 1]);
  }
} finally {
  await context.close();
  server.kill();
}

writeFileSync(join(out, 'results.json'), JSON.stringify(results, null, 2) + '\n');
const total = Object.values(results).reduce((n, r) => n + (r.failures ?? 0), 0);
const broken = Object.entries(results).filter(([, r]) => r.status !== 'done');
console.log(`d3d9 visual: ${Object.keys(results).length} functions, ${total} failures, ${broken.length} crashed, hung or not run`);
if (writeBaseline) writeFileSync(writeBaseline, JSON.stringify(results, null, 2) + '\n');
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
