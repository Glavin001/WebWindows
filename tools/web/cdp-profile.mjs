#!/usr/bin/env node
// CPU profiles of the page's workers through the Chrome DevTools Protocol,
// and what they say about a running program: where the guest worker's time
// goes (translated x86 code by DLL and function, the translator's helpers,
// Wine's Unix side, the JavaScript host, waiting) and the render worker's.
//
// Used by tools/web/drive.mjs (`profile MS`), or on saved profiles:
//
//   node tools/web/cdp-profile.mjs PROFILE.cpuprofile [--images IMAGES.json] [--syms DIR] [--top N]
//
// Translated functions are named `x86_<address>` or `<export>@<address>`
// (crates/wwt/src/codegen.rs); IMAGES ([base, size, path] of the loaded
// images, window.webwindows.images()) turns an address into DLL+offset.
// With --syms, a directory of unstripped PE files (Wine's build tree, e.g.
// copied from /opt/wine-build/dlls/*/i386-windows), offsets are named with
// i686-w64-mingw32-addr2line. The .cpuprofile files open in Chrome
// DevTools (Performance, "Load profile").

import { execFileSync } from 'node:child_process';
import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { basename, join } from 'node:path';
import { fileURLToPath } from 'node:url';

/** A DevTools Protocol client over the browser's WebSocket, with flat sessions. */
export class Cdp {
  static async connect(port) {
    const v = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
    if (typeof WebSocket !== 'function') throw new Error('profiling needs a WebSocket global (Node 22.4 or later)');
    const ws = new WebSocket(v.webSocketDebuggerUrl);
    await new Promise((res, rej) => {
      ws.onopen = res;
      ws.onerror = () => rej(new Error(`cannot connect to ${v.webSocketDebuggerUrl}`));
    });
    return new Cdp(ws);
  }

  constructor(ws) {
    this.ws = ws;
    this.id = 0;
    this.pending = new Map();
    this.listeners = [];
    ws.onmessage = (e) => {
      const m = JSON.parse(e.data);
      if (m.id && this.pending.has(m.id)) {
        const { res, rej } = this.pending.get(m.id);
        this.pending.delete(m.id);
        if (m.error) rej(new Error(`${m.error.message} (${m.error.code})`));
        else res(m.result);
      } else if (m.method) {
        for (const l of this.listeners) l(m);
      }
    };
  }

  send(method, params = {}, sessionId) {
    const id = ++this.id;
    this.ws.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
    return new Promise((res, rej) => this.pending.set(id, { res, rej }));
  }

  close() {
    this.ws.close();
  }

  /**
   * The page whose URL contains `urlPart` and every worker under it, nested
   * ones too (the render worker is started by the guest worker):
   * [{ sessionId, type, url }].
   */
  async workers(urlPart) {
    const { targetInfos } = await this.send('Target.getTargets');
    const page = targetInfos.find((t) => t.type === 'page' && t.url.includes(urlPart));
    if (!page) throw new Error(`no page at ${urlPart}`);
    const found = [];
    const attached = (m) => {
      if (m.method !== 'Target.attachedToTarget') return;
      const { sessionId, targetInfo } = m.params;
      found.push({ sessionId, type: targetInfo.type, url: targetInfo.url });
      // Its own workers, and let it run (it is not waiting, but be sure).
      this.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: false, flatten: true }, sessionId).catch(() => {});
      this.send('Runtime.runIfWaitingForDebugger', {}, sessionId).catch(() => {});
    };
    this.listeners.push(attached);
    const { sessionId } = await this.send('Target.attachToTarget', { targetId: page.targetId, flatten: true });
    await this.send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: false, flatten: true }, sessionId);
    // Attach events arrive asynchronously, nested ones after their parent's.
    await new Promise((r) => setTimeout(r, 1000));
    this.listeners.splice(this.listeners.indexOf(attached), 1);
    return found.filter((w) => w.type === 'worker');
  }

  /** CPU profiles of the given sessions over the same `ms`, sampled every `intervalUs`. */
  async profile(sessions, ms, intervalUs = 250) {
    for (const s of sessions) {
      await this.send('Profiler.enable', {}, s);
      await this.send('Profiler.setSamplingInterval', { interval: intervalUs }, s);
    }
    await Promise.all(sessions.map((s) => this.send('Profiler.start', {}, s)));
    await new Promise((r) => setTimeout(r, ms));
    return Promise.all(sessions.map((s) => this.send('Profiler.stop', {}, s).then((r) => r.profile)));
  }
}

const GUEST = /^(?:(.*)@)?(?:x86_)?([0-9a-f]+)$/;

/** Which part of the system a profile node's own time belongs to. */
function classify(cf) {
  const name = cf.functionName;
  if (name === '(idle)' || name === '(program)' || name === '(garbage collector)' || name === '(root)') return { cat: name };
  // Modules compiled from bytes are wasm://wasm/<hash>; the runtime names
  // the ones it loads after their image (wined3d.dll.wasm).
  const wasm = cf.url.startsWith('wasm://') || cf.url.endsWith('.wasm') || cf.url === '';
  if (wasm && /^helper_/.test(name)) return { cat: 'translator helpers', name };
  const g = wasm && (/^x86_[0-9a-f]+$/.test(name) || /@[0-9a-f]+$/.test(name)) ? GUEST.exec(name) : null;
  if (g) return { cat: 'guest x86', addr: parseInt(g[2], 16), symbol: g[1] ?? null };
  if (wasm) return { cat: 'other wasm', name: name || '(anonymous)' };
  const file = cf.url.split('/').pop() || '(no url)';
  return { cat: 'JavaScript', name: `${file}:${name || '(anonymous)'}` };
}

