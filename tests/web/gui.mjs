#!/usr/bin/env node
// Windowed Wine programs in headless Chromium (Milestone 4): runs Wine's
// Minesweeper and Notepad from the Wine bundle through the web front end,
// checks the canvas shows them, then clicks and types on the canvas and
// checks the programs respond (after checking the page lists every sample
// program, runtime/web/samples.json). Then (Milestone 5) a DirectDraw and
// DirectSound program built here with MinGW (tests/web/ddsound.c): its
// animation on the canvas, its tone in the page's audio ring. Screens are
// saved in target/gui.
//
//   node runtime/node/wine-bundle.mjs && node tests/web/gui.mjs [--root DIR]
//   node runtime/node/wine-bundle.mjs --arch x64 && node tests/web/gui.mjs --arch x64
//
// --arch x64 runs the 64-bit builds of the programs, from the x86_64 bundle.
// --no-memory64 as well makes the browser look like one without 64-bit
// WebAssembly memory (WebKit: modules declaring one do not validate), so the
// page runs them from the 32-bit-memory bundle
// (wine-bundle.mjs --arch x64 --mem32).
//
// --root serves another directory with the repository's layout, such as the
// static site tools/site/build.sh assembles; --url tests a deployed site
// instead (a Vercel share link is visited first, for its cookie). Besides
// the canvas, whole-page screenshots are saved (page-*.png).

