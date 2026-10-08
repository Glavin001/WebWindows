#!/usr/bin/env node
// Runs a translated Windows program in Node.
//
//   node runtime/node/run.mjs program.exe [args...]
//
// Options (before the program): --wasm <file> (translated module; default:
// translate with `wwt`), --wwt <path> (translator binary), --trace (log API
// calls), --guest-limit <MB>, --profile <file> (append missed addresses),
// --no-fast (disable run-time translation of code the translator missed).
// 64-bit programs (detected from the PE header) run on a 64-bit (memory64)
// WebAssembly memory (Node 22.22 and 24 have it; earlier Node 22 releases
// need --experimental-wasm-memory64); --mem32 runs them on a 32-bit memory
// instead (addresses below 4 GB), --mem64 runs 32-bit programs on a 64-bit
// one.

import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync, appendFileSync, mkdtempSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve, basename } from 'node:path';
import { fileURLToPath } from 'node:url';

import { Machine, ProcessExit, GuestFault, hex, hasMemory64 } from '../runtime.mjs';
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

/**
 * The default guest region: 8 GB for 64-bit programs on a 64-bit memory,
 * whose images load at their preferred bases (0x1_4000_0000 for an .exe);
 * 3 GB on a 32-bit one, where those images fold to 1-2 GB
 * (0x1_4000_0000 -> 0x4000_0000); 1 GB for 32-bit programs.
 */
export function defaultGuestLimitMB(arch, mem64) {
  if (arch !== 'x64') return 1024;
  return mem64 ? 8192 : 3072;
}

export async function runExe(exePath, argv, opts = {}) {
  const wwt = () => findWwt(opts.wwt);
  const arch = peArch(readFileSync(exePath));
  const mem64 = opts.mem64 ?? arch === 'x64';
  if (mem64 && !hasMemory64()) {
    throw new Error(
      'this Node has no 64-bit WebAssembly memory, which 64-bit programs use: ' +
        'run Node 22.22 or 24, or node --experimental-wasm-memory64 (or pass --mem32)',
    );
  }
  const m64 = mem64 ? ['--mem64'] : [];
  // x86-64 code on a 64-bit memory has 64-bit code addresses.
  const code64 = mem64 && arch === 'x64';
  let wasmPath = opts.wasm;
  if (!wasmPath) {
    const dir = mkdtempSync(join(tmpdir(), 'wwt-'));
    wasmPath = join(dir, basename(exePath) + '.wasm');
    const extra = [...(opts.translateArgs ?? []), ...(opts.memTraps ? ['--mem-traps'] : [])];
    // A guest limit known at translation time (a constant in the checks)
    // with a 32-bit memory; a 64-bit one reads it at run time.
    const limitMB = opts.guestLimitMB ?? defaultGuestLimitMB(arch, mem64);
    const limit = mem64 ? [] : ['--guest-limit-mb', String(limitMB)];
    execFileSync(wwt(), ['translate', exePath, '-o', wasmPath, ...m64, ...limit, ...extra], {
      stdio: ['ignore', 'ignore', opts.quiet ? 'ignore' : 'inherit'],
    });
  }
  const abi = JSON.parse(execFileSync(wwt(), ['abi']).toString());
  const kdir = mkdtempSync(join(tmpdir(), 'wwt-k-'));
  execFileSync(wwt(), ['kernel', '-o', join(kdir, 'kernel.wasm'), ...m64, ...(code64 ? ['--code64'] : [])]);
  const kernel = readFileSync(join(kdir, 'kernel.wasm'));

  const out = [];
  const machine = new Machine({
    abi,
    kernel,
    arch,
    mem64,
    guestLimit: (opts.guestLimitMB ?? defaultGuestLimitMB(arch, mem64)) * 1024 * 1024,
    log: opts.verbose ? (s) => stderr(s + '\n') : undefined,
  });
  await machine.init();
  if (opts.fast !== false) {
    const tw = findTranslatorWasm();
    if (tw) {
      const ft = await FastTranslator.load(readFileSync(tw));
      enableFastMode(machine, ft, { log: opts.verbose ? (s) => stderr(s + '\n') : undefined, memTraps: opts.memTraps });
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
    else if (a === '--mem32') opts.mem64 = false;
    else if (a === '--mem-traps') opts.memTraps = true;
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
