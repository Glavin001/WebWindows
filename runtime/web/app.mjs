// Page logic: choose a folder, pick an .exe, run it in a worker. Windowed
// programs (on Wine) draw into a screen shared with the worker, shown in a
// canvas; the canvas's mouse and keyboard events go back through an
// InputRing.

import { InputRing } from '../wine/input-ring.mjs';
import { createAudioRing, playAudioRing } from '../wine/audio-sink.mjs';
import { windowsKey, KEYEVENTF_KEYUP } from './keys.mjs';
import { hasMemory64 } from '../runtime.mjs';
import { parseVisual, totals } from './d3d9-visual.mjs';

const $ = (id) => document.getElementById(id);
const out = $('out');
const logEl = $('log');
let folder = new Map(); // relative path -> File
let folderSummary = null; // { files, bytes, depth }, for the page and the debug report

const params = new URLSearchParams(location.search);

// ---- Settings ----------------------------------------------------------------
//
// Every setting is both a URL parameter and a control on the page: the URL
// wins when the page loads, and changing a control rewrites the URL (without
// reloading), so the address can always be copied to reproduce a run.
// key: [element id, default, kind]; checkboxes are "1"/"0" in the URL.
const SETTINGS = {
  wine: ['wine', false, 'check'],
  nocache: ['nocache', false, 'check'],
  stats: ['showperf', true, 'check'],
  d3dpresent: ['d3dpresent', 'canvas', 'value'],
  memtraps: ['memtraps', 'auto', 'value'],
  d3dstats: ['d3dstats', false, 'check'],
  args: ['args', '', 'value'],
  debug: ['debug', '', 'value'],
  unixtrace: ['unixtrace', '', 'value'],
  translator: ['translator', '', 'value'],
  bundle: ['bundle', '', 'value'],
  bundle64: ['bundle64', '', 'value'],
  bundle64m32: ['bundle64m32', '', 'value'],
};
/** A setting's current value, from its control. */
function setting(key) {
  const [id, , kind] = SETTINGS[key];
  return kind === 'check' ? $(id).checked : $(id).value.trim();
}
/** Every setting that differs from its default, as the URL carries them. */
function settings() {
  return Object.fromEntries(
    Object.entries(SETTINGS)
      .filter(([k, [, def]]) => setting(k) !== def)
      .map(([k, [, , kind]]) => [k, kind === 'check' ? (setting(k) ? '1' : '0') : setting(k)]),
  );
}
function writeUrl() {
  const q = new URLSearchParams(location.search);
  for (const [k, [, def, kind]] of Object.entries(SETTINGS)) {
    const v = setting(k);
    if (v === def) q.delete(k);
    else q.set(k, kind === 'check' ? (v ? '1' : '0') : v);
  }
  const search = q.toString();
  history.replaceState(null, '', search ? `?${search}` : location.pathname);
}
// The stats overlay is remembered between visits too.
try {
  if (!params.has('stats') && localStorage.getItem('webwindows:stats') !== null) params.set('stats', localStorage.getItem('webwindows:stats'));
} catch {}
for (const [k, [id, , kind]] of Object.entries(SETTINGS)) {
  const el = $(id);
  if (params.has(k)) {
    const v = params.get(k);
    if (kind === 'check') el.checked = v !== '0' && v !== '' && v !== 'false';
    else if (el.tagName === 'SELECT' && ![...el.options].some((o) => o.value === v)) el.add(new Option(v, v));
    if (kind !== 'check') el.value = v;
  }
  el.addEventListener(kind === 'check' || el.tagName === 'SELECT' ? 'change' : 'input', writeUrl);
}
// The debugging options start open when one of them is set.
if (['debug', 'unixtrace', 'translator', 'bundle', 'bundle64', 'bundle64m32'].some((k) => setting(k))) $('advanced').open = true;
writeUrl();

