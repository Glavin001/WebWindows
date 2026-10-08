#!/usr/bin/env node
// Runs every d3dgpu scene in headless Chromium on the browser's WebGPU
// (Dawn and Tint; SwiftShader on CI), through the producer worker, shared
// memory and the render worker, and checks the expected pixels and
// readbacks.
//
//   runtime/d3dgpu/build.sh && node tests/web/d3dgpu.mjs

import { spawn } from 'node:child_process';
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
const port = 19000 + Math.floor(Math.random() * 1000);
const server = spawn('node', [join(root, 'runtime/web/serve.mjs'), String(port), root], { stdio: 'ignore' });
await new Promise((r) => setTimeout(r, 500));

const browser = await chromium.launch({ args: ['--enable-unsafe-webgpu'] });
let code = 1;
try {
  const page = await browser.newPage();
  page.on('pageerror', (e) => console.error('page error:', e.message));
  page.on('console', (m) => m.type() === 'error' && console.error('console:', m.text()));
  let failed = 0;
  for (const features of ['all', 'core']) {
    await page.goto(`http://localhost:${port}/runtime/d3dgpu/?test&features=${features}`);
    await page.waitForFunction(() => window.d3dgpuResults !== undefined, null, { timeout: 300000 });
    const r = await page.evaluate(() => window.d3dgpuResults);
    console.log(`adapter: ${r.adapter}`);
    console.log(`${r.total - r.failed.length}/${r.total} scenes and demos pass in the browser (${features} features)`);
    for (const f of r.failed) console.log(`FAIL ${f.name}: ${f.failures.join('; ')}`);
    failed += r.failed.length;
  }
  code = failed ? 1 : 0;
} catch (e) {
  console.error(e);
  console.error(await (await browser.contexts()[0]?.pages()[0])?.textContent('#log').catch(() => ''));
} finally {
  await browser.close();
  server.kill();
}
process.exit(code);
