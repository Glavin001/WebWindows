#!/usr/bin/env node
// Windowed Wine programs in headless Chromium (Milestone 4): runs Wine's
// Minesweeper and Notepad from the Wine bundle through the web front end,
// checks the canvas shows them, then clicks and types on the canvas and
// checks the programs respond. Then (Milestone 5) a DirectDraw and
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

async function open(page, program, path = `/target/${bundleDir}/programs/${program}`) {
  await page.goto(`${base}/runtime/web/?exe=${path}&wine=1`);
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
const browser = await chromium.launch({ proxy });
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
  console.log((await page.textContent('#log')).trim());

  // DirectDraw and DirectSound: a red square moving on blue, and a tone.
  if (!site) {
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
    const x0 = await squareX();
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
