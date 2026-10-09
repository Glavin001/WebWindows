// Page logic: choose a folder, pick an .exe, run it in a worker. Windowed
// programs (on Wine) draw into a screen shared with the worker, shown in a
// canvas; the canvas's mouse and keyboard events go back through an
// InputRing.

import { InputRing } from '../wine/input-ring.mjs';
import { createAudioRing, playAudioRing } from '../wine/audio-sink.mjs';
import { windowsKey, KEYEVENTF_KEYUP } from './keys.mjs';
import { hasMemory64 } from '../runtime.mjs';

const $ = (id) => document.getElementById(id);
const out = $('out');
const logEl = $('log');
let folder = new Map(); // relative path -> File

const params = new URLSearchParams(location.search);
const translatorUrl = params.get('translator') ?? new URL('../../target/wasm32-unknown-unknown/release-wasm/wwt_wasm.wasm', import.meta.url).href;

const bundleUrl = params.get('bundle') ?? new URL('../../target/wine-bundle/', import.meta.url).href;
// 64-bit programs run on Wine's x86_64 DLLs from their own bundle.
const bundle64Url = params.get('bundle64') ?? new URL('../../target/wine-bundle64/', import.meta.url).href;
// ... or, in browsers without 64-bit WebAssembly memory, from a bundle for a
// 32-bit memory (the DLLs below 2 GB, the Unix side lowered to wasm32).
const bundle64m32Url = params.get('bundle64m32') ?? new URL('../../target/wine-bundle64-m32/', import.meta.url).href;
if (params.get('wine')) $('wine').checked = true;
// 64-bit WebAssembly memory: WebKit (Safari, and every browser on iOS) does
// not ship it yet. Without it 64-bit programs run below 4 GB on a 32-bit
// memory, on Wine from the 32-bit-memory bundle.
const memory64 = hasMemory64();
$('mem64note').hidden = memory64;
// ?args=a+b: the program's command line (with ?exe=).
if (params.get('args')) $('args').value = params.get('args');

if (!crossOriginIsolated) {
  $('status').textContent = 'This page needs cross-origin isolation (COOP/COEP headers) for shared memory; serve it with runtime/web/serve.mjs.';
}

function setPrograms() {
  const exes = [...folder.keys()].filter((k) => k.toLowerCase().endsWith('.exe')).sort();
  const sel = $('exe');
  sel.innerHTML = '';
  for (const e of exes) sel.add(new Option(e, e));
  sel.disabled = !exes.length;
  $('run').disabled = !exes.length;
  if (!exes.length) sel.add(new Option('No .exe in this folder'));
}

async function readDir(handle, prefix = '') {
  for await (const [name, h] of handle.entries()) {
    if (h.kind === 'file') folder.set(prefix + name, await h.getFile());
    else if (prefix.split('/').length < 4) await readDir(h, prefix + name + '/');
  }
}

$('pick').onclick = async () => {
  if (window.showDirectoryPicker) {
    try {
      const dir = await window.showDirectoryPicker();
      folder = new Map();
      await readDir(dir);
      setPrograms();
    } catch {}
  } else {
    $('fallback').click();
  }
};
$('fallback').onchange = (e) => {
  folder = new Map();
  for (const f of e.target.files) folder.set(f.webkitRelativePath.split('/').slice(1).join('/'), f);
  setPrograms();
};

// ---- The screen -----------------------------------------------------------

const canvas = $('screen');
const ctx = canvas.getContext('2d');
let screen = null; // { width, height, screen, frame, input, ring }
let worker = null;
let audioCtx = null; // the page's sound output (../wine/audio-sink.mjs)

/** A new screen for a program: shared pixels, a frame counter and the input ring. */
function newScreen() {
  const { width, height } = canvas;
  const ring = InputRing.create();
  return {
    width,
    height,
    screen: new SharedArrayBuffer(width * height * 4),
    frame: new SharedArrayBuffer(4),
    input: ring.buffer,
    ring,
  };
}

