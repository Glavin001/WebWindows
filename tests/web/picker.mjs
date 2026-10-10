#!/usr/bin/env node
// The M3 criterion in headless Chromium: choose a folder, run a console
// program from it on translated Wine, and launch it again from the cache.
//
// The folder picker is a native dialog, so the test stands a directory in the
// origin private file system in for the user's choice: showDirectoryPicker
// returns its handle, which is the same FileSystemDirectoryHandle type the
// real picker returns. Everything after that is the page's own code path.
//
//   node tests/web/picker.mjs

import { execFileSync, spawn } from 'node:child_process';
import { mkdirSync, readFileSync } from 'node:fs';
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
// The folder the "user" picks: a program in a subdirectory with a data file
// next to it, which the program opens relative to its working directory,
// and a file seven levels down (games keep data that deep; the page once
// read three levels and dropped the rest).
const deep = 'data/a/b/c/d/e/deep.txt';
// And a program with two DLLs of its own at the same base (tests/web/dllpair):
// both translations are cached, the one the loader moves at its new base.
const built = join(root, 'target/web/dllpair');
mkdirSync(built, { recursive: true });
const gcc = (...args) => execFileSync('i686-w64-mingw32-gcc', ['-O2', ...args], { stdio: 'inherit' });
for (const d of ['a', 'b']) gcc('-shared', '-Wl,--image-base,0x10000000', '-o', join(built, `${d}.dll`), join(root, `tests/web/dllpair/${d}.c`));
gcc('-o', join(built, 'usedlls.exe'), join(root, 'tests/web/dllpair/main.c'), join(built, 'a.dll'), join(built, 'b.dll'));
const folder = {
  'game/usedlls.exe': [...readFileSync(join(built, 'usedlls.exe'))],
  'game/a.dll': [...readFileSync(join(built, 'a.dll'))],
  'game/b.dll': [...readFileSync(join(built, 'b.dll'))],
  'bin/readfile.exe': [...readFileSync(join(root, 'tests/programs/readfile.exe'))],
  'bin/data.txt': [...Buffer.from('hello from the chosen folder\n')],
  'readme.txt': [...Buffer.from('not a program\n')],
  [deep]: [...Buffer.from('deep\n')],
};
const expect = 'data: hello from the chosen folder';

const port = 18000 + Math.floor(Math.random() * 1000);
const server = spawn('node', [join(root, 'runtime/web/serve.mjs'), String(port), root], { stdio: 'ignore' });
await new Promise((r) => setTimeout(r, 500));

const browser = await chromium.launch();
let failed = false;
try {
  const page = await browser.newPage();
  page.on('pageerror', (e) => console.error('page error:', e.message));
  await page.addInitScript(() => {
    window.showDirectoryPicker = async () => (await navigator.storage.getDirectory()).getDirectoryHandle('picked');
  });
  await page.goto(`http://localhost:${port}/runtime/web/?wine=1`);
  await page.evaluate(async (files) => {
    const opfs = await navigator.storage.getDirectory();
    for await (const name of opfs.keys()) await opfs.removeEntry(name, { recursive: true });
    const top = await opfs.getDirectoryHandle('picked', { create: true });
    for (const [path, bytes] of Object.entries(files)) {
      let dir = top;
      const parts = path.split('/');
      for (const p of parts.slice(0, -1)) dir = await dir.getDirectoryHandle(p, { create: true });
      const w = await (await dir.getFileHandle(parts.at(-1), { create: true })).createWritable();
      await w.write(new Uint8Array(bytes));
      await w.close();
    }
  }, folder);

  await page.click('#pick');
  await page.waitForFunction(() => !document.getElementById('run').disabled);
  const programs = await page.$$eval('#exe option', (os) => os.map((o) => o.value));
  console.log(`programs in the folder: ${programs.join(', ')}`);
  if (programs.join() !== 'bin/readfile.exe,game/usedlls.exe') failed = true;
  const picked = await page.evaluate(() => window.webwindows.folder());
  console.log(`folder: ${picked.files} files, ${picked.depth} levels deep`);
  if (picked.files !== Object.keys(folder).length || !picked.paths.includes(deep) || picked.depth !== 7) {
    console.error(`FAIL: the page read ${picked.paths.join(', ')}`);
    failed = true;
  }

  await page.selectOption('#exe', 'bin/readfile.exe');
  for (const launch of ['first', 'second']) {
    await page.evaluate(() => (window.lastExit = undefined));
    await page.click('#run');
    await page.waitForFunction(() => window.lastExit !== undefined, null, { timeout: 120000 });
    const out = await page.textContent('#out');
    const log = await page.textContent('#log');
    const exit = await page.evaluate(() => window.lastExit);
    console.log(`--- ${launch} launch: exit ${exit.code}`);
    console.log(log.trim());
    console.log(out.trim());
    if (!out.includes(expect) || exit.code !== 0) failed = true;
    const translated = /translated c:\\app\\bin\\readfile\.exe/.test(log);
    if (launch === 'first' && !translated) failed = true;
    if (launch === 'second' && (translated || !log.includes('loaded cached translation'))) {
      console.error('FAIL: the second launch translated again instead of using the cache');
      failed = true;
    }
  }

  // The DLLs: translated on the first launch, from the cache on the second.
  await page.selectOption('#exe', 'game/usedlls.exe');
  for (const launch of ['first', 'second']) {
    await page.evaluate(() => (window.lastExit = undefined));
    await page.click('#run');
    await page.waitForFunction(() => window.lastExit !== undefined, null, { timeout: 120000 });
    const out = await page.textContent('#out');
    const log = await page.textContent('#log');
    const lines = log.split('\n').filter((l) => /c:\\app\\game\\[ab]\.dll|DLL cache/.test(l));
    console.log(`--- usedlls, ${launch} launch: ${out.trim()}`);
    console.log(lines.join('\n'));
    if (!out.includes('dlls: 42')) failed = true;
    const want = launch === 'first' ? /^translated c:\\app\\game\\/ : /^loaded cached translation of c:\\app\\game\\/;
    if (lines.filter((l) => want.test(l)).length !== 2) {
      console.error(`FAIL: the ${launch} launch should have ${launch === 'first' ? 'translated' : 'loaded from the cache'} both DLLs`);
      failed = true;
    }
    if (launch === 'second' && !lines.some((l) => / \(at 0x/.test(l))) {
      console.error('FAIL: the DLL the loader moved was not loaded from the cache at its new base');
      failed = true;
    }
  }
} finally {
  await browser.close();
  server.kill();
}
console.log(failed ? 'FAIL' : 'ok');
process.exit(failed ? 1 : 0);
