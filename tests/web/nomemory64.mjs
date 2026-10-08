#!/usr/bin/env node
// The page in a browser without 64-bit WebAssembly memory, as in WebKit
// (Safari, and every browser on iOS): headless Chromium where a module
// declaring a 64-bit memory does not validate, which is how the page tests
// for it. A 64-bit console program must still run (below 4 GB) and the page
// say why. On Wine, a 64-bit program runs from the 32-bit-memory bundle
// (wine-bundle.mjs --arch x64 --mem32) when it is built, and otherwise stops
// with the reason.
//
//   node tests/web/nomemory64.mjs

import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import { existsSync } from 'node:fs';
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
const port = 18000 + Math.floor(Math.random() * 1000);
const server = spawn('node', [join(root, 'runtime/web/serve.mjs'), String(port), root], { stdio: 'ignore' });
await new Promise((r) => setTimeout(r, 500));

let failed = false;
const check = (name, ok, detail = '') => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}${detail ? `: ${detail}` : ''}`);
  if (!ok) failed = true;
};

const browser = await chromium.launch();
try {
  const page = await browser.newPage();
  page.on('pageerror', (e) => console.error('page error:', e.message));
  // Workers take the page's answer, so the page alone needs the stand-in.
  await page.addInitScript(() => {
    const validate = WebAssembly.validate;
    WebAssembly.validate = (bytes) => {
      const b = new Uint8Array(bytes);
      // A memory section whose limits have the 64-bit flag (0x04).
      return b[8] === 5 && b[11] & 4 ? false : validate(bytes);
    };
  });
  const run = async (query) => {
    await page.goto(`http://localhost:${port}/runtime/web/?${query}`);
    await page.waitForFunction(() => window.lastExit !== undefined, null, { timeout: 120000 });
    return { exit: await page.evaluate(() => window.lastExit), out: await page.textContent('#out'), log: await page.textContent('#log') };
  };

  const console64 = await run('exe=/tests/programs/hello64.exe');
  check('64-bit console program runs below 4 GB', console64.exit.code === 0 && console64.out.includes('Hello from translated x86!'), console64.out.trim());
  check('the log says why', console64.log.includes('no 64-bit WebAssembly memory'));
  check('the page explains', await page.evaluate(() => !document.getElementById('mem64note').hidden));

  const wine64 = await run('exe=/tests/programs/hello64.exe&wine=1');
  if (existsSync(join(root, 'target/wine-bundle64-m32/manifest.json'))) {
    check('64-bit program on Wine runs on a 32-bit memory', wine64.exit.code === 0 && wine64.out.includes('Hello from translated x86!'), wine64.out.trim().slice(-400));
  } else {
    check(
      '64-bit program on Wine stops with the reason',
      wine64.exit.code === null && wine64.out.includes('need a browser with 64-bit WebAssembly memory') && !wine64.out.includes(' at '),
      wine64.out.trim(),
    );
  }
} finally {
  await browser.close();
  server.kill();
}
process.exit(failed ? 1 : 0);