// Copies the shared screen into the canvas when the worker drew a new frame.
let shown = -1;
function paint() {
  if (screen) {
    const n = Atomics.load(new Int32Array(screen.frame), 0);
    if (n !== shown) {
      shown = n;
      // ImageData cannot use shared memory: copy.
      const img = new ImageData(screen.width, screen.height);
      img.data.set(new Uint8ClampedArray(screen.screen));
      ctx.putImageData(img, 0, 0);
      window.framesShown = (window.framesShown ?? 0) + 1;
    }
  }
  requestAnimationFrame(paint);
}
requestAnimationFrame(paint);

// ---- Frame rate and load --------------------------------------------------
//
// Over the screen, since programs rarely show their own: Direct3D's
// presented frames (the render worker's {type: 'perf'}, twice a second),
// else how often the screen changed, and the system calls and threads from
// the latest status sample. The "stats" box (or ?stats=0) hides it.
let d3dPerf = null; // the latest {type: 'perf'}, with when it came
const perfBox = $('perf');
const showPerf = $('showperf');
try {
  const saved = params.get('stats') ?? localStorage.getItem('webwindows:stats');
  if (saved !== null) showPerf.checked = saved !== '0';
} catch {}
showPerf.onchange = () => {
  try {
    localStorage.setItem('webwindows:stats', showPerf.checked ? '1' : '0');
  } catch {}
  updatePerf();
};
let perfFrames = { at: performance.now(), shown: 0 };
const fmt = (n) => (n >= 10000 ? `${(n / 1000).toFixed(0)}k` : n >= 1000 ? `${(n / 1000).toFixed(1)}k` : `${Math.round(n)}`);
function updatePerf() {
  const now = performance.now();
  const shownNow = window.framesShown ?? 0;
  const screenFps = ((shownNow - perfFrames.shown) * 1000) / Math.max(1, now - perfFrames.at);
  perfFrames = { at: now, shown: shownNow };
  perfBox.hidden = !showPerf.checked || canvas.hidden;
  if (perfBox.hidden) return;
  const lines = [];
  // Direct3D's numbers while it presents (a stale report means it stopped).
  const d = d3dPerf && now - d3dPerf.at < 1500 && d3dPerf.fps > 0 ? d3dPerf : null;
  if (d) {
    lines.push(`${d.fps.toFixed(0)} fps  ${d.frameMs.toFixed(1)} ms (worst ${d.worstMs.toFixed(0)})`);
    lines.push(`Direct3D: ${fmt(d.drawsPerFrame)} draws/frame, GPU worker ${Math.round(d.busy * 100)}% busy`);
  } else {
    lines.push(`${screenFps.toFixed(0)} fps (screen updates)`);
  }
  const s = statusHistory.at(-1);
  if (s?.syscalls) {
    const threads = s.threads?.length ?? 0;
    const running = s.threads?.filter((t) => t.state === 'running').length ?? 0;
    lines.push(`CPU: ${fmt(s.syscalls.perSec)} syscalls/s, ${threads} threads (${running} running)`);
  }
  perfBox.textContent = lines.join('\n');
}
setInterval(updatePerf, 500);

