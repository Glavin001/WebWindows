#!/usr/bin/env node
// Runs a translated program in headless Chromium through the web front end
// three times. The first launch translates; when code had to be translated
// at run time, the second launch re-translates with that profile; the third
// must load the cached translation (the M3 criterion).
//
//   node tests/web/browser.mjs [path/to/program.exe] [expected stdout substring]

import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
let chromium;
try {
  ({ chromium } = require('playwright'));
} catch {
  ({ chromium } = require(join(process.execPath, '../../lib/node_modules/playwright')));
}

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const exe = resolve(process.argv[2] ?? join(root, 'tests/programs/hello.exe'));
const expect = process.argv[3] ?? 'Hello from translated x86!';
const wine = process.argv.includes('--wine');
const port = 18000 + Math.floor(Math.random() * 1000);
const server = spawn('node', [join(root, 'runtime/web/serve.mjs'), String(port), root], { stdio: 'ignore' });
await new Promise((r) => setTimeout(r, 500));

const browser = await chromium.launch();
let failed = false;
try {
  const page = await browser.newPage();
  page.on('pageerror', (e) => console.error('page error:', e.message));
  const url = `http://localhost:${port}/runtime/web/?exe=/${relative(root, exe)}${wine ? '&wine=1' : ''}`;
  for (const launch of ['first', 'second', 'third']) {
    await page.goto(url);
    await page.waitForFunction(() => window.lastExit !== undefined, null, { timeout: 120000 });
    const out = await page.textContent('#out');
    const log = await page.textContent('#log');
    const exit = await page.evaluate(() => window.lastExit);
    console.log(`--- ${launch} launch: exit ${exit.code}, translated=${exit.translated}, run ${exit.runMs?.toFixed(1)} ms`);
    console.log(log.trim());
    console.log(out.trim());
    if (!out.includes(expect)) failed = true;
    if (launch === 'third' && exit.translated && !wine) {
      console.error('FAIL: third launch translated again instead of using the cache');
      failed = true;
    }
  }
} finally {
  await browser.close();
  server.kill();
}
process.exit(failed ? 1 : 0);