const translatorUrl = setting('translator') || new URL('../../target/wasm32-unknown-unknown/release-wasm/wwt_wasm.wasm', import.meta.url).href;
const bundleUrl = setting('bundle') || new URL('../../target/wine-bundle/', import.meta.url).href;
// 64-bit programs run on Wine's x86_64 DLLs from their own bundle.
const bundle64Url = setting('bundle64') || new URL('../../target/wine-bundle64/', import.meta.url).href;
// ... or, in browsers without 64-bit WebAssembly memory, from a bundle for a
// 32-bit memory (the DLLs below 2 GB, the Unix side lowered to wasm32).
const bundle64m32Url = setting('bundle64m32') || new URL('../../target/wine-bundle64-m32/', import.meta.url).href;
// 64-bit WebAssembly memory: WebKit (Safari, and every browser on iOS) does
// not ship it yet. Without it 64-bit programs run below 4 GB on a 32-bit
// memory, on Wine from the 32-bit-memory bundle.
const memory64 = hasMemory64();
$('mem64note').hidden = memory64;

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

// Every file under the chosen folder, at any depth: games keep data several
// folders down (Far Cry's shaders are five levels in), and a program that
// misses some of its files fails in ways that look like anything but a
// missing file. Directories are read concurrently.
async function readDir(handle, prefix, entries) {
  const pending = [];
  for await (const [name, h] of handle.entries()) {
    if (h.kind === 'file') pending.push(h.getFile().then((f) => entries.push([prefix + name, f])));
    else pending.push(readDir(h, prefix + name + '/', entries));
  }
  await Promise.all(pending);
}

/** The folder both ways of choosing one end in: [relative path, File] pairs. */
function setFolder(entries) {
  entries.sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
  folder = new Map(entries);
  let bytes = 0;
  let depth = 0;
  for (const [k, f] of entries) {
    bytes += f.size;
    depth = Math.max(depth, k.split('/').length);
  }
  folderSummary = { files: entries.length, bytes, depth };
  $('status').textContent = `Folder: ${entries.length.toLocaleString()} files, ${(bytes / 1048576).toFixed(0)} MB, ${depth} level${depth === 1 ? '' : 's'} deep`;
  setPrograms();
}

