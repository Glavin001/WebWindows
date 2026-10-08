#!/usr/bin/env node
// Runs the MinGW executables built by check.mjs natively (on Windows) and
// compares exit codes with the recorded translated runs.
//
//   node tests/programs/windows-check.mjs <dir with *.exe and results.json>

import { spawnSync } from 'node:child_process';
import { readFileSync, existsSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

const dir = process.argv[2] ?? 'target/programs';
const results = readdirSync(dir)
  .filter((f) => /^results.*\.json$/.test(f))
  .flatMap((f) => JSON.parse(readFileSync(join(dir, f), 'utf8')));
let bad = 0;
let ran = 0;
for (const r of results) {
  // Programs that need what CI's Windows machines lack (a sound device).
  if (r.status !== 'pass' || r.native === false) continue;
  // Results name their executable (torture tests share names with ours and
  // live in a subdirectory, which CI does not upload).
  const exe = join(dir, r.exe ?? `${r.name}-${r.opt}.exe`);
  if (!existsSync(exe)) continue;
  const run = spawnSync(exe, [], { encoding: 'latin1', timeout: 20000 });
  ran++;
  if (r.exitCode !== undefined && (run.status & 0xff) !== r.exitCode) {
    console.log(`MISMATCH ${r.name} -${r.opt}: native exit ${run.status}, translated ${r.exitCode}`);
    bad++;
  }
  // Line ends: the Linux reference prints \n; native Windows programs and
  // translated Wine's msvcrt (recorded for the Windows-only tests) print \r\n.
  const lf = (t) => t.replace(/\r\n/g, '\n');
  if (r.stdout !== undefined && lf(run.stdout) !== lf(r.stdout)) {
    const [want, got] = [lf(r.stdout).split('\n'), lf(run.stdout).split('\n')];
    const i = want.findIndex((l, k) => l !== got[k]);
    console.log(`MISMATCH ${r.name} -${r.opt}: stdout differs at line ${i + 1}`);
    console.log(`  translated: ${JSON.stringify(want[i])}\n  native:     ${JSON.stringify(got[i])}`);
    bad++;
  }
}
console.log(`${ran} executables run natively, ${bad} mismatches`);
process.exit(bad ? 1 : 0);
