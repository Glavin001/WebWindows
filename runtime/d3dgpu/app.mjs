// The d3dgpu page: animated demos with live frame statistics, a benchmark
// suite (?bench), and the correctness test runner (?test). Reports copy as
// Markdown so results can be pasted into an issue or a chat.
import init, { sceneNames, demoList } from './pkg/d3dgpu_web.js';
import * as P from './protocol.mjs';

const $ = (id) => document.getElementById(id);
const log = (s) => ($('log').textContent += s + '\n');
const params = new URLSearchParams(location.search);
const test = params.has('test');
const benchOnLoad = params.has('bench');
// ?features=core: only core WebGPU (no clip-distances, BC, float32-filterable).
const optional = params.get('features') !== 'core';

// ---- Canvas size (fixed when the canvas moves to the render worker) ----
const resParam = params.get('res') ?? '1280x720';
let [width, height] = resParam === 'window'
  ? [Math.round(Math.min(innerWidth - 420, 2560) * devicePixelRatio) & ~1, Math.round(Math.min(innerHeight - 200, 1440) * devicePixelRatio) & ~1]
  : resParam.split('x').map(Number);
if (!(width > 0 && height > 0)) [width, height] = [1280, 720];
$('res').value = resParam;
const canvasEl = $('canvas');
canvasEl.width = width;
canvasEl.height = height;

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
producer.addEventListener('message', (e) => { if (e.data.type === 'error') log('producer: ' + e.data.message); });
producer.postMessage({ type: 'init', sab });
if (!test) {
  const offscreen = canvasEl.transferControlToOffscreen();
  render.postMessage({ type: 'init', sab, canvas: offscreen, optional }, [offscreen]);
} else {
  $('demo-ui').hidden = true;
  $('tests').hidden = false;
  render.postMessage({ type: 'init', sab, optional });
}
const [ready] = await Promise.all([message(render, 'ready'), message(producer, 'ready')]);
if (ready.type === 'error') throw new Error(ready.message);

// ---- Environment ----
// The browser's own adapter description (wgpu doesn't pass it through).
let gpuInfo = {};
try {
  const a = await navigator.gpu?.requestAdapter();
  const i = a?.info ?? (await a?.requestAdapterInfo?.());
  if (i) gpuInfo = { vendor: i.vendor, architecture: i.architecture, device: i.device, description: i.description };
} catch {}
const environment = () => {
  const info = ready.info ?? {};
  const ua = navigator.userAgentData;
  return {
    date: new Date().toISOString(),
    page: location.href,
    userAgent: navigator.userAgent,
    platform: ua ? `${ua.platform} ${ua.mobile ? '(mobile)' : ''}`.trim() : navigator.platform,
    browser: ua ? ua.brands.map((b) => `${b.brand} ${b.version}`).join(', ') : '',
    cores: navigator.hardwareConcurrency,
    memoryGB: navigator.deviceMemory,
    devicePixelRatio,
    screen: `${screen.width}x${screen.height}`,
    canvas: `${width}x${height}`,
    crossOriginIsolated,
    gpu: [gpuInfo.vendor, gpuInfo.architecture, gpuInfo.device, gpuInfo.description].filter(Boolean).join(' / ') || 'unknown',
    adapter: info.name || undefined,
    driver: [info.driver, info.driver_info].filter(Boolean).join(' '),
    deviceType: info.device_type,
    featuresUsed: info.features,
    adapterFeatures: info.adapter_features,
    limits: `maxTexture2D ${info.max_texture_2d}, maxStorageBinding ${info.max_storage_buffer_binding_size}`,
  };
};
const envText = () => Object.entries(environment()).filter(([, v]) => v !== undefined && v !== '' && v !== 0).map(([k, v]) => `${k}: ${v}`).join('\n');

async function copyText(text, button) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    const ta = document.createElement('textarea');
    ta.value = text;
    document.body.append(ta);
    ta.select();
    document.execCommand('copy');
    ta.remove();
  }
  if (button) {
    const old = button.textContent;
    button.textContent = 'Copied ✓';
    setTimeout(() => (button.textContent = old), 1500);
  }
}

