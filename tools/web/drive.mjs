#!/usr/bin/env node
// Runs a program from a folder on the page (runtime/web) in headless
// Chromium and drives it with commands, for looking at games and GUI
// programs from a terminal:
//
//   node tools/web/drive.mjs DIR EXE [options]
//
//   DIR             the program's folder (as the page's folder picker gets it)
//   EXE             the program, relative to DIR
//   --args "A B"    its command line
//   --out PREFIX    screenshots PREFIX-NN-LABEL.png, records PREFIX-*.json
//                   (default target/drive/run); commands are read from
//                   PREFIX.cmd as lines are appended to it
//   --present MODE  ?d3dpresent= (default gdi: Direct3D frames read back into
//                   the window, so screenshots show them headless)
//   --debug CH      Wine's PE-side debug channels (WINEDEBUG), --unixtrace CH
//                   the Unix side's (win32u, wineserver, the display driver)
//   --profile DIR   Chromium profile kept between runs (default
//                   target/drive/profile): the page's translation cache lives
//                   there, so later runs start in seconds, not minutes;
//                   --fresh uses a new one
//   --port N        the local server's port (default 19601; the cache belongs
//                   to the page's origin, so keep it fixed)
//   --video DIR     records the page as a video (WebM) into DIR; commands
//                   are printed with their time since the start, to cut by
//
// The page's status samples (runtime/wine/status.mjs, every 2 s: threads
// and where they are, system calls, screen, input) are printed as lines.
//
// Commands, one per line:
//   waitfor EXPR [MS]   until EXPR, JavaScript over the latest status sample
//                       `s`, holds (e.g. "s.screen.nonBlack > 0.2",
//                       "s.syscalls.perSec > 1000"); default timeout 300 s
//   wait MS | shot LABEL | status (the whole latest sample) | record FILE
//   key NAME [MS] | hold NAME | release NAME   (KeyboardEvent.code names)
//   move DX DY [STEPS] | click | rclick | down | up | focus (click the screen)
//   js EXPR             evaluate in the page and print the result
//   quit

import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { formatStatus } from '../../runtime/wine/status.mjs';

const require = createRequire(import.meta.url);
let chromium;
try {
  ({ chromium } = require('playwright'));
} catch {
  ({ chromium } = require(join(process.execPath, '../../lib/node_modules/playwright')));
}

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const argv = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = argv.indexOf(`--${name}`);
  if (i < 0) return fallback;
  const [, v] = argv.splice(i, 2);
  return v;
};
const flag = (name) => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 && argv.splice(i, 1).length > 0;
};
const args = opt('args', '');
const out = resolve(opt('out', join(root, 'target/drive/run')));
const present = opt('present', 'gdi');
const debug = opt('debug', '');
const unixtrace = opt('unixtrace', '');
const port = Number(opt('port', 19601));
const fresh = flag('fresh');
const profile = resolve(opt('profile', join(root, 'target/drive/profile')));
const video = opt('video', '');
const [dir, exe] = argv;
if (!dir || !exe) {
  console.error('usage: drive.mjs DIR EXE [--args A] [--out PREFIX] [--present gdi|offscreen] [--debug CH] [--unixtrace CH] [--profile DIR | --fresh] [--port N]');
  process.exit(2);
}
mkdirSync(dirname(out), { recursive: true });
const cmdFile = `${out}.cmd`;
if (!existsSync(cmdFile)) writeFileSync(cmdFile, '');