import { execFileSync, spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
let chromium;
try {
  ({ chromium } = require('playwright'));
} catch {
  ({ chromium } = require(join(process.execPath, '../../lib/node_modules/playwright')));
}

const repo = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const rootArg = process.argv.indexOf('--root');
const root = rootArg > 0 ? resolve(process.argv[rootArg + 1]) : repo;
const outDir = join(repo, 'target/gui');
mkdirSync(outDir, { recursive: true });
const x64 = process.argv.includes('--arch') && process.argv[process.argv.indexOf('--arch') + 1] === 'x64';
const noMemory64 = process.argv.includes('--no-memory64');
const bundleDir = x64 ? (noMemory64 ? 'wine-bundle64-m32' : 'wine-bundle64') : 'wine-bundle';
const tag = `${x64 ? '64' : ''}${noMemory64 ? '-m32' : ''}`;
const urlArg = process.argv.indexOf('--url');
const site = urlArg > 0 ? new URL(process.argv[urlArg + 1]) : null;
const port = 19000 + Math.floor(Math.random() * 1000);
const base = site ? site.origin : `http://localhost:${port}`;
const server = site ? null : spawn('node', [join(repo, 'runtime/web/serve.mjs'), String(port), root], { stdio: 'ignore' });
if (server) await new Promise((r) => setTimeout(r, 500));

const results = [];
function check(name, ok, detail) {
  results.push(ok);
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}${detail ? `: ${detail}` : ''}`);
}

/** Counts canvas pixels in a rectangle that satisfy pred([r, g, b]). */
async function count(page, [x0, y0, x1, y1], pred) {
  const px = await page.evaluate(
    ([x0, y0, w, h]) => Array.from(document.getElementById('screen').getContext('2d').getImageData(x0, y0, w, h).data),
    [x0, y0, x1 - x0, y1 - y0],
  );
  let n = 0;
  for (let i = 0; i < px.length; i += 4) if (pred([px[i], px[i + 1], px[i + 2]])) n++;
  return n;
}

/** Waits until `fn` (polled) returns true. */
async function until(fn, ms) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    if (await fn()) return true;
    await new Promise((r) => setTimeout(r, 250));
  }
  return false;
}

/** Clicks the canvas at screen pixel (x, y). */
async function click(page, x, y) {
  const box = await page.locator('#screen').boundingBox();
  await page.mouse.click(box.x + (x * box.width) / 800, box.y + (y * box.height) / 600);
}

async function open(page, program, path = `/target/${bundleDir}/programs/${program}`, query = '') {
  await page.goto(`${base}/runtime/web/?exe=${path}&wine=1${query}`);
  await page.waitForFunction(() => window.screenShown || window.lastExit, null, { timeout: 240000 });
  const exit = await page.evaluate(() => window.lastExit);
  if (exit) throw new Error(`${program} exited: ${await page.textContent('#out')}`);
}

const save = async (page, file) => {
  const name = file.replace('.png', `${tag}.png`);
  writeFileSync(join(outDir, name), await page.locator('#screen').screenshot());
  await page.screenshot({ path: join(outDir, `page-${name}`), fullPage: true });
};

// Through the environment's HTTPS proxy, when there is one.
const proxy = site && process.env.HTTPS_PROXY ? { server: process.env.HTTPS_PROXY } : undefined;
// WebGPU for Direct3D (wined3d's WebGPU backend); headless Chromium needs the flag.
const browser = await chromium.launch({ proxy, args: ['--enable-unsafe-webgpu'] });
try {
  const page = await browser.newPage({ viewport: { width: 1000, height: 1100 }, ignoreHTTPSErrors: !!proxy });
  if (site?.searchParams.has('_vercel_share')) await page.goto(site.href);
  page.on('pageerror', (e) => console.error('page error:', e.message));
  if (noMemory64) {
    await page.addInitScript(() => {
      const validate = WebAssembly.validate;
      // A memory section whose limits have the 64-bit flag (0x04).
      WebAssembly.validate = (bytes) => {
        const b = new Uint8Array(bytes);
        return b[8] === 5 && b[11] & 4 ? false : validate(bytes);
      };
    });
  }

  // The sample list: Wine's programs and every test program in
  // runtime/web/samples.json, each one served (the site carries them all).
  await page.goto(`${base}/runtime/web/`);
  await page.waitForSelector('#samples:not([hidden])', { timeout: 60000 });
  const listed = await page.$$eval('#sample option', (os) => os.map((o) => o.value));
  const samples = JSON.parse(readFileSync(join(repo, 'runtime/web/samples.json'), 'utf8'));
  const missing = [];
  for (const s of samples) {
    const url = listed.find((v) => v.endsWith(`/${s.path}`));
    if (!url || !(await page.evaluate((u) => fetch(u, { method: 'HEAD' }).then((r) => r.ok), url))) missing.push(s.path);
  }
  check('samples: every program listed and served', !missing.length && listed.length > samples.length, missing.length ? `missing ${missing.join(', ')}` : `${listed.length} programs`);

  // Minesweeper: LEDs, smiley and the board; a click reveals a square.
  await open(page, 'winemine.exe');
  const green = ([r, g, b]) => g > 100 && r < 60 && b < 60;
  const led = ([r, g, b]) => g > 200 && r < 60 && b < 60;
  const board = [8, 72, 152, 218];
  const drawn = await until(async () => (await count(page, board, green)) > 5000, 120000);
  check('winemine: board drawn in the canvas', drawn, `${await count(page, board, green)} green pixels`);
  check('winemine: LED digits', (await count(page, [8, 46, 44, 68], led)) > 40);
  await save(page, 'browser-winemine.png');
  const before = await count(page, board, green);
  await click(page, 60, 120);
  const revealed = await until(async () => (await count(page, board, green)) < before - 100, 30000);
  check('winemine: a click reveals squares', revealed, `${before} -> ${await count(page, board, green)} green pixels`);
  await save(page, 'browser-winemine-click.png');

  // Notepad: the window, then typed text in the edit area.
  await open(page, 'notepad.exe');
  const dark = ([r, g, b]) => r < 96 && g < 96 && b < 96;
  const edit = [8, 50, 300, 90];
  const shown = await until(async () => (await count(page, [0, 0, 400, 50], dark)) > 50, 120000);
  check('notepad: window drawn', shown);
  await save(page, 'browser-notepad.png');
  const ink = await count(page, edit, dark);
  // Keys go to Wine as physical keys (KeyboardEvent.code), so capitals
  // need Shift held, as on a real keyboard.
  await page.keyboard.press('Shift+KeyH');
  await page.keyboard.type('ello from a browser');
  const typed = await until(async () => (await count(page, edit, dark)) > ink + 150, 30000);
  check('notepad: typed text appears', typed, `${ink} -> ${await count(page, edit, dark)} dark pixels`);
  await save(page, 'browser-notepad-typed.png');

  // Direct3D 9: wined3d's WebGPU backend renders a clear, a triangle with
  // vertex and pixel shaders (and their constants), and a fixed-function
  // quad. These checks read the screen, so Present reads frames back and
  // draws them into the window (d3dpresent=gdi): headless Chromium cannot
  // show the WebGPU canvas the page otherwise presents to. These and the
  // DirectDraw program below are 32-bit programs, which the 32-bit run
  // checks (x86-64 Wine has no Direct3D or sound bridge yet).
  if (x64) {
    console.log('skipping d3d9 and ddsound: 32-bit programs (the run without --arch x64 checks them)');
  } else if (await page.evaluate(() => !!navigator.gpu)) {
    await open(page, 'd3d9tri.exe', '/tests/programs/gui/d3d9tri.exe', '&d3dpresent=gdi');
    const near = (c) => ([r, g, b]) => Math.abs(r - c[0]) < 24 && Math.abs(g - c[1]) < 24 && Math.abs(b - c[2]) < 24;
    // Window at (40,30); its client area starts at (44,53).
    const quad = [56, 65, 102, 111];
    const drawn = await until(async () => (await count(page, quad, near([255, 255, 0]))) > 1500, 180000);
    check('d3d9: fixed-function quad (yellow)', drawn, `${await count(page, quad, near([255, 255, 0]))} yellow pixels`);
    check('d3d9: clear colour', (await count(page, [300, 60, 360, 80], near([0, 0, 128]))) > 1000);
    // The triangle's lower left corner is red, its top green at half
    // intensity (pixel shader constant), and it is shifted right by a
    // quarter of the width (vertex shader constant).
    check('d3d9: shader triangle', (await count(page, [130, 250, 170, 262], ([r, g, b]) => r > 150 && b < 120)) > 100 &&
      (await count(page, [240, 90, 252, 100], ([r, g, b]) => g > 60 && g < 160 && r < 60)) > 10);
    check('d3d9: every call succeeded', /Present: 0/.test(await page.textContent('#out')), (await page.textContent('#out')).match(/[A-Za-z ()]+: 0x?[0-9a-f]+/g)?.slice(-3).join(', '));
    await save(page, 'browser-d3d9tri.png');

    // The benchmark (tests/web/d3d9bench.mjs runs it at full size): textured
    // cubes from static buffers, particles through a dynamic vertex buffer,
    // frames as fast as they come, for 3 seconds.
    await open(page, 'd3d9bench.exe', '/tests/programs/gui/d3d9bench.exe', '&args=60+500+3&d3dpresent=gdi');
    const out = async () => (await page.textContent('#out')) ?? '';
    await until(async () => / fps: /.test(await out()), 180000);
    // Window at (10,10), client area 640x480 from (14,33); cubes and
    // particles over a dark blue background.
    const lit = await count(page, [14, 33, 654, 513], ([r, g, b]) => r + g + b > 300);
    check('d3d9bench: frames drawn', lit > 5000, `${lit} lit pixels`);
    await until(() => page.evaluate(() => !!window.lastExit), 60000);
    const summary = (await out()).match(/summary: .*/)?.[0];
    check('d3d9bench: ran and exited', !!summary && (await page.evaluate(() => window.lastExit?.code)) === 0, summary);

    // OpenGL through opengl32 (native/opengl32-webgl: gl4es on WebGL 2): gltri's
    // frame. Window at (40,30) as d3d9tri's, client area from (44,53): a
    // yellow square at (10,10)-(60,60), a green square in front of a red
    // one at (230,20)-(310,100) (depth test), a 2x2 red and white texture
    // at (230,140)-(310,220) with half-transparent white over its right
    // half, over a dark blue clear.
    await open(page, 'gltri.exe', '/tests/programs/gui/gltri.exe', '&d3dpresent=gdi');
    const glDrawn = await until(async () => (await count(page, quad, near([255, 255, 0]))) > 1500, 180000);
    check('opengl: 2D overlay (yellow)', glDrawn, `${await count(page, quad, near([255, 255, 0]))} yellow pixels`);
    check('opengl: clear colour', (await count(page, [114, 58, 264, 78], near([0, 0, 128]))) > 1500);
    check('opengl: depth test', (await count(page, [284, 83, 344, 143], near([0, 255, 0]))) > 2000);
    check('opengl: texture and blending', (await count(page, [279, 198, 309, 228], near([255, 0, 0]))) > 500 &&
      (await count(page, [319, 238, 349, 268], near([255, 128, 128]))) > 500);
    check('opengl: every call succeeded', /SwapBuffers: 1/.test(await out()) && /glGetError: (0x)?0\b/.test(await out()), (await out()).split('\n').slice(-3).join(' | '));
    await save(page, 'browser-gltri.png');

    // glbench: cubes, one draw each, and a batch of particles, for 3 seconds.
    await open(page, 'glbench.exe', '/tests/programs/gui/glbench.exe', '&args=60+500+3&d3dpresent=gdi');
    await until(async () => / fps: /.test(await out()), 180000);
    const glLit = await count(page, [14, 33, 654, 513], ([r, g, b]) => r + g + b > 300);
    check('glbench: frames drawn', glLit > 5000, `${glLit} lit pixels`);
    await until(() => page.evaluate(() => !!window.lastExit), 60000);
    const glSummary = (await out()).match(/summary: .*/)?.[0];
    check('glbench: ran and exited', !!glSummary && (await page.evaluate(() => window.lastExit?.code)) === 0, glSummary);

    // gl2test: OpenGL 2.0 checked pixel by pixel (GLSL, buffers, client
    // arrays, render to texture, fixed-function state); it exits with the
    // number of checks that failed.
    await open(page, 'gl2test.exe', '/tests/programs/gui/gl2test.exe', '&d3dpresent=gdi');
    await until(() => page.evaluate(() => !!window.lastExit), 120000);
    const gl2 = (await out()).match(/gl2test: .*/)?.[0];
    check('gl2test: every check passed', (await page.evaluate(() => window.lastExit?.code)) === 0,
      gl2 ?? (await out()).split('\n').filter((l) => /^not ok/.test(l)).join(' | '));

    // Presenting on the GPU (no readback), to an offscreen buffer the page
    // can read (headless Chromium cannot show the canvas it uses
    // otherwise); and the window moves when its caption is dragged, though
    // the program polls for messages instead of waiting for them.
    await open(page, 'd3d9bench.exe', '/tests/programs/gui/d3d9bench.exe', '&args=60+500+60&d3dpresent=offscreen');
    await until(() => page.evaluate(() => !!window.d3dWindow), 180000);
    const frame = await page.evaluate(async () => {
      const { width, height, pixels } = await window.d3dSnapshot();
      let lit = 0;
      for (let i = 0; i < (pixels?.length ?? 0); i += 4) if (pixels[i] + pixels[i + 1] + pixels[i + 2] > 300) lit++;
      return { width, height, lit };
    });
    check('d3d9bench: GPU present', frame.width === 640 && frame.height === 480 && frame.lit > 5000, JSON.stringify(frame));
    const before = await page.evaluate(() => window.d3dWindow);
    const box = await page.locator('#screen').boundingBox();
    const sx = (x) => box.x + (x * box.width) / 800;
    const sy = (y) => box.y + (y * box.height) / 600;
    await page.mouse.move(sx(200), sy(20));
    await page.mouse.down();
    for (let i = 1; i <= 10; i++) {
      await page.mouse.move(sx(200 + i * 10), sy(20 + i * 6));
      await new Promise((r) => setTimeout(r, 100));
    }
    await page.mouse.up();
    const moved = await until(() => page.evaluate((x) => window.d3dWindow.x !== x, before.x), 20000);
    const after = await page.evaluate(() => window.d3dWindow);
    check('d3d9bench: window drags while rendering', moved && after.x - before.x === 100 && after.y - before.y === 60,
      `${before.x},${before.y} -> ${after.x},${after.y}`);
  } else {
    console.log('skipping d3d9: no WebGPU in this browser');
  }
  console.log((await page.textContent('#log')).trim());

  // DirectDraw and DirectSound: a red square moving on blue, and a tone.
  if (!site && !x64) {
    mkdirSync(join(root, 'target/web'), { recursive: true });
    execFileSync('i686-w64-mingw32-gcc', ['-O2', '-mwindows', '-o', join(root, 'target/web/ddsound.exe'),
      join(repo, 'tests/web/ddsound.c'), '-lddraw', '-ldsound', '-ldxguid', '-lgdi32', '-luser32', '-lwinmm']);
    await open(page, 'ddsound.exe', '/target/web/ddsound.exe');
    const client = [24, 44, 344, 284];
    const blue = ([r, g, b]) => b > 200 && r < 60 && g < 60;
    const red = ([r, g, b]) => r > 200 && g < 60 && b < 60;
    const drew = await until(async () => (await count(page, client, blue)) > 30000, 120000);
    check('ddsound: DirectDraw draws in the window', drew, `${await count(page, client, blue)} blue pixels`);
    // The square's left edge, from the red pixels along its middle row.
    const squareX = async () => {
      const row = await page.evaluate(() => Array.from(document.getElementById('screen').getContext('2d').getImageData(24, 164, 320, 1).data));
      for (let i = 0; i < row.length; i += 4) if (red([row[i], row[i + 1], row[i + 2]])) return i / 4;
      return -1;
    };
    // From a frame with the square on that row: the first blue one can come
    // before it.
    let x0 = -1;
    await until(async () => (x0 = await squareX()) >= 0, 10000);
    const moved = await until(async () => {
      const x = await squareX();
      return x >= 0 && x0 >= 0 && x !== x0;
    }, 10000);
    check('ddsound: the square moves', moved, `x ${x0} -> ${await squareX()}`);
    await save(page, 'browser-ddsound.png');
    // The tone reaches the ring the AudioWorklet reads (whether or not the
    // headless browser lets the context play it).
    const played = await page.evaluate(() => (window.audioRing ? new Int32Array(window.audioRing, 0, 2)[0] : -1));
    check('ddsound: DirectSound output reaches the page', played > 1000, `${played} frames written`);
  }
} catch (e) {
  console.error(e);
  results.push(false);
} finally {
  await browser.close();
  server?.kill();
}
const failed = results.filter((ok) => !ok).length;
console.log(failed ? `${failed} browser GUI checks failed` : `all ${results.length} browser GUI checks passed (screens in ${outDir})`);
process.exit(failed ? 1 : 0);