const at = (e) => {
  const r = canvas.getBoundingClientRect();
  return [Math.floor(((e.clientX - r.left) * canvas.width) / r.width), Math.floor(((e.clientY - r.top) * canvas.height) / r.height)];
};
// MOUSEEVENTF_* for buttons 0 (left), 1 (middle), 2 (right): [down, up].
const BUTTONS = [[0x2, 0x4], [0x20, 0x40], [0x8, 0x10]];
// Display.RELATIVE: the event's x and y are a movement, not a position.
const RELATIVE = 0x10000;
// Where the program confines the cursor (ClipCursor), or null. Games that
// read relative mouse movement (Quake moves the cursor back to its window's
// centre every frame) confine it to their window: then a click locks the
// pointer, and movement goes to Wine as movement, which it adds to the
// cursor position. Esc (the browser's) releases the lock.
let clip = null;
const locked = () => document.pointerLockElement === canvas;
// A Direct3D window over the whole screen (a fullscreen game) also gets
// the locked pointer: such games draw their own cursor from relative
// movement (UT2004's menus), which an absolute pointer leaves behind at
// the canvas's edge.
const fullscreenD3D = () => {
  const w = window.d3dWindow;
  return !!(w && w.visible && screen && w.x <= 0 && w.y <= 0 && w.width >= screen.width && w.height >= screen.height);
};
const wantsLock = () => !!clip || fullscreenD3D();
let rest = [0, 0];
const mouse = (e, flags = 0, data = 0) => {
  if (!locked()) return screen?.ring.mouse(...at(e), flags, data);
  if (flags) return screen?.ring.mouse(0, 0, flags | RELATIVE, data);
  // Movement in canvas pixels, fractions carried to the next event.
  const r = canvas.getBoundingClientRect();
  const x = (e.movementX * canvas.width) / r.width + rest[0], y = (e.movementY * canvas.height) / r.height + rest[1];
  const dx = Math.trunc(x), dy = Math.trunc(y);
  rest = [x - dx, y - dy];
  if (dx || dy) screen?.ring.mouse(dx, dy, RELATIVE);
};
function setClip(rect) {
  clip = rect;
  if (!wantsLock() && locked()) document.exitPointerLock();
  updateMouseHint();
}
function updateMouseHint() {
  const s = $('status');
  s.textContent = s.textContent.replace(/ \(.*\)$/, '');
  if (wantsLock() && !locked() && screen && !canvas.hidden) s.textContent += ' (click the screen to capture the mouse; Esc releases it)';
}
document.addEventListener('pointerlockchange', updateMouseHint);
canvas.addEventListener('pointermove', (e) => mouse(e));
canvas.addEventListener('pointerdown', (e) => {
  canvas.focus();
  if (wantsLock() && !locked()) Promise.resolve(canvas.requestPointerLock?.()).catch(() => {});
  // Throws while a pointer lock request is pending; the click must still
  // reach the program.
  if (!locked()) {
    try {
      canvas.setPointerCapture(e.pointerId);
    } catch {}
  }
  if (BUTTONS[e.button]) mouse(e, BUTTONS[e.button][0]);
  e.preventDefault();
});
canvas.addEventListener('pointerup', (e) => {
  if (BUTTONS[e.button]) mouse(e, BUTTONS[e.button][1]);
});
canvas.addEventListener('contextmenu', (e) => e.preventDefault());
canvas.addEventListener(
  'wheel',
  (e) => {
    mouse(e, 0x800, e.deltaY < 0 ? 120 : -120);
    e.preventDefault();
  },
  { passive: false },
);
for (const type of ['keydown', 'keyup']) {
  canvas.addEventListener(type, (e) => {
    const k = windowsKey(e.code);
    if (!k || !screen) return;
    screen.ring.key(k.vk, k.scan, k.flags | (type === 'keyup' ? KEYEVENTF_KEYUP : 0));
    e.preventDefault();
  });
}

// Direct3D frames: a canvas over the screen that the program's render
// worker presents to, moved over the Direct3D window as it reports where
// that is. ?d3dpresent=gdi keeps the frames in the screen instead (read
// back and drawn with GDI), to compare; ?d3dpresent=offscreen presents to
// a buffer only window.d3dSnapshot() reads, for headless tests.
let d3dCanvas = null;
let d3dPort = null;
const d3dDebugReplies = [];
function newD3DCanvas() {
  d3dCanvas?.remove();
  d3dCanvas = null;
  d3dPort?.close();
  d3dPort = null;
  const mode = params.get('d3dpresent');
  if (mode === 'gdi' || !HTMLCanvasElement.prototype.transferControlToOffscreen) return null;
  // The render worker's messages, and window.d3dSnapshot() for tests: the
  // next presented frame as {width, height, pixels} (RGBA).
  const channel = new MessageChannel();
  d3dPort = channel.port1;
  const snapshots = [];
  d3dPort.onmessage = (e) => {
    if (e.data.type === 'perf') d3dPerf = { ...e.data, at: performance.now() };
    else if (e.data.type === 'debug') d3dDebugReplies.shift()?.(e.data);
    else if (e.data.type === 'log') logEl.textContent += `d3d: ${e.data.text}\n`;
    else if (e.data.type === 'frame') snapshots.shift()?.(e.data);
  };
  const port = d3dPort;
  // ?d3dstats=1: the render worker reports how busy it is, once a second.
  if (params.get('d3dstats')) port.postMessage({ type: 'stats' });
  window.d3dSnapshot = () => new Promise((resolve) => (snapshots.push(resolve), port.postMessage({ type: 'snapshot' })));
  if (mode === 'offscreen') return { offscreen: true, port: channel.port2 };
  d3dCanvas = document.createElement('canvas');
  d3dCanvas.className = 'd3d';
  d3dCanvas.hidden = true;
  d3dCanvas.width = 640;
  d3dCanvas.height = 480;
  $('screenbox').append(d3dCanvas);
  return { canvas: d3dCanvas.transferControlToOffscreen(), port: channel.port2 };
}
function placeD3D(w) {
  window.d3dWindow = w;
  updateMouseHint();
  if (!d3dCanvas || !screen) return;
  const pct = (v, total) => `${(v / total) * 100}%`;
  Object.assign(d3dCanvas.style, {
    left: pct(w.x, screen.width),
    top: pct(w.y, screen.height),
    width: pct(w.width, screen.width),
    height: pct(w.height, screen.height),
  });
  d3dCanvas.hidden = !w.visible || !w.width || !w.height;
}

