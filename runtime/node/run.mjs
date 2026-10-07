#!/usr/bin/env node
// Runs a translated Windows program in Node.
//
//   node runtime/node/run.mjs program.exe [args...]
//
// Options (before the program): --wasm <file> (translated module; default:
// translate with `wwt`), --wwt <path> (translator binary), --trace (log API
// calls), --guest-limit <MB>, --profile <file> (append missed addresses),
// --no-fast (disable run-time translation of code the translator missed),
// --mem64 (64-bit WebAssembly memory; Node 22 needs
// --experimental-wasm-memory64). 64-bit programs are detected from the PE
// header.

import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync, appendFileSync, mkdtempSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve, basename } from 'node:path';
import { fileURLToPath } from 'node:url';

import { Machine, ProcessExit, GuestFault, hex } from '../runtime.mjs';
import { Process } from '../win32.mjs';
import { peArch } from '../pe.mjs';
import { FastTranslator, enableFastMode } from '../fastmode.mjs';
import { stdout, stderr } from './output.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '../..');

function findWwt(explicit) {
  if (explicit) return explicit;
  if (process.env.WWT) return process.env.WWT;
  // The most recently built translator.
  const found = ['target/release/wwt', 'target/debug/wwt']
    .map((p) => join(root, p))
    .filter((f) => existsSync(f))
    .sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs);
  if (found.length) return found[0];
  throw new Error('wwt translator not found; build it with `cargo build -p wwt-cli`');
}

function findTranslatorWasm() {
  if (process.env.WWT_WASM) return process.env.WWT_WASM;
  for (const p of ['target/wasm32-unknown-unknown/release-wasm/wwt_wasm.wasm', 'target/wasm32-unknown-unknown/release/wwt_wasm.wasm']) {
    const f = join(root, p);
    if (existsSync(f)) return f;
  }
  return null;
}

export async function runExe(exePath, argv, opts = {}) {
  const wwt = () => findWwt(opts.wwt);
  const arch = peArch(readFileSync(exePath));
  const mem64 = opts.mem64 ?? false;
  const m64 = mem64 ? ['--mem64'] : [];
  let wasmPath = opts.wasm;
  if (!wasmPath) {
    const dir = mkdtempSync(join(tmpdir(), 'wwt-'));
    wasmPath = join(dir, basename(exePath) + '.wasm');
    const extra = opts.translateArgs ?? [];
    execFileSync(wwt(), ['translate', exePath, '-o', wasmPath, ...m64, ...extra], {
      stdio: ['ignore', 'ignore', opts.quiet ? 'ignore' : 'inherit'],
    });
  }
  const abi = JSON.parse(execFileSync(wwt(), ['abi']).toString());
  const kdir = mkdtempSync(join(tmpdir(), 'wwt-k-'));
  execFileSync(wwt(), ['kernel', '-o', join(kdir, 'kernel.wasm'), ...m64]);
  const kernel = readFileSync(join(kdir, 'kernel.wasm'));

  const out = [];
  const machine = new Machine({
    abi,
    kernel,
    arch,
    mem64,
    // 64-bit images fold to 1-2 GB (0x1_4000_0000 -> 0x4000_0000), so their
    // guest region defaults larger.
    guestLimit: (opts.guestLimitMB ?? (arch === 'x64' ? 3072 : 1024)) * 1024 * 1024,
    log: opts.verbose ? (s) => stderr(s + '\n') : undefined,
  });
  await machine.init();
  if (opts.fast !== false) {
    const tw = findTranslatorWasm();
    if (tw) {
      const ft = await FastTranslator.load(readFileSync(tw));
      enableFastMode(machine, ft, { log: opts.verbose ? (s) => stderr(s + '\n') : undefined });
    }
  }
  const mod = await machine.loadModule(readFileSync(wasmPath), basename(wasmPath));
  const proc = new Process(machine, {
    argv: [basename(exePath), ...argv],
    stdout: opts.stdout ?? ((b) => (opts.capture ? out.push(Buffer.from(b)) : stdout(b))),
    stderr: opts.stderr ?? ((b) => stderr(b)),
    trace: opts.trace,
  });
  proc.load(readFileSync(exePath), mod.meta.image);
  let exitCode;
  let error = null;
  const t0 = performance.now();
  try {
    exitCode = proc.start();
  } catch (e) {
    if (e instanceof ProcessExit) exitCode = e.exitCode;
    else error = e;
  }
  const runMs = performance.now() - t0;
  if (opts.time) stderr(`run time: ${runMs.toFixed(1)} ms\n`);
  if (opts.profile && machine.profile.size) {
    appendFileSync(opts.profile, [...machine.profile].map((a) => hex(a)).join('\n') + '\n');
  }
  return { exitCode, error, stdout: Buffer.concat(out), machine, proc };
}

async function main() {
  const args = process.argv.slice(2);
  const opts = {};
  while (args[0]?.startsWith('--')) {
    const a = args.shift();
    if (a === '--wasm') opts.wasm = args.shift();
    else if (a === '--wwt') opts.wwt = args.shift();
    else if (a === '--trace') opts.trace = true;
    else if (a === '--no-fast') opts.fast = false;
    else if (a === '--time') opts.time = true;
    else if (a === '--verbose') opts.verbose = true;
    else if (a === '--profile') opts.profile = args.shift();
    else if (a === '--guest-limit') opts.guestLimitMB = Number(args.shift());
    else if (a === '--mem64') opts.mem64 = true;
    else throw new Error(`unknown option ${a}`);
  }
  const exe = args.shift();
  if (!exe) {
    stderr('usage: run.mjs [options] program.exe [args...]\n');
    process.exit(2);
  }
  const r = await runExe(exe, args, opts);
  if (r.error) {
    const e = r.error;
    stderr(`\n*** ${e instanceof GuestFault ? 'guest fault' : 'error'}: ${e.message}\n`);
    if (!(e instanceof GuestFault)) stderr(e.stack + '\n');
    process.exit(128);
  }
  process.exit(r.exitCode);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((e) => {
    stderr(e.stack + '\n');
    process.exit(1);
  });
}
