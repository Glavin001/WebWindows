// Page logic: choose a folder, pick an .exe, run it in a worker. Windowed
// programs (on Wine) draw into a screen shared with the worker, shown in a
// canvas; the canvas's mouse and keyboard events go back through an
// InputRing.

import { InputRing } from '../wine/input-ring.mjs';
import { createAudioRing, playAudioRing } from '../wine/audio-sink.mjs';
import { windowsKey, KEYEVENTF_KEYUP } from './keys.mjs';

const $ = (id) => document.getElementById(id);
const out = $('out');
const logEl = $('log');
let folder = new Map(); // relative path -> File

const params = new URLSearchParams(location.search);
const translatorUrl = params.get('translator') ?? new URL('../../target/wasm32-unknown-unknown/release-wasm/wwt_wasm.wasm', import.meta.url).href;

const bundleUrl = params.get('bundle') ?? new URL('../../target/wine-bundle/', import.meta.url).href;
if (params.get('wine')) $('wine').checked = true;

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

// ---- Running ---------------------------------------------------------------

async function run(exeName, exeBytes, files, exePath) {
  out.textContent = '';
  logEl.textContent = '';
  $('status').textContent = `running ${exeName}…`;
  $('run').disabled = true;
  worker?.terminate();
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
  return new Promise((resolve) => {
    worker.onmessage = (e) => {
      const m = e.data;
      if (m.type === 'stdout' || m.type === 'stderr') out.textContent += dec.decode(m.bytes);
      else if (m.type === 'log') logEl.textContent += m.text + '\n';
      else if (m.type === 'screen') {
        canvas.hidden = false;
        canvas.focus();
        $('status').textContent = `${exeName} is running`;
        window.screenShown = true;
      } else if (m.type === 'exit') {
        $('status').textContent = m.code === null ? `${exeName} stopped with an error` : `${exeName} exited with code ${m.code} (${m.runMs?.toFixed(0)} ms)`;
        $('run').disabled = false;
        worker.terminate();
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
        display: screen && { width: screen.width, height: screen.height, screen: screen.screen, frame: screen.frame, input: screen.input },
        audio,
      },
      [exeBytes],
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
