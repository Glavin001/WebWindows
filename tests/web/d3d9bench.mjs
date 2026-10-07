#!/usr/bin/env node
// Runs the Direct3D 9 benchmark (tests/programs/gui/d3d9bench.exe) on
// translated Wine in headless Chromium, through the web front end and
// wined3d's WebGPU backend, and prints what it reports: frames per second
// and where each frame's time went.
//
//   node runtime/node/wine-bundle.mjs && node tests/web/d3d9bench.mjs [cubes [particles [seconds [materials]]]]
//
// Defaults: 400 cubes, 2000 particles, 10 seconds, 1 material. --url tests a deployed
// site instead of serving this checkout; --headed shows the browser. The
// last frame is saved as target/gui/d3d9bench.png.

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
const argv = process.argv.slice(2);
const flag = (name) => {
  const i = argv.indexOf(name);
  if (i < 0) return null;
  const [, value] = argv.splice(i, 2);
  return value;
};
const headed = argv.includes('--headed') && argv.splice(argv.indexOf('--headed'), 1);
const urlArg = flag('--url');
const site = urlArg ? new URL(urlArg) : null;
const [cubes = '400', particles = '2000', seconds = '10', materials = '1'] = argv;

const port = 19000 + Math.floor(Math.random() * 1000);
const base = site ? site.origin + site.pathname.replace(/\/runtime\/web\/?$/, '').replace(/\/$/, '') : `http://localhost:${port}`;
const server = site ? null : spawn('node', [join(repo, 'runtime/web/serve.mjs'), String(port), repo], { stdio: 'ignore' });
if (server) await new Promise((r) => setTimeout(r, 500));

const proxy = site && process.env.HTTPS_PROXY ? { server: process.env.HTTPS_PROXY } : undefined;
const browser = await chromium.launch({ headless: !headed, proxy, args: ['--enable-unsafe-webgpu'] });
let failed = false;
try {
  const page = await browser.newPage({ viewport: { width: 1000, height: 1100 }, ignoreHTTPSErrors: !!proxy });
  if (site?.searchParams.has('_vercel_share')) await page.goto(site.href);
  page.on('pageerror', (e) => console.error('page error:', e.message));
  const args = [cubes, particles, seconds, materials].join('+');
  await page.goto(`${base}/runtime/web/?exe=/tests/programs/gui/d3d9bench.exe&wine=1&args=${args}`);

  const out = join(repo, 'target/gui');
  mkdirSync(out, { recursive: true });
  // Echo the program's lines as they arrive, until it exits; the screen is
  // saved with each frame rate line, while the window is still up.
  let shown = 0;
  const deadline = Date.now() + 300000 + Number(seconds) * 1000;
  for (;;) {
    const text = (await page.textContent('#out')) ?? '';
    const lines = text.split('\n');
    let rate = false;
    for (; shown < lines.length - 1; shown++) {
      if (/^(err|fixme|\d+:err):/.test(lines[shown])) continue;
      console.log(lines[shown]);
      rate ||= / fps: /.test(lines[shown]);
    }
    if (rate) writeFileSync(join(out, 'd3d9bench.png'), await page.locator('#screen').screenshot());
    if (await page.evaluate(() => window.lastExit)) {
      for (; shown < lines.length; shown++) if (lines[shown]) console.log(lines[shown]);
      break;
    }
    if (Date.now() > deadline) throw new Error('timed out');
    await new Promise((r) => setTimeout(r, 500));
  }
  const status = await page.textContent('#status');
  console.log(status);
  failed = !/exited with code 0/.test(status);
} finally {
  await browser.close();
  server?.kill();
}
process.exit(failed ? 1 : 0);