$('pick').onclick = async () => {
  if (window.showDirectoryPicker) {
    let dir;
    try {
      dir = await window.showDirectoryPicker();
    } catch {
      return; // cancelled
    }
    const entries = [];
    try {
      await readDir(dir, '', entries);
    } catch (e) {
      $('status').textContent = `Could not read the whole folder: ${e.message}`;
      return;
    }
    setFolder(entries);
  } else {
    $('fallback').click();
  }
};
$('fallback').onchange = (e) => {
  setFolder([...e.target.files].map((f) => [f.webkitRelativePath.split('/').slice(1).join('/'), f]));
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
// Above the screen, since programs rarely show their own: Direct3D's
// presented frames (the render worker's {type: 'perf'}, twice a second),
// else how often the screen changed, and the system calls and threads from
// the latest status sample. The "stats" box (?stats=0) hides it.
let d3dPerf = null; // the latest {type: 'perf'}, with when it came
const perfBox = $('perf');
const showPerf = $('showperf');
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
  perfBox.replaceChildren(...lines.map((l) => Object.assign(document.createElement('span'), { textContent: l })));
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
  const mode = setting('d3dpresent');
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
  if (setting('d3dstats')) port.postMessage({ type: 'stats' });
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
  record = { exe, started: new Date().toISOString(), files: {}, status: [], missingFiles: [], dirListings: [] };
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
  // Paths the program looked for and did not find, and the directory
  // listings it took with how many entries each found, in the order it
  // asked (each sample carries the new ones; see runtime/wine/host.mjs).
  // The record keeps the first 2000 of each (it lives in localStorage), the
  // samples only the counts.
  for (const k of ['missingFiles', 'dirListings']) {
    if (!s[k]?.new) continue;
    if (record) record[k].push(...s[k].new.slice(0, 2000 - record[k].length));
    s = { ...s, [k]: { total: s[k].total, new: s[k].new.length } };
  }
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
  r.page = { url: location.href, settings: settings(), crossOriginIsolated, memory64 };
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
  if (selfTest) r.gpuSelfTest = { ...selfTest, headless: undefined, native: undefined };
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
  r.folderSummary = folderSummary;
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
  /** Runs the GPU self-test (see gpuSelfTest); the results, also kept for the debug report. */
  gpuSelfTest: () => gpuSelfTest(),
  /** The chosen folder: { files, bytes, depth } and every relative path. */
  folder: () => ({ ...folderSummary, paths: [...folder.keys()] }),
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

// ---- GPU self-test ----------------------------------------------------------
//
// Wine's own Direct3D 9 rendering tests (dlls/d3d9/tests/visual.c: about 130
// functions that draw and read the pixels back) on this browser's GPU, built
// and run as tests/web/d3d9-visual.mjs does: ten functions per run, a run
// that takes too long stopped. The failures per function are compared with
// the same tests in headless Chromium on the build machine
// (tests/web/baseline/d3d9_visual.json) and on native Wine's OpenGL backend
// (d3d9_visual_native.json), and kept for the debug report, with the first
// failure lines of each function (they name the colors expected and read).
// ?selftest=1 starts it when the page loads.
const SELFTEST_LIMIT_MS = 300000;
let selfTest = null;
const fetchJson = (path) => fetch(new URL(path, import.meta.url)).then((r) => (r.ok ? r.json() : null), () => null);
function showSelfTest(t, done) {
  const sum = totals(t.results);
  const ref = (base) => (base ? totals(Object.fromEntries(Object.keys(t.results).map((k) => [k, base[k] ?? {}]))).failures : '?');
  $('selftestbox').hidden = false;
  $('selftestsum').textContent =
    `${done ? 'Done' : 'Running'}: ${sum.functions} of ${t.names.length} functions, ${sum.failures} failures, ${sum.broken} crashed or hung` +
    ` (the same functions: ${ref(t.headless)} in headless Chromium, ${ref(t.native)} on native Wine) · ${(t.ms / 1000).toFixed(0)} s` +
    (done ? '. "Debug report" saves them.' : '');
  // The functions that did worse than headless Chromium did.
  const rows = Object.entries(t.results).filter(([k, r]) => r.status !== 'done' || r.failures > (t.headless?.[k]?.failures ?? 0));
  const tbl = $('selftesttable');
  tbl.replaceChildren();
  const tr = (cells, tag = 'td') => {
    const row = document.createElement('tr');
    for (const c of cells) row.append(Object.assign(document.createElement(tag), { textContent: c }));
    tbl.append(row);
    return row;
  };
  if (rows.length) tr(['function (worse than headless Chromium)', 'here', 'headless', 'native', 'first failure'], 'th');
  const cell = (r) => (!r ? '?' : r.status === 'done' ? String(r.failures) : r.status);
  for (const [k, r] of rows) tr([k, cell(r), cell(t.headless?.[k]), cell(t.native?.[k]), r.failed?.[0] ?? '']);
}
async function gpuSelfTest() {
  const dir = '../../target/d3d9-visual/app/';
  const [exe, meta, headless, native] = await Promise.all([
    fetch(new URL(dir + 'd3d9_test.exe', import.meta.url)).then((r) => (r.ok ? r.arrayBuffer() : null), () => null),
    fetchJson(dir + 'd3d9_test.json'),
    fetchJson('../../tests/web/baseline/d3d9_visual.json'),
    fetchJson('../../tests/web/baseline/d3d9_visual_native.json'),
  ]);
  if (!exe || !meta) {
    $('status').textContent = 'The GPU self-test is not on this site (target/d3d9-visual/app: node tests/web/d3d9-visual.mjs --build-only)';
    return;
  }
  $('wine').checked = true;
  writeUrl();
  const names = meta.names;
  const t0 = performance.now();
  selfTest = { created: new Date().toISOString(), settings: settings(), names, headless, native, results: {}, runs: [], ms: 0 };
  const ranges = [];
  for (let a = 0; a < names.length; a += 10) ranges.push([a, Math.min(a + 10, names.length)]);
  for (let k = 0; k < ranges.length; k++) {
    const [a, b] = ranges[k];
    const started = performance.now();
    const timer = setTimeout(stopRun, SELFTEST_LIMIT_MS);
    const m = await run('d3d9_test.exe', exe.slice(0), {}, undefined, {}, ['visual', `${a}-${b}`]);
    clearTimeout(timer);
    const r = parseVisual(out.textContent, a, b, names, !m.stopped);
    Object.assign(selfTest.results, r.results);
    ranges.push(...r.next);
    selfTest.runs.push({ range: [a, b], code: m.code, stopped: !!m.stopped, ms: Math.round(performance.now() - started), log: logEl.textContent.slice(-4000) });
    selfTest.ms = performance.now() - t0;
    showSelfTest(selfTest, false);
    $('status').textContent = `GPU self-test: ${Object.keys(selfTest.results).length} of ${names.length} functions`;
  }
  showSelfTest(selfTest, true);
  $('status').textContent = `GPU self-test done: ${totals(selfTest.results).failures} failures`;
  return selfTest;
}
$('selftest').onclick = () => {
  $('selftest').disabled = true;
  gpuSelfTest().finally(() => ($('selftest').disabled = false));
};

// ---- Running ---------------------------------------------------------------

// The run in progress: stopRun() ends it (the GPU self-test stops a test
// that takes too long).
let stopCurrent = null;
function stopRun() {
  stopCurrent?.();
}

async function run(exeName, exeBytes, files, exePath, times = {}, argv = null) {
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
        stopCurrent = null;
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
    stopCurrent = () => worker.onmessage({ data: { type: 'exit', code: null, stopped: true } });
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
        argv: argv ?? (args ? args.split(/\s+/) : []),
        translatorUrl,
        noCache: setting('nocache'),
        // The program is translated with bounds traps instead of memory
        // checks (wwt translate --mem-traps) where the engine reports where
        // a trap happened (Chrome, Firefox; not Safari). "faithful memory
        // checks" (?memtraps=0) turns them off; ?memtraps=1 forces them.
        memTraps: setting('memtraps') === '0' ? false : setting('memtraps') === '1' ? true : undefined,
        wine,
        bundleUrl,
        bundle64Url,
        bundle64m32Url,
        memory64,
        // ?debug=+d3d: Wine's debug channels (WINEDEBUG), on stderr.
        debug: setting('debug'),
        // ?unixtrace=win,key: the channels of Wine's Unix side (win32u,
        // wineserver, the display driver "browser"; "all" for every one).
        unixTrace: setting('unixtrace'),
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
  writeUrl();
  // A sample with files runs from C:\app, with them beside it.
  const files = {};
  for (const [name, path] of Object.entries(sampleFiles.get(url) ?? {})) {
    files[name] = await (await fetch(new URL(`../../${path}`, import.meta.url))).arrayBuffer();
  }
  const name = url.split('/').pop();
  // The address runs it again (?exe=), for a sample without files.
  if (!sampleFiles.has(url)) {
    const q = new URLSearchParams(location.search);
    q.set('exe', new URL(url).pathname);
    history.replaceState(null, '', `?${q}`);
  }
  run(name, bytes, files, sampleFiles.has(url) ? name : undefined);
};

if (params.get('selftest') === '1') $('selftest').click();

// ?exe=<url> runs a program directly (used by tests and demos).
if (params.get('exe')) {
  const url = params.get('exe');
  const bytes = await (await fetch(url)).arrayBuffer();
  run(url.split('/').pop(), bytes, {});
}
