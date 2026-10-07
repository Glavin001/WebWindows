#!/usr/bin/env node
// Test layer 3: Wine's own conformance tests, run on translated Wine.
//
// Runs every unit of a Wine test program (e.g. kernel32_test.exe) in its own
// process with a time limit, and records each unit's summary line:
//   "<unit>: N tests executed (T marked as todo, F as flaky, X failures), S skipped."
// A unit that crashes or hangs is recorded as such. Results go to
// target/winetest/<name>.json (<name>64.json for a 64-bit one); with --baseline FILE the run fails when a unit
// does worse than the baseline (more failures, or no longer finishing), and
// --write-baseline FILE records the current results as the new baseline.
//
//   node tests/wine/winetest.mjs [--jobs N] [--timeout S] [--baseline F]
//        [--write-baseline F] path/to/kernel32_test.exe [unit ...]

import { spawn } from 'node:child_process';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const args = process.argv.slice(2);
let jobs = 4;
let timeout = 120;
let baseline = null;
let writeBaseline = null;
const rest = [];
while (args.length) {
  const a = args.shift();
  if (a === '--jobs') jobs = Number(args.shift());
  else if (a === '--timeout') timeout = Number(args.shift());
  else if (a === '--baseline') baseline = args.shift();
  else if (a === '--write-baseline') writeBaseline = args.shift();
  else rest.push(a);
}
const exe = resolve(rest.shift() ?? '');
// 64-bit test programs record as <name>64.
const pe = readFileSync(exe);
const name = basename(exe, '.exe') + (pe.readUInt16LE(pe.readUInt32LE(0x3c) + 4) === 0x8664 ? '64' : '');

function run(argv, ms) {
  return new Promise((done) => {
    const child = spawn(process.execPath, [join(root, 'runtime/node/wine.mjs'), exe, ...argv], { stdio: ['ignore', 'pipe', 'pipe'] });
    const out = [];
    const err = [];
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      child.kill('SIGKILL');
    }, ms);
    child.stdout.on('data', (d) => out.push(d));
    child.stderr.on('data', (d) => err.push(d));
    child.on('close', (status) => {
      clearTimeout(timer);
      done({ status, timedOut, stdout: Buffer.concat(out).toString('latin1'), stderr: Buffer.concat(err).toString('latin1') });
    });
  });
}

let units = rest;
if (!units.length) {
  const r = await run(['--list'], 60000);
  units = r.stdout.split('\n').slice(1).map((s) => s.trim()).filter(Boolean);
}

async function runUnit(unit) {
  const t0 = performance.now();
  const r = await run([unit], timeout * 1000);
  const ms = Math.round(performance.now() - t0);
  const m = r.stdout.match(/: (\d+) tests? executed \((\d+) marked as todo, (?:(\d+) as flaky, )?(\d+) failures?\), (\d+) skipped/);
  // Failures the unit reported before it stopped, when it did not finish.
  const failedLines = (r.stdout.match(/Test failed:/g) ?? []).length;
  const unimplemented = r.stderr.match(/unimplemented syscalls: (.*)/)?.[1].split(', ') ?? [];
  if (m) {
    return { unit, status: 'done', tests: +m[1], todo: +m[2], failures: +m[4], skipped: +m[5], ms, unimplemented };
  }
  const why = r.timedOut
    ? 'timed out'
    : (r.stderr.split('\n').find((l) => /\*\*\*|Error|fault|panicked/.test(l)) ?? `exit ${r.status}`).trim().slice(0, 200);
  return { unit, status: r.timedOut ? 'timeout' : 'crash', failures: failedLines, why, ms, unimplemented };
}

const results = [];
let next = 0;
await Promise.all(
  Array.from({ length: jobs }, async () => {
    while (next < units.length) {
      const u = units[next++];
      const r = await runUnit(u);
      results.push(r);
      const desc =
        r.status === 'done'
          ? `${r.tests} tests, ${r.failures} failures, ${r.todo} todo, ${r.skipped} skipped`
          : `${r.status}: ${r.why}`;
      console.log(`${r.status === 'done' && !r.failures ? 'ok  ' : r.status === 'done' ? 'some' : 'FAIL'} ${u.padEnd(12)} ${desc} (${(r.ms / 1000).toFixed(1)} s)`);
    }
  }),
);
results.sort((a, b) => a.unit.localeCompare(b.unit));

const done = results.filter((r) => r.status === 'done');
const tests = done.reduce((s, r) => s + r.tests, 0);
const failures = done.reduce((s, r) => s + r.failures, 0);
const clean = done.filter((r) => !r.failures).length;
console.log(
  `${name}: ${results.length} units, ${done.length} finished (${clean} with no failures), ` +
    `${results.length - done.length} crashed or timed out; ${tests} tests executed, ${failures} failed ` +
    `(${tests ? ((100 * (tests - failures)) / tests).toFixed(2) : 0}% pass)`,
);
mkdirSync(join(root, 'target/winetest'), { recursive: true });
writeFileSync(join(root, 'target/winetest', `${name}.json`), JSON.stringify(results, null, 2));

const brief = (r) =>
  r.status === 'done' ? { status: 'done', tests: r.tests, failures: r.failures } : { status: r.status, failures: r.failures };
if (writeBaseline) {
  writeFileSync(writeBaseline, JSON.stringify(Object.fromEntries(results.map((r) => [r.unit, brief(r)])), null, 2) + '\n');
}
let regressed = false;
if (baseline) {
  const base = JSON.parse(readFileSync(baseline, 'utf8'));
  for (const r of results) {
    const b = base[r.unit];
    if (!b) continue;
    if (b.status === 'done' && r.status !== 'done') {
      console.log(`REGRESSION ${r.unit}: finished in the baseline, now ${r.status} (${r.why})`);
      regressed = true;
    } else if (b.status === 'done' && r.failures > b.failures) {
      console.log(`REGRESSION ${r.unit}: ${r.failures} failures, baseline ${b.failures}`);
      regressed = true;
    } else if (r.status === 'done' && (b.status !== 'done' || r.failures < b.failures)) {
      console.log(`improved   ${r.unit}: ${r.failures} failures (baseline ${b.status === 'done' ? b.failures : b.status})`);
    }
  }
}
process.exit(regressed ? 1 : 0);
