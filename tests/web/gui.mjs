#!/usr/bin/env node
// Windowed Wine programs in headless Chromium (Milestone 4): runs Wine's
// Minesweeper and Notepad from the Wine bundle through the web front end,
// checks the canvas shows them, then clicks and types on the canvas and
// checks the programs respond. Screens are saved in target/gui.
//
//   node runtime/node/wine-bundle.mjs && node tests/web/gui.mjs

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

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const outDir = join(root, 'target/gui');
mkdirSync(outDir, { recursive: true });
const port = 19000 + Math.floor(Math.random() * 1000);
const server = spawn('node', [join(root, 'runtime/web/serve.mjs'), String(port), root], { stdio: 'ignore' });
await new Promise((r) => setTimeout(r, 500));

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

async function open(page, program) {
  await page.goto(`http://localhost:${port}/runtime/web/?exe=/target/wine-bundle/programs/${program}&wine=1`);
  await page.waitForFunction(() => window.screenShown || window.lastExit, null, { timeout: 240000 });
  const exit = await page.evaluate(() => window.lastExit);
  if (exit) throw new Error(`${program} exited: ${await page.textContent('#out')}`);
}

const save = async (page, name) => writeFileSync(join(outDir, name), await page.locator('#screen').screenshot());

const browser = await chromium.launch();
try {
  const page = await browser.newPage({ viewport: { width: 1000, height: 1100 } });
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
  console.log((await page.textContent('#log')).trim());
} catch (e) {
  console.error(e);
  results.push(false);
} finally {
  await browser.close();
  server.kill();
}
const failed = results.filter((ok) => !ok).length;
console.log(failed ? `${failed} browser GUI checks failed` : `all ${results.length} browser GUI checks passed (screens in ${outDir})`);
process.exit(failed ? 1 : 0);
