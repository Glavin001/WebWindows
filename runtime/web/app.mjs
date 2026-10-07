// Page logic: choose a folder, pick an .exe, run it in a worker.

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

function run(exeName, exeBytes, files) {
  out.textContent = '';
  logEl.textContent = '';
  $('status').textContent = `running ${exeName}…`;
  $('run').disabled = true;
  const worker = new Worker(new URL('worker.mjs', import.meta.url), { type: 'module' });
  const dec = new TextDecoder('latin1');
  return new Promise((resolve) => {
    worker.onmessage = (e) => {
      const m = e.data;
      if (m.type === 'stdout' || m.type === 'stderr') out.textContent += dec.decode(m.bytes);
      else if (m.type === 'log') logEl.textContent += m.text + '\n';
      else if (m.type === 'exit') {
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
        exeBytes,
        files,
        argv: args ? args.split(/\s+/) : [],
        translatorUrl,
        noCache: $('nocache').checked,
        wine: $('wine').checked,
        bundleUrl,
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
  run(name.split('/').pop(), exe, files);
};

// ?exe=<url> runs a program directly (used by tests and demos).
if (params.get('exe')) {
  const url = params.get('exe');
  const bytes = await (await fetch(url)).arrayBuffer();
  if (params.get('nocache')) $('nocache').checked = true;
  run(url.split('/').pop(), bytes, {});
}
