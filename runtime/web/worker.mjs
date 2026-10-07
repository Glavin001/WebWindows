// Runs one Windows program in a Web Worker: translates it (or loads the
// cached translation), then executes it over a shared memory. Messages to
// the page: {type: 'log' | 'stdout' | 'stderr' | 'exit', ...}.

import { Machine, ProcessExit, GuestFault } from '../runtime.mjs';
import { Process } from '../win32.mjs';
import { FastTranslator, enableFastMode } from '../fastmode.mjs';

const log = (text) => postMessage({ type: 'log', text });

async function sha256(bytes) {
  const d = await crypto.subtle.digest('SHA-256', bytes);
  return [...new Uint8Array(d)].map((b) => b.toString(16).padStart(2, '0')).join('');
}

// ---- Cache in the origin private file system --------------------------------

async function cacheDir() {
  try {
    const root = await navigator.storage.getDirectory();
    return await root.getDirectoryHandle('wwt-cache', { create: true });
  } catch {
    return null;
  }
}

async function cacheRead(dir, name) {
  if (!dir) return null;
  try {
    const f = await (await dir.getFileHandle(name)).getFile();
    return new Uint8Array(await f.arrayBuffer());
  } catch {
    return null;
  }
}

async function cacheWrite(dir, name, bytes) {
  if (!dir) return;
  const h = await dir.getFileHandle(name, { create: true });
  const w = await h.createWritable();
  await w.write(bytes);
  await w.close();
}

onmessage = async (e) => {
  const { exeName, exeBytes, files = {}, argv = [], translatorUrl, guestLimitMB = 512, noCache } = e.data;
  const enc = new TextEncoder();
  try {
    const t0 = performance.now();
    const ft = await FastTranslator.load(await (await fetch(translatorUrl)).arrayBuffer());
    const abi = ft.abi();
    const exe = new Uint8Array(exeBytes);
    const key = `${await sha256(exe)}-abi${abi.version}`;
    const dir = noCache ? null : await cacheDir();
    // The profile lists code found at run time on earlier launches.
    const profileBytes = await cacheRead(dir, `${key}.profile`);
    const profile = profileBytes ? [...new Uint32Array(profileBytes.buffer)] : [];
    let wasm = await cacheRead(dir, `${key}.wasm`);
    let translated = false;
    if (!wasm) {
      const t = performance.now();
      wasm = ft.translatePe(exe, { profile });
      if (!wasm.length) throw new Error('translation failed');
      translated = true;
      log(`translated ${exeName} in ${(performance.now() - t).toFixed(0)} ms (${(wasm.length / 1024).toFixed(0)} KB${profile.length ? `, ${profile.length} profiled entries` : ''})`);
      await cacheWrite(dir, `${key}.wasm`, wasm);
    } else {
      log(`loaded cached translation of ${exeName} (${(wasm.length / 1024).toFixed(0)} KB)`);
    }

    const machine = new Machine({ abi, kernel: ft.kernel(), guestLimit: guestLimitMB << 20, log });
    await machine.init();
    enableFastMode(machine, ft, { log });
    const mod = await machine.loadModule(wasm, exeName);
    const fileMap = new Map(Object.entries(files).map(([k, v]) => [k.toLowerCase(), new Uint8Array(v)]));
    const proc = new Process(machine, {
      argv: [exeName, ...argv],
      stdout: (b) => postMessage({ type: 'stdout', bytes: b }),
      stderr: (b) => postMessage({ type: 'stderr', bytes: b }),
      files: fileMap,
    });
    proc.load(exe, mod.meta.image);
    log(`ready in ${(performance.now() - t0).toFixed(0)} ms; running`);
    let exitCode;
    let error = null;
    const tr = performance.now();
    try {
      exitCode = proc.start();
    } catch (err) {
      if (err instanceof ProcessExit) exitCode = err.exitCode;
      else error = err;
    }
    const runMs = performance.now() - tr;
    // Fold code found at run time into the next ahead-of-time pass.
    if (machine.profile.size) {
      const all = new Set([...profile, ...machine.profile]);
      await cacheWrite(dir, `${key}.profile`, new Uint8Array(Uint32Array.from(all).buffer));
      if (dir && all.size > profile.length) {
        try {
          await dir.removeEntry(`${key}.wasm`);
        } catch {}
        log(`${machine.profile.size} addresses translated at run time; the next launch includes them`);
      }
    }
    if (error) {
      postMessage({ type: 'stderr', bytes: enc.encode(`\n*** ${error instanceof GuestFault ? 'guest fault' : 'error'}: ${error.message}\n`) });
    }
    postMessage({ type: 'exit', code: error ? null : exitCode, translated, runMs });
  } catch (err) {
    postMessage({ type: 'stderr', bytes: enc.encode(`\n*** ${err.stack ?? err}\n`) });
    postMessage({ type: 'exit', code: null });
  }
};
