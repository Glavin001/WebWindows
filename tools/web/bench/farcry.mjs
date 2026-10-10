#!/usr/bin/env node
// Far Cry demo benchmark on the page (tools/web/drive.mjs): launch to the
// menu, the menu's frame rate, "Launch Demo" and "Start" with the mouse, the
// Fort level's frame rate where the game starts (in the boat), and CPU
// profiles of the guest and render workers there. Prints one summary and
// writes it as JSON, so changes can be compared run against run.
//
//   node tools/web/bench/farcry.mjs [DIR] [--headed] [--out PREFIX] [--fresh]
//                                   [--level-secs N] [--profile-ms N] [--syms DIR]
//
//   DIR           the demo's folder (default target/games/farcry-x/far-cry-demo)
//   --headed      Google Chrome in a window on the real GPU (drive.mjs
//                 --headed); headless Chromium otherwise (SwiftShader)
//   --vsync       present at the display's rate, as the game asks (default:
//                 without waiting for it, to measure how fast the game runs)
//   --fresh       a new browser profile: no cached translations (a cold start)
//   --out PREFIX  files: PREFIX.json (the summary), PREFIX.log, screenshots and
//                 profiles (default target/bench/farcry); the browser profile
//                 (and so the translation cache) is kept next to them
//   --port N      the local server's port (default 19621; the cache belongs to
//                 the origin, so keep it fixed)
//   --syms DIR    unstripped PE files to name guest functions (drive.mjs --syms)
//   --page-args Q the page's settings for an experiment ("bundle=URL&translator=URL")
//   --chrome-args A  Chrome flags for an experiment ("--js-flags=--no-liftoff")
//   --fixed-step S   the game advances S seconds a frame (0.0166): frame N is
//                 the same scene on every run, so runs compare closely
//
// The game's menu takes the mouse as relative movement from where its own
// cursor starts (the screen's centre), so the clicks are moves, not
// coordinates; they are the same on every run at the page's 800x600.

import { spawn } from 'node:child_process';
import { appendFileSync, copyFileSync, existsSync, linkSync, mkdirSync, readdirSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');
const argv = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = argv.indexOf(`--${name}`);
  return i < 0 ? fallback : argv.splice(i, 2)[1];
};
const flag = (name) => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 && argv.splice(i, 1).length > 0;
};
const headed = flag('headed');
const fresh = flag('fresh');
// Frames as fast as the game makes them, not at the display's rate: the
// measure of how fast it runs (the game asks for vsync).
const vsync = flag('vsync');
const out = resolve(opt('out', join(root, 'target/bench/farcry')));
const levelSecs = Number(opt('level-secs', 20));
const profileMs = Number(opt('profile-ms', 10000));
const syms = opt('syms', '');
const port = opt('port', '19621');
const pageArgs = opt('page-args', '');
const chromeArgs = opt('chrome-args', '');
const fixedStep = opt('fixed-step', '');
const levelFrames = Number(opt('level-frames', 1500));
let dir = resolve(argv[0] ?? join(root, 'target/games/farcry-x/far-cry-demo'));
mkdirSync(dirname(out), { recursive: true });

// --fixed-step S: the game advances S seconds a frame, whatever the frame
// rate (Far Cry's fixed_time_step), so frame N shows the same scene on
// every run and runs compare (in real time the boat drifts and the draws
// per frame with it). Set in systemcfgoverride.lua, which the game reads
// after its own systemcfg.lua, in a copy of the folder made of hard links
// (the original is left alone).
if (fixedStep) {
  const copy = join(dirname(out), `farcry-folder-step-${fixedStep}`);
  const link = (from, to) => {
    for (const e of readdirSync(from, { withFileTypes: true })) {
      const [f, t] = [join(from, e.name), join(to, e.name)];
      if (e.isDirectory()) {
        mkdirSync(t, { recursive: true });
        link(f, t);
      } else if (!existsSync(t)) {
        try {
          linkSync(f, t);
        } catch {
          copyFileSync(f, t);
        }
      }
    }
  };
  mkdirSync(copy, { recursive: true });
  link(dir, copy);
  writeFileSync(join(copy, 'systemcfgoverride.lua'), `fixed_time_step = "${fixedStep}"\n`);
  dir = copy;
}

