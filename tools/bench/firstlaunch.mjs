#!/usr/bin/env node
// What a first launch costs before any code runs, per image: translating
// it (the native translator ahead of time, and the translator compiled to
// WebAssembly as the page runs it) and compiling the result (V8's baseline
// tier, its optimizing tier, and the lazy default that only validates).
//
//   node tools/bench/firstlaunch.mjs [--json F] [image.exe|dll ...]
//
// Without images: the suite's programs (target/bench) and Wine's largest
// DLLs. Reports x86 code bytes, output bytes per x86 byte, and translation
// speed in MB of x86 code per second.

import { execFileSync, spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { FastTranslator } from '../../runtime/fastmode.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const wwt = process.env.WWT ?? join(root, 'target/release/wwt');
const wineBuild = process.env.WINE_BUILD ?? '/opt/wine-build';
const GUEST_LIMIT_MB = 1024;

const argv = process.argv.slice(2);
let json = null;
const images = [];
while (argv.length) {
  const a = argv.shift();
  if (a === '--json') json = argv.shift();
  else images.push(resolve(a));
}
if (!images.length) {
  for (const w of ['coremark', 'lua', 'sqlite', 'apibench']) images.push(join(root, `target/bench/suite-${w}.exe`));
  for (const d of ['ntdll', 'kernelbase', 'msvcrt', 'user32']) images.push(join(wineBuild, `dlls/${d}/i386-windows/${d}.dll`));
}

/** Bytes of executable sections (their virtual size). */
function x86Bytes(file) {
  const dv = new DataView(file.buffer, file.byteOffset, file.byteLength);
  const pe = dv.getUint32(0x3c, true);
  const nsec = dv.getUint16(pe + 6, true);
  const opt = pe + 24;
  const optSize = dv.getUint16(pe + 20, true);
  let n = 0;
  for (let i = 0; i < nsec; i++) {
    const s = opt + optSize + i * 40;
    if (dv.getUint32(s + 36, true) & 0x20000000) n += Math.max(dv.getUint32(s + 8, true), dv.getUint32(s + 16, true));
  }
  return n;
}

/** Seconds to compile `wasm` in a fresh Node with `flags` (median of 3). */
function compileTime(wasmPath, flags) {
  const code = `const b=require('fs').readFileSync(${JSON.stringify(wasmPath)});const t=performance.now();new WebAssembly.Module(b);console.log(performance.now()-t)`;
  const ts = [];
  for (let i = 0; i < 3; i++) {
    const r = spawnSync(process.execPath, [...flags, '-e', code], { encoding: 'utf8' });
    if (r.status !== 0) return null;
    ts.push(Number(r.stdout) / 1000);
  }
  return ts.sort((a, b) => a - b)[1];
}

const tw = join(root, 'target/wasm32-unknown-unknown/release-wasm/wwt_wasm.wasm');
const fast = existsSync(tw) ? await FastTranslator.load(readFileSync(tw)) : null;
const tmp = mkdtempSync(join(tmpdir(), 'wwt-first-'));
const rows = [];
for (const img of images) {
  if (!existsSync(img)) {
    console.error(`missing ${img}`);
    continue;
  }
  const bytes = readFileSync(img);
  const x86 = x86Bytes(bytes);
  const out = join(tmp, `${basename(img)}.wasm`);
  let t0 = performance.now();
  execFileSync(wwt, ['translate', img, '-o', out, '--guest-limit-mb', String(GUEST_LIMIT_MB)], { stdio: 'ignore' });
  const aot = (performance.now() - t0) / 1000;
  const aotBytes = statSync(out).size;
  let wasmTr = null;
  let wasmBytes = null;
  if (fast) {
    t0 = performance.now();
    try {
      wasmBytes = fast.translatePe(bytes, { guestLimit: GUEST_LIMIT_MB << 20 }).length;
      wasmTr = (performance.now() - t0) / 1000;
    } catch (e) {
      console.error(`${basename(img)}: wasm translator: ${e.message}`);
    }
  }
  const row = {
    image: basename(img),
    x86,
    aot: { seconds: aot, bytes: aotBytes, mbPerSec: x86 / 1e6 / aot, ratio: aotBytes / x86 },
    wasmTranslator: wasmTr && { seconds: wasmTr, bytes: wasmBytes, mbPerSec: x86 / 1e6 / wasmTr },
    compile: {
      lazy: compileTime(out, []),
      liftoff: compileTime(out, ['--no-wasm-lazy-compilation', '--liftoff', '--no-wasm-tier-up']),
      turbofan: compileTime(out, ['--no-wasm-lazy-compilation', '--no-liftoff']),
    },
  };
  rows.push(row);
  const f = (x, d = 2) => (x == null ? '—' : x.toFixed(d));
  console.log(
    `${row.image.padEnd(22)} x86 ${(x86 / 1024).toFixed(0).padStart(5)} KB → ${(aotBytes / 1024).toFixed(0).padStart(6)} KB (${f(row.aot.ratio, 1)}×)` +
      `  translate ${f(aot)} s (${f(row.aot.mbPerSec)} MB/s), in wasm ${f(wasmTr)} s` +
      `  compile lazy ${f(row.compile.lazy, 3)} s, baseline ${f(row.compile.liftoff)} s, optimized ${f(row.compile.turbofan)} s`,
  );
}
rmSync(tmp, { recursive: true, force: true });
if (json) writeFileSync(json, JSON.stringify({ date: new Date().toISOString(), node: process.version, rows }, null, 2) + '\n');