// ---- The record of a run ---------------------------------------------------

// What the program printed, the runtime's log and the files the program
// wrote in its folder (a game's log.txt), saved in localStorage every two
// seconds and when the program ends, so it outlives a hang, a crash or a
// reload. window.webwindows.lastRun() and .previousRun() read it back;
// .download() saves it as a JSON file.
const RECORD = 'webwindows.lastRun';
const PREVIOUS = 'webwindows.previousRun';
const tail = (text, n) => (text.length > n ? text.slice(-n) : text);
let record = null;
let recordTimer = 0;
function saveRecord() {
  if (!record) return;
  record.saved = new Date().toISOString();
  record.status = $('status').textContent;
  record.out = tail(out.textContent, 192 << 10);
  record.log = tail(logEl.textContent, 192 << 10);
  // Logs (.txt, .log), and the six other files written last, each up to
  // its last 128 KB.
  const entries = Object.entries(record.files);
  const isLog = ([k]) => /\.(txt|log)$/i.test(k);
  const files = [...entries.filter(isLog), ...entries.filter((e) => !isLog(e)).slice(-6)].map(([k, v]) => [k, tail(v, 128 << 10)]);
  for (const kept of [files, []]) {
    try {
      localStorage.setItem(RECORD, JSON.stringify({ ...record, files: Object.fromEntries(kept) }));
      return;
    } catch {}
  }
}
function startRecord(exe) {
  try {
    const last = localStorage.getItem(RECORD);
    if (last) localStorage.setItem(PREVIOUS, last);
  } catch {}
  record = { exe, started: new Date().toISOString(), files: {}, status: [] };
  statusHistory.length = 0;
  clearInterval(recordTimer);
  recordTimer = setInterval(saveRecord, 2000);
}
function endRecord(exit) {
  if (!record) return;
  record.exit = exit;
  saveRecord();
  clearInterval(recordTimer);
}
addEventListener('pagehide', saveRecord);
const readRecord = (key) => {
  try {
    return JSON.parse(localStorage.getItem(key));
  } catch {
    return null;
  }
};
// Status samples from the worker (runtime/wine/status.mjs), every 2 s:
// threads and where they are, system calls, screen, windows, input. Kept
// here (the last 60, the last 30 also in the run record) and logged on the
// console as "webwindows:status <json>" for tools that watch it.
const statusHistory = [];
function onStatus(s) {
  statusHistory.push(s);
  if (statusHistory.length > 60) statusHistory.shift();
  if (record) record.status = statusHistory.slice(-30);
  console.debug('webwindows:status ' + JSON.stringify(s));
}