/**
 * Self time by category and by function, and guest time by image.
 * @param {object} profile  a Profiler.Profile
 * @param {Array<[number, number, string]>} images  [base, size, path]
 */
export function summarize(profile, images = []) {
  const nodes = new Map(profile.nodes.map((n) => [n.id, n]));
  const self = new Map();
  let total = 0;
  for (let i = 0; i < profile.samples.length; i++) {
    const dt = profile.timeDeltas[i + 1] ?? 0; // a sample's time runs to the next one
    self.set(profile.samples[i], (self.get(profile.samples[i]) ?? 0) + dt);
    total += dt;
  }
  const sorted = [...images].sort((a, b) => a[0] - b[0]);
  const imageOf = (addr) => {
    for (const [base, size, path] of sorted) if (addr >= base && addr < base + size) return { dll: path.split('\\').pop(), offset: addr - base, path };
    return null;
  };
  const cats = new Map();
  const funcs = new Map();
  const dlls = new Map();
  for (const [id, us] of self) {
    const c = classify(nodes.get(id).callFrame);
    cats.set(c.cat, (cats.get(c.cat) ?? 0) + us);
    let key;
    if (c.cat === 'guest x86') {
      const img = imageOf(c.addr);
      const dll = img?.dll ?? '?';
      dlls.set(dll, (dlls.get(dll) ?? 0) + us);
      key = `${dll}+0x${(img?.offset ?? c.addr).toString(16)}${c.symbol ? ` ${c.symbol}` : ''}`;
      if (!funcs.has(key)) funcs.set(key, { us: 0, cat: c.cat, dll, offset: img?.offset, path: img?.path });
    } else {
      key = c.name ? `${c.cat}: ${c.name}` : c.cat;
      if (!funcs.has(key)) funcs.set(key, { us: 0, cat: c.cat });
    }
    funcs.get(key).us += us;
  }
  const byUs = (m) => [...m].sort((a, b) => b[1] - a[1]);
  return {
    totalMs: total / 1000,
    categories: byUs(cats).map(([k, us]) => [k, us / 1000]),
    dlls: byUs(dlls).map(([k, us]) => [k, us / 1000]),
    functions: [...funcs].sort((a, b) => b[1].us - a[1].us).map(([k, f]) => ({ key: k, ms: f.us / 1000, ...f })),
  };
}

/**
 * Names guest functions from unstripped PE files in `dir` (matched by file
 * name): the offset in the loaded image is the offset in the file's image.
 */
export function symbolize(summary, dir, limit = 60) {
  if (!dir || !existsSync(dir)) return;
  const files = new Map();
  const walk = (d) => {
    for (const n of readdirSync(d)) {
      const p = join(d, n);
      if (statSync(p).isDirectory()) walk(p);
      else files.set(n.toLowerCase(), p);
    }
  };
  walk(dir);
  const want = new Map();
  for (const f of summary.functions.slice(0, limit)) {
    if (f.cat !== 'guest x86' || f.offset === undefined) continue;
    const file = files.get(f.dll.toLowerCase());
    if (!file) continue;
    if (!want.has(file)) want.set(file, []);
    want.get(file).push(f);
  }
  for (const [file, fs] of want) {
    const bytes = readFileSync(file);
    const pe = bytes.readUInt32LE(0x3c);
    const imageBase = bytes.readUInt32LE(pe + 24 + 28);
    let out;
    try {
      out = execFileSync('i686-w64-mingw32-addr2line', ['-f', '-C', '-e', file, ...fs.map((f) => (imageBase + f.offset).toString(16))], { encoding: 'utf8' });
    } catch {
      continue;
    }
    const lines = out.split('\n');
    fs.forEach((f, i) => {
      const fn = lines[i * 2];
      const loc = lines[i * 2 + 1];
      if (fn && fn !== '??') f.symbol = `${fn} (${basename(loc ?? '').replace(/ \(discriminator.*$/, '')})`;
    });
  }
}

/** The summary as text. */
export function report(summary, { top = 40, title = '' } = {}) {
  const pct = (ms) => `${((100 * ms) / summary.totalMs).toFixed(1).padStart(5)}%`;
  const lines = [`${title}${summary.totalMs.toFixed(0)} ms sampled`];
  lines.push('  by category:');
  for (const [k, ms] of summary.categories) lines.push(`    ${pct(ms)}  ${ms.toFixed(0).padStart(7)} ms  ${k}`);
  if (summary.dlls.length) {
    lines.push('  guest x86 by image:');
    for (const [k, ms] of summary.dlls.slice(0, 20)) lines.push(`    ${pct(ms)}  ${ms.toFixed(0).padStart(7)} ms  ${k}`);
  }
  lines.push(`  top ${top} functions (self time):`);
  for (const f of summary.functions.slice(0, top)) lines.push(`    ${pct(f.ms)}  ${f.ms.toFixed(1).padStart(8)} ms  ${f.key}${f.symbol ? `  ${f.symbol}` : ''}`);
  return lines.join('\n');
}

// As a command: summarize a saved profile.
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const argv = process.argv.slice(2);
  const opt = (name) => {
    const i = argv.indexOf(`--${name}`);
    return i < 0 ? undefined : argv.splice(i, 2)[1];
  };
  const images = opt('images');
  const syms = opt('syms');
  const top = Number(opt('top') ?? 40);
  const [file] = argv;
  if (!file) {
    console.error('usage: cdp-profile.mjs PROFILE.cpuprofile [--images IMAGES.json] [--syms DIR] [--top N]');
    process.exit(2);
  }
  const s = summarize(JSON.parse(readFileSync(file, 'utf8')), images ? JSON.parse(readFileSync(images, 'utf8')) : []);
  symbolize(s, syms, top);
  console.log(report(s, { top }));
}
