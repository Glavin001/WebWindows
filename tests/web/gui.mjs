#!/usr/bin/env node
// Windowed Wine programs in headless Chromium (Milestone 4): runs Wine's
// Minesweeper and Notepad from the Wine bundle through the web front end,
// checks the canvas shows them, then clicks and types on the canvas and
// checks the programs respond. Screens are saved in target/gui.
//
//   node runtime/node/wine-bundle.mjs && node tests/web/gui.mjs [--root DIR]
//
// --root serves another directory with the repository's layout, such as the
// static site tools/site/build.sh assembles; --url tests a deployed site
// instead (a Vercel share link is visited first, for its cookie). Besides
// the canvas, whole-page screenshots are saved (page-*.png).

import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import { mkdirSync, writeFileSync } from 'node:fs';
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

async function open(page, program, path = `/target/wine-bundle/programs/${program}`, query = '') {
  await page.goto(`${base}/runtime/web/?exe=${path}&wine=1${query}`);
  await page.waitForFunction(() => window.screenShown || window.lastExit, null, { timeout: 240000 });
  const exit = await page.evaluate(() => window.lastExit);
  if (exit) throw new Error(`${program} exited: ${await page.textContent('#out')}`);
}

const save = async (page, name) => {
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
  // show the WebGPU canvas the page otherwise presents to.
  if (await page.evaluate(() => !!navigator.gpu)) {
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
  } else {
    console.log('skipping d3d9: no WebGPU in this browser');
  }
  console.log((await page.textContent('#log')).trim());
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