// ---- Debug report -----------------------------------------------------------
//
// One JSON file with what a bug report needs: the build, browser, screen and
// GPU, the URL's options, the run record (output, logs, status samples, files
// the program wrote), Direct3D's state and frame rate, the program folder's
// listing and its small configuration files, the frame the screen shows and
// a copy of the last Direct3D frame presented (read from the GPU; the canvas
// is left alone), and the user's own description.
const withTimeout = (promise, ms) => Promise.race([promise, new Promise((r) => setTimeout(() => r(null), ms))]);
const pngOf = (width, height, pixels) => {
  if (!width || !height || !pixels?.length) return null;
  const c = document.createElement('canvas');
  c.width = width;
  c.height = height;
  c.getContext('2d').putImageData(new ImageData(new Uint8ClampedArray(pixels), width, height), 0, 0);
  return c.toDataURL('image/png');
};
async function debugReport() {
  const notes = prompt('What happened, and what did you expect? (optional)') ?? '';
  saveRecord();
  const r = { kind: 'webwindows debug report', created: new Date().toISOString(), notes };
  r.build = await fetch(new URL('../../version.txt', location.href)).then((x) => (x.ok ? x.text() : null), () => null);
  r.page = { url: location.href, options: Object.fromEntries(params), crossOriginIsolated, memory64 };
  r.browser = {
    userAgent: navigator.userAgent,
    platform: navigator.userAgentData?.platform ?? navigator.platform,
    brands: navigator.userAgentData?.brands,
    language: navigator.language,
    cores: navigator.hardwareConcurrency,
    deviceMemoryGB: navigator.deviceMemory,
    devicePixelRatio,
    window: [innerWidth, innerHeight],
    display: [window.screen.width, window.screen.height],
  };
  try {
    const a = await navigator.gpu?.requestAdapter();
    if (a) {
      const i = a.info ?? {};
      r.webgpu = {
        vendor: i.vendor,
        architecture: i.architecture,
        device: i.device,
        description: i.description,
        features: [...a.features].sort(),
        limits: Object.fromEntries(['maxTextureDimension2D', 'maxBufferSize', 'maxStorageBufferBindingSize', 'maxBindGroups', 'maxColorAttachments'].map((k) => [k, a.limits[k]])),
      };
    } else r.webgpu = navigator.gpu ? 'no adapter' : 'no WebGPU';
  } catch (e) {
    r.webgpu = String(e);
  }
  r.run = window.webwindows.lastRun();
  r.previousRun = readRecord(PREVIOUS);
  r.statusHistory = statusHistory.slice();
  r.d3d = {
    perf: d3dPerf,
    window: window.d3dWindow ?? null,
    canvas: d3dCanvas && { hidden: d3dCanvas.hidden, style: d3dCanvas.getAttribute('style'), box: d3dCanvas.getBoundingClientRect().toJSON() },
    worker: d3dPort ? await withTimeout(new Promise((res) => (d3dDebugReplies.push(res), d3dPort.postMessage({ type: 'debug' }))), 8000) : null,
  };
  // The program folder: every file's name and size, and the small text
  // files a program reads its settings from.
  r.folder = [...folder].slice(0, 5000).map(([k, f]) => [k, f.size]);
  r.folderConfig = {};
  let budget = 1 << 20;
  for (const [k, f] of folder) {
    if (!/\.(ini|cfg|lua|conf|json|xml|txt|log)$/i.test(k) || f.size > 64 << 10 || k.split('/').length > 2 || f.size > budget) continue;
    r.folderConfig[k] = await f.text();
    budget -= f.size;
  }
  // What the screen shows, and the last Direct3D frame presented (read from
  // the GPU, as the canvas shows it before scaling).
  try {
    r.screenPng = canvas.hidden ? null : canvas.toDataURL('image/png');
  } catch {}
  const frame = r.d3d.worker?.frame;
  r.d3dFramePng = frame ? pngOf(frame.width, frame.height, frame.pixels) : null;
  if (frame) r.d3d.worker.frame = { width: frame.width, height: frame.height };
  const a = document.createElement('a');
  a.href = URL.createObjectURL(new Blob([JSON.stringify(r, null, 1)], { type: 'application/json' }));
  a.download = `webwindows-debug-${(r.run?.exe ?? 'page').split(/[\\/]/).pop()}-${r.created.replace(/[:.]/g, '-')}.json`;
  a.click();
  setTimeout(() => URL.revokeObjectURL(a.href), 10000);
  return r;
}
$('debugreport').onclick = () => {
  $('debugreport').disabled = true;
  debugReport().finally(() => ($('debugreport').disabled = false));
};

