#!/usr/bin/env node
// Runs the MinGW executables built by check.mjs natively (on Windows) and
// compares exit codes with the recorded translated runs.
//
//   node tests/programs/windows-check.mjs <dir with *.exe and results.json>

import { spawnSync } from 'node:child_process';
import { readFileSync, existsSync } from 'node:fs';
import { join } from 'node:path';

const dir = process.argv[2] ?? 'target/programs';
const results = JSON.parse(readFileSync(join(dir, 'results.json'), 'utf8'));
let bad = 0;
let ran = 0;
for (const r of results) {
  if (r.status !== 'pass') continue;
  const exe = join(dir, `${r.name}-${r.opt}.exe`);
  if (!existsSync(exe)) continue;
  const run = spawnSync(exe, [], { encoding: 'latin1', timeout: 20000 });
  ran++;
  if (r.exitCode !== undefined && (run.status & 0xff) !== r.exitCode) {
    console.log(`MISMATCH ${r.name} -${r.opt}: native exit ${run.status}, translated ${r.exitCode}`);
    bad++;
  }
  if (r.stdout !== undefined && run.stdout.replace(/\r\n/g, '\n') !== r.stdout) {
    console.log(`MISMATCH ${r.name} -${r.opt}: stdout differs`);
    bad++;
  }
}
console.log(`${ran} executables run natively, ${bad} mismatches`);
process.exit(bad ? 1 : 0);