const server = spawn(process.execPath, [join(root, 'runtime/web/serve.mjs'), String(port), root], { stdio: 'ignore' });
await new Promise((r) => setTimeout(r, 500));
const t0 = Date.now();
const secs = () => ((Date.now() - t0) / 1000).toFixed(0);
const launch = { args: ['--enable-unsafe-webgpu'], viewport: { width: 1000, height: 1000 } };
if (video) launch.recordVideo = { dir: resolve(video), size: launch.viewport };
const context = fresh ? await (await chromium.launch(launch)).newContext(launch) : await chromium.launchPersistentContext(profile, launch);
let status = null;
try {
  const page = context.pages()[0] ?? (await context.newPage());
  page.on('pageerror', (e) => console.log(`page error: ${e.message}`));
  const seen = new Map();
  page.on('console', (m) => {
    const text = m.text();
    if (text.startsWith('webwindows:status ')) {
      status = JSON.parse(text.slice(18));
      console.log(formatStatus(status));
      return;
    }
    if (!['error', 'warning'].includes(m.type())) return;
    const n = (seen.get(text.slice(0, 160)) ?? 0) + 1;
    seen.set(text.slice(0, 160), n);
    if (n <= 3) console.log(`console ${m.type()}: ${text.slice(0, 400)}`);
  });
  const q = new URLSearchParams({ wine: '1', d3dpresent: present, debug, unixtrace });
  await page.goto(`http://localhost:${port}/runtime/web/?${q}`);
  await page.setInputFiles('#fallback', resolve(dir));
  await page.waitForFunction(() => !document.getElementById('run').disabled);
  await page.selectOption('#exe', exe);
  await page.fill('#args', args);
  await page.check('#wine');
  await page.click('#run');
  await page.waitForFunction(() => window.screenShown || window.lastExit, null, { timeout: 600000 }).catch(() => {});
  if (!(await page.evaluate(() => window.screenShown))) {
    console.log(`[${secs()} s] no screen: ${await page.textContent('#status')}`);
    const rec = await page.evaluate(() => window.webwindows?.lastRun());
    console.log((rec?.out ?? '').split('\n').slice(-15).join('\n'));
    throw new Error('no screen');
  }
  const box = await page.locator('#screen').boundingBox();
  console.log(`[${secs()} s] screen up at ${Math.round(box.x)},${Math.round(box.y)} ${Math.round(box.width)}x${Math.round(box.height)}`);

  let shot = 0;
  let mx = box.x + box.width / 2;
  let my = box.y + box.height / 2;
  await page.mouse.move(mx, my);
  let done = 0;
  for (;;) {
    const lines = readFileSync(cmdFile, 'utf8').split('\n').filter((l) => l.trim());
    if (lines.length <= done) {
      await page.waitForTimeout(300);
      continue;
    }
    const line = lines[done++].trim();
    const [cmd, a, b, c] = line.split(/\s+/);
    console.log(`[${secs()} s] > ${line}`);
    if (cmd === 'quit') break;
    else if (cmd === 'wait') await page.waitForTimeout(Number(a));
    else if (cmd === 'waitfor') {
      const m = /^waitfor\s+(.*?)(?:\s+(\d+))?$/.exec(line);
      const test = new Function('s', `return (${m[1]});`);
      const until = Date.now() + Number(m[2] ?? 300000);
      let ok = false;
      while (Date.now() < until) {
        try {
          if (status && test(status)) {
            ok = true;
            break;
          }
        } catch {}
        await page.waitForTimeout(500);
      }
      console.log(`[${secs()} s] waitfor ${ok ? 'met' : 'timed out'}: ${m[1]}`);
    } else if (cmd === 'shot') {
      const f = `${out}-${String(++shot).padStart(2, '0')}-${a ?? 'shot'}.png`;
      await page.screenshot({ path: f, clip: box });
      console.log(`[${secs()} s] ${f}`);
    } else if (cmd === 'status') console.log(JSON.stringify(status));
    else if (cmd === 'record') {
      writeFileSync(a, JSON.stringify(await page.evaluate(() => window.webwindows.lastRun()), null, 1));
      console.log(`record saved to ${a}`);
    } else if (cmd === 'key') {
      await page.keyboard.down(a);
      await page.waitForTimeout(Number(b ?? 100));
      await page.keyboard.up(a);
    } else if (cmd === 'hold') await page.keyboard.down(a);
    else if (cmd === 'release') await page.keyboard.up(a);
    else if (cmd === 'move') {
      const n = Number(c ?? 10);
      for (let k = 0; k < n; k++) {
        mx += Number(a) / n;
        my += Number(b) / n;
        await page.mouse.move(mx, my);
        await page.waitForTimeout(30);
      }
    } else if (cmd === 'click' || cmd === 'rclick') {
      const button = cmd === 'click' ? 'left' : 'right';
      await page.mouse.down({ button });
      await page.waitForTimeout(150);
      await page.mouse.up({ button });
    } else if (cmd === 'down') await page.mouse.down();
    else if (cmd === 'up') await page.mouse.up();
    else if (cmd === 'focus') await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
    else if (cmd === 'js') console.log('= ' + JSON.stringify(await page.evaluate(line.slice(3))));
    else console.log(`unknown command: ${cmd}`);
  }
} finally {
  await context.close();
  server.kill();
}