window.webwindows = {
  /** Saves the debug report (see debugReport) and returns it. */
  debugReport: () => debugReport(),
  /** The latest status sample, and the last 60. */
  status: () => statusHistory.at(-1) ?? null,
  statusHistory: () => statusHistory.slice(),
  lastRun: () => (saveRecord(), readRecord(RECORD)),
  previousRun: () => readRecord(PREVIOUS),
  download(which = 'lastRun') {
    const r = which === 'previousRun' ? readRecord(PREVIOUS) : window.webwindows.lastRun();
    if (!r) return;
    const a = document.createElement('a');
    a.href = URL.createObjectURL(new Blob([JSON.stringify(r, null, 1)], { type: 'application/json' }));
    a.download = `webwindows-${r.exe}-${r.started.replace(/[:.]/g, '-')}.json`;
    a.click();
    URL.revokeObjectURL(a.href);
  },
};

// ---- Running ---------------------------------------------------------------

async function run(exeName, exeBytes, files, exePath, times = {}) {
  saveRecord();
  startRecord(exePath ?? exeName);
  out.textContent = '';
  logEl.textContent = '';
  $('status').textContent = `running ${exeName}…`;
  $('run').disabled = true;
  worker?.terminate();
  setClip(null);
  worker = new Worker(new URL('worker.mjs', import.meta.url), { type: 'module' });
  const dec = new TextDecoder('latin1');
  const wine = $('wine').checked;
  screen = wine ? newScreen() : null;
  // Sound: started here, inside the click that runs the program (browsers
  // start audio only from a user gesture).
  let audio = null;
  if (wine && typeof AudioContext !== 'undefined') {
    const buffer = createAudioRing();
    try {
      audioCtx?.close();
      audioCtx = await playAudioRing(buffer);
      audio = { buffer, rate: audioCtx.sampleRate };
      window.audioRing = buffer; // for tests: [write, read] frame counts first
    } catch (e) {
      logEl.textContent += `no sound: ${e.message}\n`;
    }
  }
  shown = -1;
  canvas.hidden = true;
  const d3dOffscreen = screen ? newD3DCanvas() : null;
  return new Promise((resolve) => {
    worker.onmessage = (e) => {
      const m = e.data;
      if (m.type === 'stdout' || m.type === 'stderr') out.textContent += dec.decode(m.bytes);
      else if (m.type === 'log') logEl.textContent += m.text + '\n';
      else if (m.type === 'files') Object.assign(record.files, m.files);
      else if (m.type === 'status') onStatus(m.status);
      else if (m.type === 'd3d-window') placeD3D(m);
      else if (m.type === 'clip') setClip(m.rect);
      else if (m.type === 'screen') {
        canvas.hidden = false;
        canvas.focus();
        $('status').textContent = `${exeName} is running`;
        updateMouseHint();
        window.screenShown = true;
      } else if (m.type === 'exit') {
        setClip(null);
        $('status').textContent = m.code === null ? `${exeName} stopped with an error` : `${exeName} exited with code ${m.code} (${m.runMs?.toFixed(0)} ms)`;
        $('run').disabled = false;
        worker.terminate();
        if (d3dCanvas) d3dCanvas.hidden = true;
        window.lastExit = m;
        endRecord({ code: m.code, runMs: m.runMs });
        resolve(m);
      }
    };
    // The worker itself failed (an error out of the runtime, or out of memory).
    worker.onerror = (e) => {
      out.textContent += `\n*** worker error: ${e.message ?? e}\n`;
      saveRecord();
    };
    const args = $('args').value.trim();
    worker.postMessage(
      {
        exeName,
        exePath,
        exeBytes,
        files,
        times,
        argv: args ? args.split(/\s+/) : [],
        translatorUrl,
        noCache: $('nocache').checked,
        // The program is translated with bounds traps instead of memory
        // checks (wwt translate --mem-traps) where the engine reports where
        // a trap happened (Chrome, Firefox; not Safari). "faithful memory
        // checks" (?memtraps=0) turns them off; ?memtraps=1 forces them.
        memTraps: $('faithful').checked ? false : params.get('memtraps') === '1' ? true : undefined,
        wine,
        bundleUrl,
        bundle64Url,
        bundle64m32Url,
        memory64,
        // ?debug=+d3d: Wine's debug channels (WINEDEBUG), on stderr.
        debug: params.get('debug') ?? '',
        // ?unixtrace=win,key: the channels of Wine's Unix side (win32u,
        // wineserver, the display driver "browser"; "all" for every one).
        unixTrace: params.get('unixtrace') ?? '',
        display: screen && { width: screen.width, height: screen.height, screen: screen.screen, frame: screen.frame, input: screen.input },
        audio,
        d3dCanvas: d3dOffscreen?.canvas,
        d3dOffscreen: d3dOffscreen?.offscreen,
        d3dPort: d3dOffscreen?.port,
      },
      // The folder's files move to the worker rather than being copied
      // (a game's data is hundreds of megabytes).
      [exeBytes, ...Object.values(files), d3dOffscreen?.canvas, d3dOffscreen?.port].filter(Boolean),
    );
  });
}

