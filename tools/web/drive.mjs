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
//   --headed        runs the installed Google Chrome in a window, on the real
//                   GPU (default present mode: canvas), instead of headless
//                   Chromium on SwiftShader
//   --screen-timeout MS  how long to wait for the program's screen (default
//                   600000); without one the run ends with the runtime's log
//   --page-args Q   more of the page's settings, as a query ("d3dvsync=0")
//   --syms DIR      unstripped PE files (Wine's build tree) to name guest
//                   functions in profiles (tools/web/cdp-profile.mjs)
//
// The page's status samples (runtime/wine/status.mjs, every 2 s: threads
// and where they are, system calls, screen, input) are printed as lines.
//
// Commands, one per line:
//   waitfor EXPR [MS]   until EXPR, JavaScript over the latest status sample
//                       `s`, holds (e.g. "s.screen.nonBlack > 0.2",
//                       "s.syscalls.perSec > 1000"); default timeout 300 s
//   waitpage EXPR [MS]  until EXPR, JavaScript in the page, is true (e.g.
//                       "window.webwindows.d3dPerf()?.drawsPerFrame > 500");
//                       default timeout 300 s
//   wait MS | shot LABEL | status (the whole latest sample) | record FILE
//   frame LABEL         the next presented Direct3D frame, read from the GPU
//                       (window.d3dSnapshot(): canvas and offscreen present,
//                       where `shot` shows only the GDI screen)
//   key NAME [MS] | hold NAME | release NAME   (KeyboardEvent.code names)
//   move DX DY [STEPS] | click | rclick | down | up | focus (click the screen)
//   js EXPR             evaluate in the page and print the result
//   profile MS [NAME]   CPU profiles of the guest and render workers for MS:
//                       a summary (time by category, DLL and function), and
//                       PREFIX-NAME-guest.cpuprofile / -render.cpuprofile
//                       (open them in Chrome DevTools) with -images.json
//   fps MS              the frame rate over the next MS: average, worst
//                       frame, draws per frame, render worker busy; as a
//                       line "fps {json}" too
//   quit

