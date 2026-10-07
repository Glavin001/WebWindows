// Page logic: choose a folder, pick an .exe, run it in a worker. Windowed
// programs (on Wine) draw into a screen shared with the worker, shown in a
// canvas; the canvas's mouse and keyboard events go back through an
// InputRing.

import { InputRing } from '../wine/input-ring.mjs';
import { windowsKey, KEYEVENTF_KEYUP } from './keys.mjs';

const $ = (id) => document.getElementById(id);
const out = $('out');
const logEl = $('log');
let folder = new Map(); // relative path -> File

const params = new URLSearchParams(location.search);
const translatorUrl = params.get('translator') ?? new URL('../../target/wasm32-unknown-unknown/release-wasm/wwt_wasm.wasm', import.meta.url).href;

const bundleUrl = params.get('bundle') ?? new URL('../../target/wine-bundle/', import.meta.url).href;
if (params.get('wine')) $('wine').checked = true;
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

const at = (e) => {
  const r = canvas.getBoundingClientRect();
  return [Math.floor(((e.clientX - r.left) * canvas.width) / r.width), Math.floor(((e.clientY - r.top) * canvas.height) / r.height)];
};
// MOUSEEVENTF_* for buttons 0 (left), 1 (middle), 2 (right): [down, up].
const BUTTONS = [[0x2, 0x4], [0x20, 0x40], [0x8, 0x10]];
canvas.addEventListener('pointermove', (e) => screen?.ring.mouse(...at(e)));
canvas.addEventListener('pointerdown', (e) => {
  canvas.focus();
  canvas.setPointerCapture(e.pointerId);
  if (BUTTONS[e.button]) screen?.ring.mouse(...at(e), BUTTONS[e.button][0]);
  e.preventDefault();
});
canvas.addEventListener('pointerup', (e) => {
  if (BUTTONS[e.button]) screen?.ring.mouse(...at(e), BUTTONS[e.button][1]);
});
canvas.addEventListener('contextmenu', (e) => e.preventDefault());
canvas.addEventListener(
  'wheel',
  (e) => {
    screen?.ring.mouse(...at(e), 0x800, e.deltaY < 0 ? 120 : -120);
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
    if (e.data.type === 'log') logEl.textContent += `d3d: ${e.data.text}\n`;
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

// ---- Running ---------------------------------------------------------------

function run(exeName, exeBytes, files, exePath) {
  out.textContent = '';
  logEl.textContent = '';
  $('status').textContent = `running ${exeName}…`;
  $('run').disabled = true;
  worker?.terminate();
  worker = new Worker(new URL('worker.mjs', import.meta.url), { type: 'module' });
  const dec = new TextDecoder('latin1');
  const wine = $('wine').checked;
  screen = wine ? newScreen() : null;
  shown = -1;
  canvas.hidden = true;
  const d3dOffscreen = screen ? newD3DCanvas() : null;
  return new Promise((resolve) => {
    worker.onmessage = (e) => {
      const m = e.data;
      if (m.type === 'stdout' || m.type === 'stderr') out.textContent += dec.decode(m.bytes);
      else if (m.type === 'log') logEl.textContent += m.text + '\n';
      else if (m.type === 'd3d-window') placeD3D(m);
      else if (m.type === 'screen') {
        canvas.hidden = false;
        canvas.focus();
        $('status').textContent = `${exeName} is running`;
        window.screenShown = true;
      } else if (m.type === 'exit') {
        $('status').textContent = m.code === null ? `${exeName} stopped with an error` : `${exeName} exited with code ${m.code} (${m.runMs?.toFixed(0)} ms)`;
        $('run').disabled = false;
        worker.terminate();
        if (d3dCanvas) d3dCanvas.hidden = true;
        window.lastExit = m;
        resolve(m);
      }
    };
    const args = $('args').value.trim();
    worker.postMessage(
      {
        exeName,
        exePath,
        exeBytes,
        files,
        argv: args ? args.split(/\s+/) : [],
        translatorUrl,
        noCache: $('nocache').checked,
        wine,
        bundleUrl,
        // ?debug=+d3d: Wine's debug channels (WINEDEBUG), on stderr.
        debug: params.get('debug') ?? '',
        display: screen && { width: screen.width, height: screen.height, screen: screen.screen, frame: screen.frame, input: screen.input },
        d3dCanvas: d3dOffscreen?.canvas,
        d3dOffscreen: d3dOffscreen?.offscreen,
        d3dPort: d3dOffscreen?.port,
      },
      [exeBytes, d3dOffscreen?.canvas, d3dOffscreen?.port].filter(Boolean),
    );
  });
}

$('run').onclick = async () => {
  const name = $('exe').value;
  const files = {};
  for (const [k, f] of folder) if (f.size < 64 << 20) files[k] = await f.arrayBuffer();
  const exe = await folder.get(name).arrayBuffer();
  run(name.split('/').pop(), exe, files, name);
};

// Wine's own programs from the bundle (when it carries Wine's Unix side).
fetch(new URL('manifest.json', bundleUrl))
  .then((r) => (r.ok ? r.json() : null))
  .then((manifest) => {
    if (!manifest?.programs?.length) return;
    for (const p of manifest.programs) $('sample').add(new Option(p.split('/').pop(), p));
    // Direct3D 9 test programs (tests/programs/gui); the benchmark takes
    // "cubes particles seconds" in the arguments box.
    for (const [name, label] of [['d3d9bench.exe', 'd3d9bench.exe (Direct3D 9 benchmark)'], ['d3d9tri.exe', 'd3d9tri.exe (Direct3D 9)']])
      $('sample').add(new Option(label, new URL(`../../tests/programs/gui/${name}`, import.meta.url).href));
    $('samples').hidden = false;
  })
  .catch(() => {});
$('runsample').onclick = async () => {
  const rel = $('sample').value;
  const bytes = await (await fetch(new URL(rel, bundleUrl))).arrayBuffer();
  $('wine').checked = true;
  run(rel.split('/').pop(), bytes, {});
};

// ?exe=<url> runs a program directly (used by tests and demos).
if (params.get('exe')) {
  const url = params.get('exe');
  const bytes = await (await fetch(url)).arrayBuffer();
  if (params.get('nocache')) $('nocache').checked = true;
  run(url.split('/').pop(), bytes, {});
}
