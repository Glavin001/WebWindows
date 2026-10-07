// The demo page and the browser test runner (?test).
import init, { sceneNames } from './pkg/d3dgpu_web.js';
import * as P from './protocol.mjs';

const log = (s) => (document.getElementById('log').textContent += s + '\n');
const params = new URLSearchParams(location.search);
const test = params.has('test');
// ?features=core: only core WebGPU (no clip-distances, BC, float32-filterable).
const optional = params.get('features') !== 'core';

if (!crossOriginIsolated) log('not cross-origin isolated: serve with runtime/web/serve.mjs');
await init();
const sab = new SharedArrayBuffer(P.SAB_BYTES);
const render = new Worker(new URL('./render-worker.mjs', import.meta.url), { type: 'module' });
const producer = new Worker(new URL('./producer-worker.mjs', import.meta.url), { type: 'module' });
const message = (w, type) => new Promise((resolve) => {
  const h = (e) => { if (e.data.type === type || e.data.type === 'error') { w.removeEventListener('message', h); resolve(e.data); } };
  w.addEventListener('message', h);
});

render.addEventListener('message', (e) => { if (e.data.type === 'error') log('render worker: ' + e.data.message); });
producer.postMessage({ type: 'init', sab });
let canvas = null;
if (!test) {
  canvas = document.getElementById('canvas').transferControlToOffscreen();
  render.postMessage({ type: 'init', sab, canvas, optional }, [canvas]);
} else {
  document.getElementById('demo').hidden = true;
  render.postMessage({ type: 'init', sab, optional });
}
const [ready] = await Promise.all([message(render, 'ready'), message(producer, 'ready')]);
if (ready.type === 'error') throw new Error(ready.message);
log('adapter: ' + ready.adapter);

const names = sceneNames();

if (test) {
  const results = [];
  const table = document.getElementById('results');
  for (const [index, name] of names.entries()) {
    const result = message(render, 'result');
    const readback = message(producer, 'readback');
    producer.postMessage({ type: 'scene', name, index });
    const [r, rb] = await Promise.all([result, readback]);
    const failures = [...r.failures, ...r.messages];
    if (!rb.ok) failures.push('producer readback: ' + rb.detail);
    results.push({ name, failures });
    const row = table.insertRow();
    row.insertCell().textContent = name;
    const c = row.insertCell();
    c.textContent = failures.length ? failures.join('; ') : 'pass';
    c.className = failures.length ? 'fail' : 'pass';
  }
  const failed = results.filter((r) => r.failures.length);
  window.d3dgpuResults = { adapter: ready.adapter, total: results.length, failed };
  log(`${results.length - failed.length}/${results.length} scenes pass`);
} else {
  const select = document.getElementById('scene');
  const perf = [];
  for (const api of ['d3d9', 'd3d11']) for (const n of [500, 2000, 5000]) perf.push(`perf ${api}: ${n} draws`);
  for (const label of [...perf, ...names]) select.add(new Option(label));
  const start = () => {
    producer.postMessage({ type: 'stop' });
    const v = select.value;
    const p = /^perf (d3d9|d3d11): (\d+)/.exec(v);
    if (p) {
      producer.postMessage({ type: 'perf', api: p[1], draws: parseInt(p[2]), width: 640, height: 480 });
    } else {
      producer.postMessage({ type: 'scene', name: v, index: names.indexOf(v), width: 640, height: 480 });
    }
  };
  select.onchange = start;
  start();
  render.addEventListener('message', (e) => {
    if (e.data.type !== 'stats') return;
    const s = e.data.stats;
    document.getElementById('stats').textContent =
      `${e.data.fps.toFixed(1)} batches/s\nexecute ${e.data.executeMs.toFixed(2)} ms per batch\n` +
      Object.entries(s).map(([k, v]) => `${k} ${v}`).join('\n') +
      (e.data.messages.length ? '\n\n' + e.data.messages.slice(-5).join('\n') : '');
  });
}