$('run').onclick = async () => {
  const name = $('exe').value;
  const files = {};
  // When each file was last written, for the program's file times (games
  // compare them with what their caches recorded).
  const times = {};
  for (const [k, f] of folder) (files[k] = await f.arrayBuffer()), (times[k] = f.lastModified);
  const exe = await folder.get(name).arrayBuffer();
  run(name.split('/').pop(), exe, files, name, times);
};

// Wine's own programs from the bundles that carry Wine's Unix side (the
// 64-bit one for this browser's memory), the benchmark programs the site
// was built with (./apps.json, from tools/site/apps.mjs, with the files
// they read), then the repository's test programs (./samples.json). Every
// sample runs on Wine.
const sampleArgs = new Map();
const sampleFiles = new Map();
const bundle64ForBrowser = memory64 ? bundle64Url : bundle64m32Url;
const manifestOf = (url) => fetch(new URL('manifest.json', url)).then((r) => (r.ok ? r.json() : null)).catch(() => null);
Promise.all([
  manifestOf(bundleUrl),
  manifestOf(bundle64ForBrowser),
  fetch(new URL('samples.json', import.meta.url)).then((r) => (r.ok ? r.json() : [])),
  fetch(new URL('apps.json', import.meta.url))
    .then((r) => (r.ok ? r.json() : []))
    .catch(() => []),
])
  .then(([manifest, manifest64, samples, apps]) => {
    if (!manifest?.programs?.length) return;
    const groups = new Map();
    const group = (label) => {
      if (!groups.has(label)) {
        const g = document.createElement('optgroup');
        g.label = label;
        $('sample').append(g);
        groups.set(label, g);
      }
      return groups.get(label);
    };
    for (const p of manifest.programs) group("Wine's programs").append(new Option(p.split('/').pop(), new URL(p, bundleUrl).href));
    for (const p of manifest64?.programs ?? []) {
      group("Wine's programs, 64-bit").append(new Option(`${p.split('/').pop()} (64-bit)`, new URL(p, bundle64ForBrowser).href));
    }
    for (const s of [...apps, ...samples]) {
      const url = new URL(`../../${s.path}`, import.meta.url).href;
      group(s.group).append(new Option(s.label, url));
      if (s.args) sampleArgs.set(url, s.args);
      if (s.files) sampleFiles.set(url, s.files);
    }
    $('samples').hidden = false;
  })
  .catch(() => {});
// A sample's suggested arguments go in the arguments box.
$('sample').onchange = () => ($('args').value = sampleArgs.get($('sample').value) ?? '');
$('runsample').onclick = async () => {
  const url = $('sample').value;
  const bytes = await (await fetch(url)).arrayBuffer();
  $('wine').checked = true;
  // A sample with files runs from C:\app, with them beside it.
  const files = {};
  for (const [name, path] of Object.entries(sampleFiles.get(url) ?? {})) {
    files[name] = await (await fetch(new URL(`../../${path}`, import.meta.url))).arrayBuffer();
  }
  const name = url.split('/').pop();
  run(name, bytes, files, sampleFiles.has(url) ? name : undefined);
};

// ?exe=<url> runs a program directly (used by tests and demos).
if (params.get('memtraps') === '0') $('faithful').checked = true;
if (params.get('exe')) {
  const url = params.get('exe');
  const bytes = await (await fetch(url)).arrayBuffer();
  if (params.get('nocache')) $('nocache').checked = true;
  run(url.split('/').pop(), bytes, {});
}
