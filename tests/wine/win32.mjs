#!/usr/bin/env node
// Windows-only test programs (Milestone 5 and on): exceptions, threads,
// timers... They cannot run natively on Linux, so each has its expected
// output recorded next to it (NAME.out); an `exit: N` line in the source
// gives the expected exit code (default 0), a `libs: -lfoo` line libraries
// to link, a `native: skip` line keeps it off the native Windows check. Each is built with MinGW at -O0
// and -O2 and run on translated Wine. CI runs the same executables on real
// Windows (tests/programs/windows-check.mjs), which checks the recordings.
//
//   node tests/wine/win32.mjs [--update] [--jobs N] [name...]
//
// --update rewrites the .out files from the translated runs.

import { spawn, execFileSync } from 'node:child_process';
import { mkdirSync, readdirSync, readFileSync, writeFileSync, existsSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '../..');
const src = join(here, 'win32');
const work = join(root, 'target/win32');
mkdirSync(work, { recursive: true });

const args = process.argv.slice(2);
let update = false;
let jobs = 2;
const names = [];
while (args.length) {
  const a = args.shift();
  if (a === '--update') update = true;
  else if (a === '--jobs') jobs = Number(args.shift());
  else names.push(a.replace(/\.c$/, ''));
}
const programs = readdirSync(src)
  .filter((f) => f.endsWith('.c'))
  .map((f) => basename(f, '.c'))
  .filter((n) => !names.length || names.includes(n))
  .sort();

function run(cmd, argv, timeoutMs) {
  return new Promise((done) => {
    const p = spawn(cmd, argv, { cwd: root });
    let stdout = '';
    let stderr = '';
    p.stdout.on('data', (d) => (stdout += d.toString('latin1')));
    p.stderr.on('data', (d) => (stderr += d.toString('latin1')));
    const timer = setTimeout(() => p.kill('SIGKILL'), timeoutMs);
    p.on('close', (code, signal) => {
      clearTimeout(timer);
      done({ code, signal, stdout, stderr });
    });
  });
}

const builds = [];
for (const name of programs) {
  const source = readFileSync(join(src, `${name}.c`), 'utf8');
  const exit = Number(/exit:\s*(\d+)/.exec(source)?.[1] ?? 0);
  const libs = (/libs:\s*(.*)/.exec(source)?.[1] ?? '').trim().split(/\s+/).filter(Boolean);
  const native = !/native:\s*skip/.test(source);
  const outFile = join(src, `${name}.out`);
  const expected = existsSync(outFile) ? readFileSync(outFile, 'latin1') : null;
  for (const opt of ['O0', 'O2']) {
    const exe = join(work, `${name}-${opt}.exe`);
    execFileSync('i686-w64-mingw32-gcc', [`-${opt}`, '-o', exe, join(src, `${name}.c`), ...libs], { stdio: 'inherit' });
    builds.push({ name, opt, exe, exit, expected, outFile, native });
  }
}

const results = [];
let failed = 0;
let next = 0;
async function worker() {
  while (next < builds.length) {
    const b = builds[next++];
    const r = await run('node', [join(root, 'runtime/node/wine.mjs'), b.exe], 300000);
    const exitCode = r.code ?? -1;
    let status = 'pass';
    const why = [];
    if (exitCode !== b.exit) why.push(`exit ${r.signal ?? exitCode}, expected ${b.exit}`);
    // (With --update, the -O2 run records it.)
    if (b.expected === null) update || why.push('no recorded output');
    else if (r.stdout !== b.expected) why.push('output differs');
    if (why.length && !(update && b.opt === 'O2' && exitCode === b.exit)) status = 'fail';
    if (update && b.opt === 'O2' && exitCode === b.exit) writeFileSync(b.outFile, r.stdout, 'latin1');
    results.push({ name: b.name, opt: b.opt, exe: basename(b.exe), status, exitCode: exitCode & 0xff, stdout: r.stdout, native: b.native });
    if (status === 'fail') {
      failed++;
      console.log(`FAIL ${b.name} -${b.opt}: ${why.join('; ')}`);
      if (b.expected !== null && r.stdout !== b.expected) {
        console.log('--- expected\n' + b.expected + '--- got\n' + r.stdout + '---');
      }
      console.log(r.stderr.split('\n').slice(-15).join('\n'));
    } else {
      console.log(`ok   ${b.name} -${b.opt}`);
    }
  }
}
await Promise.all(Array.from({ length: jobs }, worker));
writeFileSync(join(work, 'results-win32.json'), JSON.stringify(results, null, 1));
console.log(`${results.length - failed}/${results.length} Windows test programs passed on translated Wine`);
process.exit(failed ? 1 : 0);
