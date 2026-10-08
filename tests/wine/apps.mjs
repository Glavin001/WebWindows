#!/usr/bin/env node
// Real Windows programs on translated Wine: the 32-bit and 64-bit builds of
// NASM, 7-Zip, PuTTY (and plink), and the 64-bit SQLite shell, curl and
// trurl, each given a folder as C:\app and its output checked.
//
//   sh tools/apps/fetch.sh                        # once: into target/apps
//   node tests/wine/apps.mjs [--arch x86|x64] [--mem32] [--keep DIR]
//
// Both architectures by default. The 64-bit ones need Node 24 (table64) and
// the x86_64 Wine build (WINE_BUILD64) with the wasm64 Unix side; with
// --mem32 they run on a 32-bit memory with the lowered Unix side instead, as
// browsers without 64-bit WebAssembly memory do.

import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { inflateSync } from 'node:zlib';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const args = process.argv.slice(2);
let archs = ['x86', 'x64'];
let keep = join(root, 'target/apps-test');
let mem32 = false;
for (let i = 0; i < args.length; i++) {
  if (args[i] === '--arch') archs = [args[++i]];
  else if (args[i] === '--mem32') mem32 = true;
  else if (args[i] === '--keep') keep = resolve(args[++i]);
}
rmSync(keep, { recursive: true, force: true });

let failed = 0;
let passed = 0;
const result = (ok, name, detail = '') => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}${detail && !ok ? `: ${detail}` : ''}`);
  if (ok) passed++;
  else failed++;
};

/** Runs `exe args` on Wine with `dir` as C:\app; returns its output. */
function wine(arch, exe, exeArgs, { dir, extra = [] } = {}) {
  const r = spawnSync(
    process.execPath,
    [join(root, 'runtime/node/wine.mjs'), ...(mem32 ? ['--mem32'] : []), ...(dir ? ['--dir', dir] : []), ...extra, join(root, 'target/apps', arch, exe), ...exeArgs],
    { encoding: 'latin1', maxBuffer: 1 << 26, timeout: 600_000 },
  );
  const out = (r.stdout ?? '') + (r.stderr ?? '');
  return { status: r.status, out, stdout: r.stdout ?? '' };
}

function folder(name, files = {}) {
  const d = join(keep, name);
  mkdirSync(d, { recursive: true });
  for (const [f, data] of Object.entries(files)) writeFileSync(join(d, f), data);
  return d;
}

/** The fraction of the screen's pixels near `rgb`, from a PNG display.mjs wrote. */
function share(png, rgb) {
  const buf = readFileSync(png);
  let p = 8;
  let width = 0;
  let height = 0;
  const idat = [];
  while (p < buf.length) {
    const len = buf.readUInt32BE(p);
    const type = buf.toString('latin1', p + 4, p + 8);
    if (type === 'IHDR') [width, height] = [buf.readUInt32BE(p + 8), buf.readUInt32BE(p + 12)];
    else if (type === 'IDAT') idat.push(buf.subarray(p + 8, p + 8 + len));
    p += 12 + len;
  }
  const raw = inflateSync(Buffer.concat(idat));
  let n = 0;
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const o = y * (width * 4 + 1) + 1 + x * 4;
      if (Math.abs(raw[o] - rgb[0]) + Math.abs(raw[o + 1] - rgb[1]) + Math.abs(raw[o + 2] - rgb[2]) < 24) n++;
    }
  }
  return n / (width * height);
}

const ASM = `bits 64
%macro rep_add 2
  %rep %1
    add rax, %2
  %endrep
%endmacro
start:
  mov rax, 0x123456789abcdef0
  rep_add 5, 7
  lea rcx, [rel msg]
  vaddps ymm1, ymm2, [rcx+r8*4+16]
  times 3 db 0x90
  jmp start
