// Runs one Windows program in a Web Worker: translates it (or loads the
// cached translation), then executes it over a shared memory. Messages to
// the page: {type: 'log' | 'stdout' | 'stderr' | 'screen' | 'exit', ...}.
//
// Windowed programs on Wine draw into the screen the page shares with the
// worker (`display`: its pixels, a frame counter the page polls, and an
// InputRing of keyboard and mouse events); 'screen' tells the page the
// first frame is there.

import { Machine, ProcessExit, GuestFault, hasMemory64 } from '../runtime.mjs';
import { Process } from '../win32.mjs';
import { peArch } from '../pe.mjs';
import { FastTranslator, enableFastMode } from '../fastmode.mjs';
import { WineHost } from '../wine/host.mjs';
import { loadWineUnix } from '../wine/unix.mjs';
import { Display } from '../wine/display.mjs';
import { InputRing } from '../wine/input-ring.mjs';

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
    // An interrupted write leaves an empty file: treat it as missing.
    return f.size ? new Uint8Array(await f.arrayBuffer()) : null;
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

/**
 * Runs the program on translated Wine (Milestone 2): Wine's DLLs come
 * pre-translated in the bundle; the .exe is translated here and cached.
 */
async function runOnWine({ exeName, exePath, exe, folder, argv, ft, abi, dir, key, bundleUrl, display: shared }) {
  const base = new URL(bundleUrl, self.location.href);
  const manifest = await (await fetch(new URL('manifest.json', base))).json();
  const t0 = performance.now();
  const bytesOf = async (rel) => new Uint8Array(await (await fetch(new URL(rel, base))).arrayBuffer());
  // Wine's Unix side (windowed programs): its layout decides the machine's.
  const unixFiles = manifest.unix && shared
    ? await Promise.all([
        fetch(new URL(manifest.unix.layout, base)).then((r) => r.json()),
        fetch(new URL(manifest.unix.syscalls, base)).then((r) => r.json()),
        Promise.all(Object.entries(manifest.unix.data).map(async ([path, rel]) => [path, await bytesOf(rel)])),
      ])
    : null;
  const files = new Map();
  const compiled = new Map();
  const sys32 = 'c:\\windows\\system32';
  await Promise.all([
    ...Object.entries(manifest.dlls).map(async ([name, f]) => {
      const [pe, mod] = await Promise.all([
        fetch(new URL(f.pe, base)).then((r) => r.arrayBuffer()),
        // Streaming compilation: browsers cache the compiled code by URL.
        WebAssembly.compileStreaming(fetch(new URL(f.wasm, base))),
      ]);
      files.set(`${sys32}\\${name}`, new Uint8Array(pe));
      compiled.set(`${sys32}\\${name}`, mod);
    }),
    ...manifest.nls.map(async (n) => {
      files.set(`${sys32}\\${n}`, new Uint8Array(await (await fetch(new URL(n, base))).arrayBuffer()));
    }),
  ]);
  files.set('c:\\windows\\globalization\\sorting\\sortdefault.nls', files.get(`${sys32}\\sortdefault.nls`));
  log(`loaded Wine ${manifest.wine} (${compiled.size} DLLs) in ${(performance.now() - t0).toFixed(0)} ms`);
  // The chosen folder is C:\app; a program given by URL runs from C:\.
  for (const [rel, bytes] of Object.entries(folder)) {
    files.set(`c:\\app\\${rel.replaceAll('/', '\\').toLowerCase()}`, new Uint8Array(bytes));
  }
  const exeWin = exePath ? `C:\\app\\${exePath.replaceAll('/', '\\')}` : `C:\\${exeName}`;
  const exeDos = exeWin.toLowerCase();
  files.set(exeDos, exe);
  const translateTimed = (path, bytes) => {
    const t = performance.now();
    const w = ft.translatePe(bytes);
    log(`translated ${path} in ${(performance.now() - t).toFixed(0)} ms`);
    return w;
  };
  // The .exe is translated before boot so its cache write completes before
  // the program runs (the worker may be terminated as soon as it exits).
  let exeWasm = await cacheRead(dir, `${key}.wine.wasm`);
  if (exeWasm) {
    log(`loaded cached translation of ${exeDos}`);
  } else {
    exeWasm = translateTimed(exeDos, exe);
    await cacheWrite(dir, `${key}.wine.wasm`, exeWasm);
  }
  // By file, too: an assembly's DLL is also installed in C:\windows\winsxs.
  const compiledFor = new Map([...compiled].map(([path, mod]) => [files.get(path), mod]));
  const translate = (path, bytes, { rebased }) => {
    // The bundle's modules (and the cached .exe) were translated at the
    // image's own base.
    if (!rebased && compiledFor.has(bytes)) return compiledFor.get(bytes);
    if (!rebased && path === exeDos) return exeWasm;
    return translateTimed(path, bytes);
  };
  const [layout, win32uNames, dataFiles] = unixFiles ?? [];
  const machine = new Machine({
    abi,
    kernel: ft.kernel(),
    guestLimit: 0x8000_0000,
    ...(layout && { nativeSize: layout.nativeSize, extraSize: layout.extraSize }),
    log,
  });
  await machine.init();
  enableFastMode(machine, ft, { log });
  let unix = null;
  if (layout) {
    const ring = new InputRing(shared.input);
    const frame = new Int32Array(shared.frame);
    const display = new Display({
      width: shared.width,
      height: shared.height,
      buffer: shared.screen,
      onChange: () => {
        if (Atomics.add(frame, 0, 1) === 0) postMessage({ type: 'screen' });
      },
      inputSource: (d) => ring.drain(d),
    });
    unix = await loadWineUnix(machine, {
      factory: async () => (await import(new URL(manifest.unix.module, base).href)).default,
      layout,
      dataFiles: new Map(dataFiles),
      display,
      // The program waits: sleep until its timeout or the page's next event.
      wait: (ms) => {
        if (display.hasInput()) return 1;
        ring.wait(ms);
        return display.hasInput() ? 1 : 0;
      },
      stderr: (s) => postMessage({ type: 'stderr', bytes: new TextEncoder().encode(s) }),
      win32uNames,
    });
    log(`loaded Wine's Unix side (wineserver, win32u) in ${(performance.now() - t0).toFixed(0)} ms`);
  }
  const host = new WineHost(machine, {
    translate,
    files,
    argv: [exeWin, ...argv],
    exePath: exeWin,
    stdout: (b) => postMessage({ type: 'stdout', bytes: b }),
    stderr: (b) => postMessage({ type: 'stderr', bytes: b }),
    unix,
  });
  host.boot(`${sys32}\\ntdll.dll`, exeDos);
  log(`Wine process ready in ${(performance.now() - t0).toFixed(0)} ms; running`);
  const tr = performance.now();
  const r = host.run();
  return { ...r, runMs: performance.now() - tr };
}