const names = sceneNames();

// ======================= Correctness tests (?test) =======================
if (test) {
  const results = [];
  const table = $('results');
  table.innerHTML = '<tr><th>scene</th><th>result</th><th>ms</th></tr>';
  // The static scenes, then a few frames of every demo at a small size.
  const smallParam = (d) => d.param && Math.min(d.param.default, { particles: 8192, instances: 2000 }[d.param.label] ?? 200);
  const jobs = [
    ...names.map((name, index) => ({ name, msg: { type: 'scene', name, index } })),
    ...JSON.parse(demoList()).map((d, index) => ({
      name: 'demo:' + d.name,
      msg: { type: 'demotest', name: d.name, index, width: 160, height: 120, param: smallParam(d) },
    })),
  ];
  for (const { name, msg } of jobs) {
    const t0 = performance.now();
    const result = message(render, 'result');
    const readback = message(producer, 'readback');
    producer.postMessage(msg);
    const [r, rb] = await Promise.all([result, readback]);
    const failures = [...r.failures, ...r.messages];
    if (!rb.ok) failures.push('producer readback: ' + rb.detail);
    results.push({ name, failures, ms: performance.now() - t0 });
    const row = table.insertRow();
    row.insertCell().textContent = name;
    const c = row.insertCell();
    c.textContent = failures.length ? failures.join('; ') : 'pass';
    c.className = failures.length ? 'fail' : 'pass';
    row.insertCell().textContent = (performance.now() - t0).toFixed(0);
    $('test-status').textContent = `${results.length}/${jobs.length}…`;
  }
  const failed = results.filter((r) => r.failures.length);
  window.d3dgpuResults = { adapter: ready.adapter, total: results.length, failed };
  const summary = `${results.length - failed.length}/${results.length} scenes and demos pass`;
  $('test-status').textContent = summary + (optional ? '' : ' (core features only)');
  log('adapter: ' + ready.adapter);
  log(summary);
  $('test-copy').hidden = false;
  $('test-copy').onclick = () => copyText(
    `## d3dgpu correctness tests: ${summary}${optional ? '' : ' (core features only)'}\n\n` +
    '```\n' + envText() + '\n```\n\n' +
    (failed.length ? '| scene | failures |\n| --- | --- |\n' + failed.map((f) => `| ${f.name} | ${f.failures.join('; ').replace(/\|/g, '\\|')} |`).join('\n') : 'No failures.') + '\n',
    $('test-copy'));
} else {
  // ============================ Demo page ============================
  const demos = JSON.parse(demoList());
  const select = $('scene');
  const group = (label, items) => {
    const g = document.createElement('optgroup');
    g.label = label;
    for (const [value, text] of items) g.append(new Option(text, value));
    select.append(g);
  };
  const apiName = (a) => (a === 'D3D11' ? 'Direct3D 11' : 'Direct3D 9');
  group('Demos', demos.filter((d) => !d.name.startsWith('perf')).map((d) => [`demo:${d.name}`, `${d.name} (${apiName(d.api)})`]));
  group('Synthetic perf', demos.filter((d) => d.name.startsWith('perf')).map((d) => [`demo:${d.name}`, `${d.name} (${apiName(d.api)})`]));
  group('Test scenes (Direct3D 9)', names.filter((n) => !n.startsWith('d3d11_')).map((n) => [`scene:${n}`, n]));
  group('Test scenes (Direct3D 11)', names.filter((n) => n.startsWith('d3d11_')).map((n) => [`scene:${n}`, n]));
  $('env').textContent = envText();

  // ---- Parameter control (log-scale slider + number) ----
  const paramInput = $('param');
  const paramRange = $('param-range');
  let spec = null;
  const toSlider = (v) => Math.round((Math.log(v / spec.min) / Math.log(spec.max / spec.min || 2)) * 1000);
  const fromSlider = (s) => Math.round(spec.min * Math.pow(spec.max / spec.min, s / 1000));

  // ---- State and statistics ----
  let current = null; // { kind, name, param }
  let paused = false;
  const frameLog = []; // { t, interval, exec } for the last few seconds
  const gpuHistory = []; // { t, ms }
  let counters = null; // last stats message counters
  const windowStats = { frames: 0, draws: 0, passCommands: 0, skipped: 0, created: 0, bytes: 0, errors: 0, skippedDraws: 0, drawNs: 0, prepareNs: 0, recordNs: 0, submitNs: 0 };
  let windowStart = performance.now();
  let lastMessages = [];

  const percentile = (arr, p) => {
    if (!arr.length) return NaN;
    const s = [...arr].sort((a, b) => a - b);
    return s[Math.min(s.length - 1, Math.floor((p / 100) * s.length))];
  };
  const avg = (arr) => (arr.length ? arr.reduce((a, b) => a + b, 0) / arr.length : NaN);
  const fmt = (v, d = 1) => (Number.isFinite(v) ? v.toFixed(d) : '–');

  function resetStats() {
    frameLog.length = 0;
    gpuHistory.length = 0;
    counters = null;
    Object.keys(windowStats).forEach((k) => (windowStats[k] = 0));
    windowStart = performance.now();
    lastMessages = [];
    render.postMessage({ type: 'restart' });
  }

  function start() {
    producer.postMessage({ type: 'stop' });
    const [kind, name] = select.value.split(':');
    const d = demos.find((x) => x.name === name);
    spec = kind === 'demo' ? d?.param : null;
    $('param-label').hidden = !spec;
    let param = null;
    if (spec) {
      $('param-name').textContent = spec.label;
      paramInput.min = spec.min;
      paramInput.max = spec.max;
      param = Math.min(spec.max, Math.max(spec.min, parseInt(paramInput.value) || spec.default));
      paramInput.value = param;
      paramRange.value = toSlider(param);
    }
    $('about').textContent = kind === 'demo' ? `${apiName(d.api)} — ${d.about}` : 'Correctness test scene (static; see ?test for the checks).';
    current = { kind, name, param };
    resetStats();
    paused = false;
    $('pause').textContent = 'Pause';
    if (kind === 'demo') {
      producer.postMessage({ type: 'demo', name, param, width, height });
    } else {
      producer.postMessage({ type: 'scene', name, index: names.indexOf(name), width, height });
    }
    const q = new URLSearchParams(location.search);
    q.set('scene', select.value);
    if (param !== null) q.set('param', param); else q.delete('param');
    window.history.replaceState(null, '', '?' + q);
  }

  render.addEventListener('message', (e) => {
    if (e.data.type !== 'stats') return;
    const now = performance.now();
    let t = now;
    for (let i = e.data.samples.length - 1; i >= 0; i--) {
      const [interval, exec] = e.data.samples[i];
      frameLog.unshift({ t, interval, exec });
      t -= interval;
    }
    frameLog.sort((a, b) => a.t - b.t);
    for (const ms of e.data.gpu) gpuHistory.push({ t: now, ms });
    while (frameLog.length && frameLog[0].t < now - 5000) frameLog.shift();
    while (gpuHistory.length && gpuHistory[0].t < now - 5000) gpuHistory.shift();
    const s = e.data.stats;
    if (counters) {
      windowStats.frames += e.data.samples.length;
      windowStats.draws += s.draws - counters.draws;
      windowStats.passCommands += s.pass_commands - counters.pass_commands;
      windowStats.skipped += s.pass_commands_skipped - counters.pass_commands_skipped;
      windowStats.created += s.pipelines + s.bind_groups + s.buffers + s.textures + s.samplers -
        (counters.pipelines + counters.bind_groups + counters.buffers + counters.textures + counters.samplers);
      windowStats.bytes += s.bytes_uploaded - counters.bytes_uploaded;
      windowStats.errors += s.errors - counters.errors;
      windowStats.skippedDraws += s.skipped_draws - counters.skipped_draws;
      windowStats.drawNs += s.draw_ns - counters.draw_ns;
      windowStats.prepareNs += s.prepare_ns - counters.prepare_ns;
      windowStats.recordNs += s.record_ns - counters.record_ns;
      windowStats.submitNs += s.submit_ns - counters.submit_ns;
    }
    counters = s;
    if (e.data.messages.length) lastMessages = [...lastMessages, ...e.data.messages].slice(-20);
    if (benchCollector) benchCollector(e.data);
  });

  // The numbers shown and reported: the last two seconds.
  function summary(spanMs = 2000) {
    const now = performance.now();
    const recent = frameLog.filter((h) => h.t >= now - spanMs && h.interval > 0);
    const intervals = recent.map((h) => h.interval);
    const execs = recent.map((h) => h.exec);
    const gpu = gpuHistory.filter((g) => g.t >= now - spanMs).map((g) => g.ms);
    const f = Math.max(1, windowStats.frames);
    return {
      fps: intervals.length ? 1000 / avg(intervals) : NaN,
      frameAvg: avg(intervals),
      p50: percentile(intervals, 50),
      p95: percentile(intervals, 95),
      p99: percentile(intervals, 99),
      max: intervals.length ? Math.max(...intervals) : NaN,
      cpu: avg(execs),
      cpuP95: percentile(execs, 95),
      gpu: avg(gpu),
      gpuP95: percentile(gpu, 95),
      drawsPerFrame: windowStats.draws / f,
      passCmdsPerDraw: windowStats.passCommands / Math.max(1, windowStats.draws),
      skippedPerDraw: windowStats.skipped / Math.max(1, windowStats.draws),
      createdPerSec: (windowStats.created * 1000) / Math.max(1, now - windowStart),
      uploadKBPerFrame: windowStats.bytes / 1024 / f,
      drawsMs: windowStats.drawNs / 1e6 / f,
      prepareMs: windowStats.prepareNs / 1e6 / f,
      recordMs: windowStats.recordNs / 1e6 / f,
      submitMs: windowStats.submitNs / 1e6 / f,
      errors: windowStats.errors,
      skippedDraws: windowStats.skippedDraws,
      frames: windowStats.frames,
    };
  }

  // ---- Display ----
  const graph = $('graph');
  const g = graph.getContext('2d');
  function drawGraph() {
    const W = graph.width, H = graph.height;
    g.clearRect(0, 0, W, H);
    const maxMs = 50;
    const y = (ms) => H - (Math.min(ms, maxMs) / maxMs) * H;
    g.strokeStyle = '#4cc38a55';
    g.lineWidth = 1;
    for (const ms of [16.7, 33.3]) {
      g.beginPath(); g.moveTo(0, y(ms)); g.lineTo(W, y(ms)); g.stroke();
    }
    const pts = frameLog.slice(-240);
    const n = pts.length;
    const line = (key, color) => {
      g.strokeStyle = color; g.lineWidth = 2; g.beginPath();
      pts.forEach((p, i) => { const x = W - (n - 1 - i) * (W / 240); i ? g.lineTo(x, y(p[key])) : g.moveTo(x, y(p[key])); });
      g.stroke();
    };
    line('exec', '#e0b341');
    line('interval', '#e6e8ee');
    const gp = gpuHistory.slice(-240);
    g.fillStyle = '#7aa2ff';
    gp.forEach((p, i) => g.fillRect(W - (gp.length - 1 - i) * (W / 240) - 1, y(p.ms) - 1, 3, 3));
    g.fillStyle = '#9aa1b1'; g.font = '20px system-ui';
    g.fillText('50 ms', 4, 20);
  }

  function updatePanel() {
    const s = summary();
    $('fps').textContent = paused ? 'paused' : fmt(s.fps, 1);
    const rows = [
      ['frame time', `${fmt(s.frameAvg, 2)} ms avg`],
      ['  p50 / p95 / p99', `${fmt(s.p50)} / ${fmt(s.p95)} / ${fmt(s.p99)} ms`],
      ['  worst', `${fmt(s.max)} ms`],
      ['CPU: core execute', `${fmt(s.cpu, 2)} ms (p95 ${fmt(s.cpuP95, 2)})`],
      ['  in draws', `${fmt(s.drawsMs, 2)} ms (derive ${fmt(s.prepareMs, 2)}, record ${fmt(s.recordMs, 2)})`],
      ['  finish + submit', `${fmt(s.submitMs, 2)} ms`],
      ['GPU: submit → done', `${fmt(s.gpu, 2)} ms (p95 ${fmt(s.gpuP95, 2)})`],
      ['draws / frame', fmt(s.drawsPerFrame, 0)],
      ['CPU µs / draw', fmt((s.cpu * 1000) / Math.max(1, s.drawsPerFrame), 2)],
      ['pass cmds / draw', `${fmt(s.passCmdsPerDraw, 2)} (+${fmt(s.skippedPerDraw, 2)} skipped)`],
      ['uploads / frame', `${fmt(s.uploadKBPerFrame, 1)} KiB`],
      ['objects created / s', fmt(s.createdPerSec, 1)],
      ['errors / skipped draws', `${s.errors} / ${s.skippedDraws}`],
      ['canvas', `${width}x${height}${$('pace').checked ? ', vsync' : ', uncapped'}`],
    ];
    $('kv').innerHTML = rows.map(([k, v]) => `<tr><td>${k}</td><td>${v}</td></tr>`).join('');
    $('messages-panel').hidden = !lastMessages.length;
    $('messages').textContent = lastMessages.join('\n');
    drawGraph();
  }
  setInterval(updatePanel, 250);

  // ---- Report ----
  function currentReport() {
    const s = summary();
    const label = current ? `${current.name}${current.param !== null ? ` (${spec?.label} ${current.param})` : ''}` : '';
    return `## d3dgpu: ${label}\n\n` +
      `FPS ${fmt(s.fps)} · frame ${fmt(s.frameAvg, 2)} ms (p50 ${fmt(s.p50)}, p95 ${fmt(s.p95)}, p99 ${fmt(s.p99)}, worst ${fmt(s.max)}) · ` +
      `CPU ${fmt(s.cpu, 2)} ms (draws ${fmt(s.drawsMs, 2)}: derive ${fmt(s.prepareMs, 2)}, record ${fmt(s.recordMs, 2)}; submit ${fmt(s.submitMs, 2)}) · GPU submit→done ${fmt(s.gpu, 2)} ms · ${fmt(s.drawsPerFrame, 0)} draws/frame · ` +
      `${fmt((s.cpu * 1000) / Math.max(1, s.drawsPerFrame), 2)} µs/draw · ${fmt(s.passCmdsPerDraw, 2)} pass cmds/draw · errors ${s.errors}, skipped draws ${s.skippedDraws}` +
      `${$('pace').checked ? ' · vsync' : ' · uncapped'}\n\n` +
      '```\n' + envText() + '\n```\n' +
      (lastMessages.length ? '\nMessages:\n```\n' + lastMessages.join('\n') + '\n```\n' : '');
  }
  $('copy').onclick = () => copyText(currentReport(), $('copy'));

  // ---- Controls ----
  select.onchange = () => { paramInput.value = ''; start(); };
  paramInput.onchange = () => start();
  paramRange.oninput = () => { paramInput.value = fromSlider(+paramRange.value); };
  paramRange.onchange = () => start();
  $('res').onchange = () => {
    const q = new URLSearchParams(location.search);
    q.set('res', $('res').value);
    location.search = q;
  };
  $('pace').onchange = () => {
    render.postMessage({ type: 'pace', on: $('pace').checked });
    resetStats();
  };
  $('pause').onclick = () => {
    if (!current || current.kind !== 'demo') return;
    paused = !paused;
    $('pause').textContent = paused ? 'Resume' : 'Pause';
    if (paused) producer.postMessage({ type: 'stop' });
    else start();
  };

  // ---- Benchmark ----
  const SUITE = [
    ['perf9', 500], ['perf9', 2000], ['perf9', 5000],
    ['perf11', 500], ['perf11', 2000], ['perf11', 5000],
    ['cubes9', 1000], ['cubes9', 5000],
    ['cubes11', 1000], ['cubes11', 5000],
    ['instanced11', 100000], ['instanced11', 500000],
    ['particles11', 262144], ['particles11', 1048576],
    ['bloom11', 64], ['shadows11', 100], ['sprites9', 10000],
  ];
  let benchCollector = null;
  let benchResults = null;
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

  async function runBenchmark() {
    $('bench').disabled = true;
    $('bench-panel').hidden = false;
    $('bench-copy').hidden = $('bench-json').hidden = true;
    const wasPaced = $('pace').checked;
    $('pace').checked = false;
    render.postMessage({ type: 'pace', on: false });
    const table = $('bench-table');
    table.innerHTML = '<tr><th>scene</th><th>size</th><th>FPS</th><th>frame p50</th><th>p95</th><th>p99</th><th>CPU ms</th><th>µs/draw</th><th>GPU ms</th><th>draws</th><th>cmds/draw</th><th>errors</th></tr>';
    benchResults = { environment: environment(), results: [] };
    for (const [i, [name, param]] of SUITE.entries()) {
      $('bench-status').textContent = `${i + 1}/${SUITE.length}: ${name} ${param}…`;
      select.value = `demo:${name}`;
      paramInput.value = param;
      start();
      await sleep(1500); // warm-up: pipelines, first uploads
      resetStats();
      await sleep(4000);
      const s = summary(4000);
      const r = { scene: name, param, ...Object.fromEntries(Object.entries(s).map(([k, v]) => [k, Number.isFinite(v) ? +v.toFixed(3) : v])) };
      benchResults.results.push(r);
      const row = table.insertRow();
      const usPerDraw = (s.cpu * 1000) / Math.max(1, s.drawsPerFrame);
      for (const v of [name, param, fmt(s.fps), fmt(s.p50), fmt(s.p95), fmt(s.p99), fmt(s.cpu, 2), fmt(usPerDraw, 2), fmt(s.gpu, 2), fmt(s.drawsPerFrame, 0), fmt(s.passCmdsPerDraw, 2), s.errors + s.skippedDraws]) {
        row.insertCell().textContent = v;
      }
    }
    $('bench-status').textContent = `done (${width}x${height}, uncapped)`;
    $('bench-copy').hidden = $('bench-json').hidden = false;
    $('bench').disabled = false;
    $('pace').checked = wasPaced;
    render.postMessage({ type: 'pace', on: wasPaced });
    window.d3dgpuBench = benchResults;
  }
  function benchMarkdown() {
    const rows = benchResults.results.map((r) =>
      `| ${r.scene} | ${r.param} | ${fmt(r.fps)} | ${fmt(r.p50)} / ${fmt(r.p95)} / ${fmt(r.p99)} | ${fmt(r.cpu, 2)} | ${fmt((r.cpu * 1000) / Math.max(1, r.drawsPerFrame), 2)} | ${fmt(r.gpu, 2)} | ${fmt(r.drawsPerFrame, 0)} | ${fmt(r.passCmdsPerDraw, 2)} | ${r.errors + r.skippedDraws} |`);
    return `## d3dgpu benchmark (${width}x${height}, uncapped)\n\n` +
      '| scene | size | FPS | frame ms p50 / p95 / p99 | CPU ms | µs/draw | GPU ms | draws/frame | pass cmds/draw | errors |\n' +
      '| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n' + rows.join('\n') +
      '\n\n```\n' + envText() + '\n```\n';
  }
  $('bench').onclick = runBenchmark;
  $('bench-copy').onclick = () => copyText(benchMarkdown(), $('bench-copy'));
  $('bench-json').onclick = () => {
    const a = document.createElement('a');
    a.href = URL.createObjectURL(new Blob([JSON.stringify(benchResults, null, 2)], { type: 'application/json' }));
    a.download = `d3dgpu-bench-${Date.now()}.json`;
    a.click();
  };

  // ---- Start ----
  const initial = params.get('scene');
  select.value = [...select.options].some((o) => o.value === initial) ? initial : 'demo:cubes11';
  if (params.get('param')) paramInput.value = params.get('param');
  start();
  if (benchOnLoad) runBenchmark();
}