msg: db "hello", 0
`;
// What NASM assembles ASM to.
const ASM_BIN = '48b8f0debc9a78563412' + '4883c007'.repeat(5) + '488d0d0c000000c4a16c584c8110909090ebcf68656c6c6f00';

// Compressible text with some variety, for 7-Zip and curl.
const text = Array.from({ length: 4000 }, (_, i) => `line ${i}: ${'abcdefghij'.repeat(1 + (i % 4))} ${(i * 7919) % 1000}\n`).join('');

const SQL = `create table t(id integer primary key, name text, v real);
with recursive c(x) as (select 1 union all select x+1 from c where x<2000) insert into t select x, 'n'||(x*7919%1000), x*1.5/7 from c;
select count(*), sum(id), printf('%.6f', avg(v)), min(name), max(name) from t;
select name, count(*) c from t group by name order by c desc, name limit 3;
select json_object('a', 1, 'b', json_array(1,2,'x')), json_extract('{"k":[1,{"z":5}]}', '$.k[1].z');
select printf('%.10g', exp(1)), printf('%.10g', sqrt(2)), abs(-9223372036854775807), 5/2, 5.0/2, 7 % 3;
select group_concat(id, ',') from (select id from t order by v desc limit 5);
create index ix on t(name);
select count(*) from t where name between 'n100' and 'n200';
select typeof(1e400), quote(x'deadbeef'), length(replace(hex(zeroblob(100)),'00','x'));
`;
const SQL_OUT = `2000|2001000|214.392857|n0|n999
n0|2
n1|2
n10|2
{"a":1,"b":[1,2,"x"]}|5
2.718281828|1.414213562|9223372036854775807|2|2.5|1
2000,1999,1998,1997,1996
224
real|X'DEADBEEF'|100
`;

const have = (arch, exe) => existsSync(join(root, 'target/apps', arch, exe));

for (const arch of archs) {
  if (!have(arch, 'nasm.exe')) {
    console.log(`skip ${arch}: no programs in target/apps/${arch} (sh tools/apps/fetch.sh)`);
    continue;
  }
  if (have(arch, 'nasm.exe')) {
    const d = folder(`${arch}-nasm`, { 'test.asm': ASM });
    const r = wine(arch, 'nasm.exe', ['-f', 'bin', '-o', 'test.bin', 'test.asm'], { dir: d });
    const bin = existsSync(join(d, 'test.bin')) ? readFileSync(join(d, 'test.bin')).toString('hex') : '';
    result(bin === ASM_BIN, `${arch} nasm`, bin ? `assembled ${bin}` : r.out.slice(-500));
    const dis = wine(arch, 'ndisasm.exe', ['-b', '64', 'test.bin'], { dir: d });
    result(/mov rax,0x123456789abcdef0/.test(dis.stdout) && /vaddps ymm1,ymm2,yword \[rcx\+r8\*4\+0x10\]/.test(dis.stdout), `${arch} ndisasm`, dis.out.slice(-500));
  }
  if (have(arch, '7za.exe')) {
    const d = folder(`${arch}-7z`, { 'data.txt': text });
    const a = wine(arch, '7za.exe', ['a', '-mmt=off', '-mx=5', 'out.7z', 'data.txt'], { dir: d });
    const t = wine(arch, '7za.exe', ['t', 'out.7z'], { dir: d });
    result(/Everything is Ok/.test(t.stdout), `${arch} 7za a, t`, (a.out + t.out).slice(-500));
    const x = folder(`${arch}-7z-x`);
    if (existsSync(join(d, 'out.7z'))) writeFileSync(join(x, 'out.7z'), readFileSync(join(d, 'out.7z')));
    const r = wine(arch, '7za.exe', ['x', '-y', 'out.7z'], { dir: x });
    result(existsSync(join(x, 'data.txt')) && readFileSync(join(x, 'data.txt'), 'latin1') === text, `${arch} 7za x`, r.out.slice(-500));
  }
  if (have(arch, 'plink.exe')) {
    const r = wine(arch, 'plink.exe', ['-V']);
    result(new RegExp(`Build platform: ${arch === 'x64' ? 64 : 32}-bit x86 Windows`).test(r.stdout), `${arch} plink -V`, r.out.slice(-500));
  }
  if (have(arch, 'putty.exe')) {
    // The configuration dialog: mostly the dialog's grey, which is not on
    // the desktop.
    const png = join(keep, `putty-${arch}.png`);
    mkdirSync(keep, { recursive: true });
    const r = wine(arch, 'putty.exe', [], { extra: ['--screenshot', png, '--run-for', '60000'] });
    const grey = existsSync(png) ? share(png, [212, 208, 200]) : 0;
    result(grey > 0.15, `${arch} putty (dialog)`, `grey ${grey.toFixed(3)}; ${r.out.slice(-300)}`);
  }
  if (have(arch, 'sqlite3.exe')) {
    const d = folder(`${arch}-sqlite`, { 'q.sql': SQL });
    const r = wine(arch, 'sqlite3.exe', [':memory:', '.read q.sql'], { dir: d });
    result(r.stdout.replaceAll('\r\n', '\n') === SQL_OUT, `${arch} sqlite3`, r.out.slice(-500));
  }
  if (have(arch, 'trurl.exe')) {
    const r = wine(arch, 'trurl.exe', ['--url', 'https://example.com:8080/a/b?x=1#f', '--get', '{host} {port} {path}']);
    result(r.stdout.trim() === 'example.com 8080 /a/b', `${arch} trurl`, r.out.slice(-500));
  }
  if (have(arch, 'curl.exe')) {
    const v = wine(arch, 'curl.exe', ['-V']);
    result(/^curl \d+\.\d+/m.test(v.stdout) && /Protocols: .*https/.test(v.stdout), `${arch} curl -V`, v.out.slice(-500));
    const d = folder(`${arch}-curl`, { 'data.txt': text });
    const r = wine(arch, 'curl.exe', ['-sS', '-o', 'copy.txt', 'file:///C:/app/data.txt'], { dir: d });
    result(existsSync(join(d, 'copy.txt')) && readFileSync(join(d, 'copy.txt'), 'latin1') === text, `${arch} curl file://`, r.out.slice(-500));
  }
}
console.log(`${passed} passed, ${failed} failed`);
process.exit(failed ? 1 : 0);
