#!/usr/bin/env node
// GUI programs on translated Wine with the browser display driver (M4),
// headless: each program runs in Node until it goes idle (or for a while),
// the screen is saved as a PNG, and pixels are checked.
//
//   node tests/wine/gui.mjs [--arch x64 [--mem32]] [--keep DIR]
//
// --arch x64 runs the 64-bit builds on x86_64 Wine (WINE_BUILD64) with the
// wasm64 Unix side; its table64 needs Node 24. --mem32 runs them on a 32-bit
// memory with the lowered Unix side (ARCH=x86_64 MEM32=1), as browsers
// without 64-bit WebAssembly memory do.

import { spawnSync } from 'node:child_process';
import { mkdirSync, readFileSync } from 'node:fs';
import { inflateSync } from 'node:zlib';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const args = process.argv.slice(2);
const x64 = args[0] === '--arch' && args[1] === 'x64';
if (args[0] === '--arch') args.splice(0, 2);
const mem32 = args[0] === '--mem32';
if (mem32) args.shift();
const wineBuild = x64 ? (process.env.WINE_BUILD64 ?? '/opt/wine-build64') : (process.env.WINE_BUILD ?? '/opt/wine-build');
const peDir = x64 ? 'x86_64-windows' : 'i386-windows';
const keep = args[0] === '--keep' ? resolve(args[1]) : join(root, 'target/gui');
mkdirSync(keep, { recursive: true });

/** Decodes the PNGs runtime/wine/display.mjs writes (RGBA, filter 0). */
function readPng(path) {
  const buf = readFileSync(path);
  let p = 8;
  let width = 0;
  let height = 0;
  const idat = [];
  while (p < buf.length) {
    const len = buf.readUInt32BE(p);
    const type = buf.toString('latin1', p + 4, p + 8);
    if (type === 'IHDR') {
      width = buf.readUInt32BE(p + 8);
      height = buf.readUInt32BE(p + 12);
    } else if (type === 'IDAT') idat.push(buf.subarray(p + 8, p + 8 + len));
    p += 12 + len;
  }
  const raw = inflateSync(Buffer.concat(idat));
  const px = (x, y) => {
    const o = y * (width * 4 + 1) + 1 + x * 4;
    return [raw[o], raw[o + 1], raw[o + 2]];
  };
  return { width, height, px };
}

const near = ([r, g, b], [R, G, B], tol = 8) => Math.abs(r - R) <= tol && Math.abs(g - G) <= tol && Math.abs(b - B) <= tol;
const count = (img, x0, y0, x1, y1, pred) => {
  let n = 0;
  for (let y = y0; y < y1; y++) for (let x = x0; x < x1; x++) if (pred(img.px(x, y))) n++;
  return n;
};

function run(name, exe, extra) {
  const png = join(keep, `${name}${x64 ? '64' : ''}${mem32 ? '-m32' : ''}.png`);
  const r = spawnSync(process.execPath, [join(root, 'runtime/node/wine.mjs'), ...(mem32 ? ['--mem32'] : []), '--screenshot', png, ...extra, exe], {
    encoding: 'latin1',
    timeout: 300000,
  });
  if (r.status !== 0) throw new Error(`${name}: exit ${r.status}\n${r.stderr.slice(-2000)}`);
  return { img: readPng(png), stdout: r.stdout };
}

const results = [];
function check(name, ok, detail) {
  results.push(ok);
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}${detail ? `: ${detail}` : ''}`);
}

// winbasic: a window with a caption, text, solid brushes and a 4-bit DIB.
{
  const { img, stdout } = run('winbasic', join(root, `tests/programs/gui/winbasic${x64 ? '64' : ''}.exe`), []);
  check('winbasic: painted once', /WM_PAINT 1/.test(stdout));
  check('winbasic: text metrics', /TextOut 1, extent 105x16/.test(stdout), stdout.match(/TextOut[^\n]*/)?.[0]);
  // Window at (40,30); client area from (44,53).
  check('winbasic: brush colours', near(img.px(60, 180), [255, 0, 0]) && near(img.px(90, 180), [0, 255, 0]) &&
    near(img.px(120, 180), [0, 0, 255]) && near(img.px(150, 180), [128, 128, 128]));
  check('winbasic: DIB colours', near(img.px(60, 210), [255, 0, 0]) && near(img.px(150, 210), [128, 128, 128]));
  const ink = count(img, 54, 63, 200, 80, (c) => c[0] < 96 && c[1] < 96 && c[2] < 96);
  check('winbasic: text drawn', ink > 60, `${ink} dark pixels`);
  const caption = count(img, 60, 36, 120, 48, (c) => c[0] > 200 && c[1] > 200 && c[2] > 200);
  check('winbasic: caption text', caption > 20, `${caption} light pixels`);
}

// winemine (Wine's Minesweeper): menu, LED counters, smiley, the board;
// then a click on a square reveals it (and starts the clock).
const green = (c) => c[1] > 100 && c[0] < 60 && c[2] < 60;
const board = [8, 72, 152, 218];
let unclicked = 0;
{
  const exe = join(wineBuild, `programs/winemine/${peDir}/winemine.exe`);
  const { img } = run('winemine', exe, ['--run-for', '3000']);
  unclicked = count(img, ...board, green);
  check('winemine: board', unclicked > 15000, `${unclicked} green pixels`);
  const leds = count(img, 8, 46, 44, 68, (c) => c[1] > 200 && c[0] < 60 && c[2] < 60);
  check('winemine: LED digits', leds > 40, `${leds} green pixels`);
  const face = count(img, 70, 48, 90, 66, (c) => c[0] > 200 && c[1] > 200 && c[2] < 60);
  check('winemine: smiley', face > 40, `${face} yellow pixels`);
  const menu = count(img, 8, 28, 70, 38, (c) => c[0] < 96 && c[1] < 96 && c[2] < 96);
  check('winemine: menu text', menu > 15, `${menu} dark pixels`);
}

{
  const exe = join(wineBuild, `programs/winemine/${peDir}/winemine.exe`);
  const { img } = run('winemine-click', exe, ['--run-for', '6000', '--input', '500:click 60,120']);
  const squares = count(img, ...board, green);
  // At least one square (16x16, mostly green) is no longer covered.
  check('winemine: a click reveals squares', squares < unclicked - 100, `${unclicked} -> ${squares} green pixels`);
}

// Notepad: its edit control comes from comctl32 v6 (a side-by-side
// assembly); typed text appears in it.
{
  const exe = join(wineBuild, `programs/notepad/${peDir}/notepad.exe`);
  const { img } = run('notepad', exe, ['--run-for', '12000', '--input', '500:text Hello Wine; 900:key Enter; 1000:text Typed in Notepad']);
  const dark = (c) => c[0] < 96 && c[1] < 96 && c[2] < 96;
  const title = count(img, 20, 6, 140, 22, (c) => c[0] > 200 && c[1] > 200 && c[2] > 200);
  check('notepad: caption', title > 60, `${title} light pixels`);
  const line1 = count(img, 6, 46, 120, 60, dark);
  const line2 = count(img, 6, 62, 160, 76, dark);
  check('notepad: typed text', line1 > 60 && line2 > 80, `${line1} and ${line2} dark pixels on lines 1 and 2`);
}

const failed = results.filter((ok) => !ok).length;
console.log(failed ? `${failed} GUI checks failed` : `all ${results.length} GUI checks passed (screens in ${keep})`);
process.exit(failed ? 1 : 0);