import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { formatStatus } from '../../runtime/wine/status.mjs';
import { Cdp, report, summarize, symbolize } from './cdp-profile.mjs';

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
const headed = flag('headed');
const present = opt('present', headed ? 'canvas' : 'gdi');
const syms = opt('syms', '');
const pageArgs = opt('page-args', '');
const screenTimeout = Number(opt('screen-timeout', 600000));
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
// The DevTools Protocol on its own port, for `profile` (Playwright talks
// to the browser over a pipe).
const cdpPort = port + 1;
const launch = { args: ['--enable-unsafe-webgpu', `--remote-debugging-port=${cdpPort}`], viewport: { width: 1000, height: 1000 } };
if (headed) Object.assign(launch, { headless: false, channel: 'chrome', viewport: null, args: [...launch.args, '--window-size=1100,1050'] });
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
  for (const [k, v] of new URLSearchParams(pageArgs)) q.set(k, v);
  await page.goto(`http://localhost:${port}/runtime/web/?${q}`);
  await page.setInputFiles('#fallback', resolve(dir));
  await page.waitForFunction(() => !document.getElementById('run').disabled);
  await page.selectOption('#exe', exe);
  await page.fill('#args', args);
  await page.check('#wine');
  await page.click('#run');
  await page.waitForFunction(() => window.screenShown || window.lastExit, null, { timeout: screenTimeout }).catch(() => {});
  if (!(await page.evaluate(() => window.screenShown))) {
    console.log(`[${secs()} s] no screen: ${await page.textContent('#status')}`);
    const rec = await page.evaluate(() => window.webwindows?.lastRun());
    console.log('--- the program\'s output, last 15 lines:');
    console.log((rec?.out ?? '').split('\n').slice(-15).join('\n'));
    console.log('--- the runtime log, last 30 lines:');
    console.log((rec?.log ?? '').split('\n').slice(-30).join('\n'));
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
    } else if (cmd === 'waitpage') {
      const m = /^waitpage\s+(.*?)(?:\s+(\d+))?$/.exec(line);
      const ok = await page
        .waitForFunction(m[1], null, { timeout: Number(m[2] ?? 300000), polling: 500 })
        .then(() => true)
        .catch(() => false);
      console.log(`[${secs()} s] waitpage ${ok ? 'met' : 'timed out'}: ${m[1]}`);
    } else if (cmd === 'shot') {
      const f = `${out}-${String(++shot).padStart(2, '0')}-${a ?? 'shot'}.png`;
      await page.screenshot({ path: f, clip: box });
      console.log(`[${secs()} s] ${f}`);
    } else if (cmd === 'frame') {
      const f = `${out}-${String(++shot).padStart(2, '0')}-${a ?? 'frame'}.png`;
      const png = await page.evaluate(async () => {
        const fr = await Promise.race([window.d3dSnapshot?.(), new Promise((r) => setTimeout(() => r(null), 10000))]);
        if (!fr?.width || !fr.pixels?.length) return null;
        const c = document.createElement('canvas');
        c.width = fr.width;
        c.height = fr.height;
        c.getContext('2d').putImageData(new ImageData(new Uint8ClampedArray(fr.pixels), fr.width, fr.height), 0, 0);
        return c.toDataURL('image/png').split(',')[1];
      });
      if (png) writeFileSync(f, Buffer.from(png, 'base64'));
      console.log(`[${secs()} s] ${png ? f : 'frame: no Direct3D frame presented'}`);
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
    else if (cmd === 'profile') await cpuProfile(Number(a ?? 5000), b ?? `p${++shot}`);
    else if (cmd === 'fps') {
      const from = await page.evaluate(() => performance.now());
      await page.waitForTimeout(Number(a ?? 10000));
      const h = (await page.evaluate(() => window.webwindows.d3dPerfHistory())).filter((p) => p.at > from);
      if (h.length < 2) console.log('fps: no Direct3D frames reported');
      else {
        let frames = 0;
        for (let i = 1; i < h.length; i++) frames += (h[i].fps * (h[i].at - h[i - 1].at)) / 1000;
        const span = (h.at(-1).at - h[0].at) / 1000;
        const mean = (k) => h.reduce((s, p) => s + p[k], 0) / h.length;
        const r = {
          fps: +(frames / span).toFixed(1),
          minFps: +Math.min(...h.map((p) => p.fps)).toFixed(1),
          worstMs: +Math.max(...h.map((p) => p.worstMs)).toFixed(1),
          drawsPerFrame: Math.round(mean('drawsPerFrame')),
          renderBusy: +mean('busy').toFixed(3),
          seconds: +span.toFixed(1),
        };
        console.log(`[${secs()} s] ${r.fps} fps (lowest half-second ${r.minFps}, worst frame ${r.worstMs} ms), ${r.drawsPerFrame} draws/frame, render worker ${Math.round(r.renderBusy * 100)}% busy`);
        console.log(`fps ${JSON.stringify(r)}`);
      }
    }
    else console.log(`unknown command: ${cmd}`);
  }
} finally {
  await context.close();
  server.kill();
}

/** `profile MS NAME`: the guest and render workers' CPU profiles, saved and summarized. */
async function cpuProfile(ms, name) {
  const cdp = await Cdp.connect(cdpPort);
  try {
    const workers = await cdp.workers(`localhost:${port}/runtime/web/`);
    const pick = (re) => workers.find((w) => re.test(w.url));
    const targets = [
      ['guest', pick(/\/runtime\/web\/worker\.mjs/)],
      ['render', pick(/d3d-worker\.mjs/)],
    ].filter(([, w]) => w);
    if (!targets.length) {
      console.log(`profile: no workers (${workers.map((w) => w.url).join(', ') || 'none'})`);
      return;
    }
    const images = await context.pages()[0].evaluate(() => window.webwindows.images());
    console.log(`[${secs()} s] profiling ${targets.map(([n]) => n).join(' and ')} for ${ms} ms`);
    const profiles = await cdp.profile(targets.map(([, w]) => w.sessionId), ms);
    writeFileSync(`${out}-${name}-images.json`, JSON.stringify(images));
    targets.forEach(([n], i) => {
      const file = `${out}-${name}-${n}.cpuprofile`;
      writeFileSync(file, JSON.stringify(profiles[i]));
      const s = summarize(profiles[i], images);
      symbolize(s, syms);
      console.log(report(s, { title: `${n} worker (${file}): ` }));
    });
  } finally {
    cdp.close();
  }
}