const perf = 'window.webwindows.d3dPerf()';
const commands = [
  `waitpage (${perf}?.fps ?? 0) > 20 600000`,
  'wait 3000',
  'fps 5000',
  'frame menu',
  'focus',
  'wait 500',
  'move -306 -174 20',
  'wait 500',
  'move -40 0 5',
  'wait 300',
  'click',
  'wait 3000',
  'move 556 324 25',
  'wait 400',
  'move 139 81 10',
  'wait 400',
  'click',
  // The level draws hundreds of times a frame; the menu and the loading
  // screen tens.
  `waitpage (${perf}?.drawsPerFrame ?? 0) > 300 600000`,
  // With a fixed step, the same frames of the level on every run (300 to
  // settle, then levelFrames); otherwise 10 s to settle and a time window.
  ...(fixedStep ? [`frames ${levelFrames} 300`] : ['wait 10000', `fps ${levelSecs * 1000}`]),
  'frame level',
  `profile ${profileMs} level`,
  'quit',
];
writeFileSync(`${out}.cmd`, commands.join('\n') + '\n');

const args = [join(root, 'tools/web/drive.mjs'), dir, 'FarCry.exe', '--out', out, '--port', port, '--profile', join(dirname(out), `profile-${headed ? 'headed' : 'headless'}`)];
// Headless, frames stay on the GPU too (offscreen): the frame statistics
// come from presenting, and GDI read-back would add its own readbacks.
args.push(...(headed ? ['--headed'] : ['--present', 'offscreen']));
if (fresh) args.push('--fresh');
if (syms) args.push('--syms', syms);
const page = new URLSearchParams(pageArgs);
if (!vsync) page.set('d3dvsync', '0');
if ([...page].length) args.push('--page-args', page.toString());
if (chromeArgs) args.push('--chrome-args', chromeArgs);
const t0 = Date.now();
const child = spawn(process.execPath, args, { stdio: ['ignore', 'pipe', 'pipe'] });
const result = { headed, fresh, vsync, started: new Date().toISOString() };
const fpsResults = [];
const waits = [];
writeFileSync(`${out}.log`, '');
const onLine = (line) => {
  appendFileSync(`${out}.log`, line + '\n');
  const fps = /^fps (\{.*\})$/.exec(line);
  if (fps) fpsResults.push(JSON.parse(fps[1]));
  const w = /^\[(\d+) s\] waitpage (met|timed out)/.exec(line);
  if (w) waits.push({ s: Number(w[1]), met: w[2] === 'met' });
  if (/^\[\d+ s\] (> |waitpage|screen up|no screen)|^fps |profiling|^\s+[\d.]+%|worker \(|by category|by image|top \d+/.test(line)) console.log(line);
};
let buf = '';
const feed = (d) => {
  buf += d;
  let i;
  while ((i = buf.indexOf('\n')) >= 0) {
    onLine(buf.slice(0, i));
    buf = buf.slice(i + 1);
  }
};
child.stdout.on('data', feed);
child.stderr.on('data', feed);
const code = await new Promise((r) => child.on('close', r));
Object.assign(result, {
  exit: code,
  totalSeconds: Math.round((Date.now() - t0) / 1000),
  toMenuSeconds: waits[0]?.met ? waits[0].s : null,
  toLevelSeconds: waits[1]?.met ? waits[1].s : null,
  menu: fpsResults[0] ?? null,
  level: fpsResults[1] ?? null,
});
writeFileSync(`${out}.json`, JSON.stringify(result, null, 1));
console.log(`\nFar Cry ${headed ? 'headed' : 'headless'}${fresh ? ', fresh profile' : ''}: menu in ${result.toMenuSeconds ?? '-'} s at ${result.menu?.fps ?? '-'} fps; level in ${result.toLevelSeconds ?? '-'} s at ${result.level?.fps ?? '-'} fps (${result.level?.drawsPerFrame ?? '-'} draws/frame, render worker ${result.level ? Math.round(result.level.renderBusy * 100) : '-'}% busy)`);
console.log(`summary: ${out}.json, log: ${out}.log`);
process.exit(code ?? 1);
