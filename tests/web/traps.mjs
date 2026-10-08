#!/usr/bin/env node
// Memory traps in headless Chromium (docs/memory-traps.md): a program that
// reads through a null-region pointer runs on the page with bounds traps
// (?memtraps=1, and by default, since V8 reports where a trap happened)
// and with faithful checks (?memtraps=0). Each must stop with an access
// violation at the same instruction, not a bare WebAssembly trap.
//
//   node tests/web/traps.mjs
// PW_CHROMIUM=path runs that Chromium instead of Playwright's own.

import { execFileSync, spawn } from 'node:child_process';
import { mkdirSync } from 'node:fs';
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
mkdirSync(join(root, 'target/web'), { recursive: true });
execFileSync('i686-w64-mingw32-gcc', ['-O2', '-o', join(root, 'target/web/nullread.exe'), join(root, 'tests/web/nullread.c')]);
const port = 18000 + Math.floor(Math.random() * 1000);
const server = spawn('node', [join(root, 'runtime/web/serve.mjs'), String(port), root], { stdio: 'ignore' });
await new Promise((r) => setTimeout(r, 500));

const browser = await chromium.launch(process.env.PW_CHROMIUM ? { executablePath: process.env.PW_CHROMIUM } : {});
let failed = false;
const check = (name, ok, detail) => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}: ${detail}`);
  if (!ok) failed = true;
};
try {
  const page = await browser.newPage();
  page.on('pageerror', (e) => console.error('page error:', e.message));
  const faults = {};
  for (const [mode, query, expect] of [
    ['default', '', 'bounds traps'],
    ['traps', '&memtraps=1', 'bounds traps'],
    ['faithful', '&memtraps=0', 'faithful'],
  ]) {
    await page.goto(`http://localhost:${port}/runtime/web/?exe=/target/web/nullread.exe&nocache=1${query}`);
    await page.waitForFunction(() => window.lastExit !== undefined, null, { timeout: 120000 });
    const out = await page.textContent('#out');
    const log = await page.textContent('#log');
    check(`${mode}: memory checks`, log.includes(`memory checks: ${expect}`), expect);
    faults[mode] = /ACCESS_VIOLATION at (0x[0-9a-f]+)/.exec(out)?.[1];
    check(`${mode}: access violation`, !!faults[mode], out.trim().split('\n').slice(-1)[0]);
  }
  check('same instruction', faults.traps === faults.faithful && faults.default === faults.traps, JSON.stringify(faults));
} finally {
  await browser.close();
  server.kill();
}
process.exit(failed ? 1 : 0);