onmessage = async (e) => {
  const { exeName, exePath, exeBytes, files = {}, argv = [], translatorUrl, guestLimitMB = 512, noCache, wine, bundleUrl, display } = e.data;
  const enc = new TextEncoder();
  try {
    const t0 = performance.now();
    const ft = await FastTranslator.load(await (await fetch(translatorUrl)).arrayBuffer());
    const abi = ft.abi();
    const exe = new Uint8Array(exeBytes);
    const key = `${await sha256(exe)}-abi${abi.version}`;
    const dir = noCache ? null : await cacheDir();
    if (wine) {
      const r = await runOnWine({ exeName, exePath, exe, folder: files, argv, ft, abi, dir, key, bundleUrl, display });
      if (r.error) postMessage({ type: 'stderr', bytes: enc.encode(`\n*** ${r.error.message}\n`) });
      postMessage({ type: 'exit', code: r.error ? null : r.exitCode, translated: false, runMs: r.runMs, wine: true });
      return;
    }
    // 64-bit programs run on a 64-bit (memory64) memory where the browser
    // has one (Chrome 133, Firefox 134), otherwise below 4 GB in a 32-bit
    // memory.
    const arch = peArch(exe);
    const mem64 = arch === 'x64' && hasMemory64();
    if (arch === 'x64') log(mem64 ? '64-bit program: 64-bit WebAssembly memory' : '64-bit program: this browser has no 64-bit WebAssembly memory; running it below 4 GB');
    const mkey = `${key}${mem64 ? '-m64' : ''}`;
    // The profile lists code found at run time on earlier launches (as
    // float64s: 64-bit code addresses are above 4 GB). Images load at other
    // bases on the two memories, so each has its own.
    const profileBytes = await cacheRead(dir, `${mkey}.profile`);
    const profile = profileBytes ? [...new Float64Array(profileBytes.buffer)] : [];
    let wasm = await cacheRead(dir, `${mkey}.wasm`);
    let translated = false;
    if (!wasm) {
      const t = performance.now();
      wasm = ft.translatePe(exe, { profile, mem64 });
      if (!wasm.length) throw new Error('translation failed');
      translated = true;
      log(`translated ${exeName} in ${(performance.now() - t).toFixed(0)} ms (${(wasm.length / 1024).toFixed(0)} KB${profile.length ? `, ${profile.length} profiled entries` : ''})`);
      await cacheWrite(dir, `${mkey}.wasm`, wasm);
    } else {
      log(`loaded cached translation of ${exeName} (${(wasm.length / 1024).toFixed(0)} KB)`);
    }

    // 64-bit images load at their preferred bases (0x1_4000_0000 for an
    // .exe) on a 64-bit memory, so its guest region is 8 GB; on a 32-bit one
    // they fold to 1 GB and up, and the region is at least 2 GB.
    const limitMB = arch === 'x64' ? Math.max(guestLimitMB, mem64 ? 8192 : 2048) : guestLimitMB;
    const code64 = arch === 'x64' && mem64;
    const machine = new Machine({ abi, kernel: ft.kernel({ mem64, code64 }), arch, mem64, guestLimit: limitMB * 1024 * 1024, log });
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
      await cacheWrite(dir, `${mkey}.profile`, new Uint8Array(Float64Array.from(all).buffer));
      if (dir && all.size > profile.length) {
        try {
          await dir.removeEntry(`${mkey}.wasm`);
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
