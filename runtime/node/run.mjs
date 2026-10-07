#!/usr/bin/env node
// Runs a translated Windows program in Node.
//
//   node runtime/node/run.mjs program.exe [args...]
//
// Options (before the program): --wasm <file> (translated module; default:
// translate with `wwt`), --wwt <path> (translator binary), --trace (log API
// calls), --guest-limit <MB>, --profile <file> (append missed addresses).

import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync, appendFileSync, mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve, basename } from 'node:path';
import { fileURLToPath } from 'node:url';

import { Machine, ProcessExit, GuestFault, hex } from '../runtime.mjs';
import { Process } from '../win32.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '../..');

function findWwt(explicit) {
  if (explicit) return explicit;
  if (process.env.WWT) return process.env.WWT;
  for (const p of ['target/release/wwt', 'target/debug/wwt']) {
    const f = join(root, p);
    if (existsSync(f)) return f;
  }
  throw new Error('wwt translator not found; build it with `cargo build -p wwt-cli`');
}

export async function runExe(exePath, argv, opts = {}) {
  const wwt = () => findWwt(opts.wwt);
  let wasmPath = opts.wasm;
  if (!wasmPath) {
    const dir = mkdtempSync(join(tmpdir(), 'wwt-'));
    wasmPath = join(dir, basename(exePath) + '.wasm');
    const extra = opts.translateArgs ?? [];
    execFileSync(wwt(), ['translate', exePath, '-o', wasmPath, ...extra], {
      stdio: ['ignore', 'ignore', opts.quiet ? 'ignore' : 'inherit'],
    });
  }
  const abi = JSON.parse(execFileSync(wwt(), ['abi']).toString());
  const kdir = mkdtempSync(join(tmpdir(), 'wwt-k-'));
  execFileSync(wwt(), ['kernel', '-o', join(kdir, 'kernel.wasm')]);
  const kernel = readFileSync(join(kdir, 'kernel.wasm'));

  const out = [];
  const machine = new Machine({
    abi,
    kernel,
    guestLimit: (opts.guestLimitMB ?? 1024) * 1024 * 1024,
    log: opts.verbose ? (s) => process.stderr.write(s + '\n') : undefined,
  });
  await machine.init();
  const mod = await machine.loadModule(readFileSync(wasmPath), basename(wasmPath));
  const proc = new Process(machine, {
    argv: [basename(exePath), ...argv],
    stdout: opts.stdout ?? ((b) => (opts.capture ? out.push(Buffer.from(b)) : process.stdout.write(b))),
    stderr: opts.stderr ?? ((b) => process.stderr.write(b)),
    trace: opts.trace,
  });
  proc.load(readFileSync(exePath), mod.meta.image);
  let exitCode;
  let error = null;
  try {
    exitCode = proc.start();
  } catch (e) {
    if (e instanceof ProcessExit) exitCode = e.exitCode;
    else error = e;
  }
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
    else if (a === '--verbose') opts.verbose = true;
    else if (a === '--profile') opts.profile = args.shift();
    else if (a === '--guest-limit') opts.guestLimitMB = Number(args.shift());
    else throw new Error(`unknown option ${a}`);
  }
  const exe = args.shift();
  if (!exe) {
    process.stderr.write('usage: run.mjs [options] program.exe [args...]\n');
    process.exit(2);
  }
  const r = await runExe(exe, args, opts);
  if (r.error) {
    const e = r.error;
    process.stderr.write(`\n*** ${e instanceof GuestFault ? 'guest fault' : 'error'}: ${e.message}\n`);
    if (!(e instanceof GuestFault)) process.stderr.write(e.stack + '\n');
    process.exit(128);
  }
  process.exit(r.exitCode);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((e) => {
    process.stderr.write(e.stack + '\n');
    process.exit(1);
  });
}
