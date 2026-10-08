#!/usr/bin/env node
// Compares one CoreMark function as wwt translated it and as Emscripten
// compiled it: a table of instruction counts by kind, plus the x86, IR and
// both WebAssembly listings written to target/bench/inspect/ for reading.
//
//   node tools/bench/inspect.mjs core_state_transition [more names...]
//
// Names match symbols as a substring (`core_list` matches several). The
// counts are static, so weigh them by the profile (tools/bench/profile.mjs);
// fault paths (register write-back before raising an access violation) are
// counted apart, as they only run when the program faults.
// GCC and clang inline differently: a function Emscripten inlined has no
// listing of its own there.

import { execFileSync, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const bench = join(root, 'target/bench');
const outDir = join(bench, 'inspect');
const wwt = join(root, 'target/release/wwt');
const exe = join(bench, 'coremark.exe');
const emccWasm = join(bench, 'coremark.emcc.wasm');

const names = process.argv.slice(2);
if (!names.length) {
  console.error('usage: node tools/bench/inspect.mjs <function name>...');
  process.exit(2);
}
if (![exe, emccWasm].every(existsSync)) {
  execFileSync(process.execPath, [join(root, 'tools/bench/coremark.mjs'), '--build-only'], { stdio: 'inherit' });
}

const wat = (file) => execFileSync(wwt, ['wat', file], { maxBuffer: 256 << 20 }).toString();

/** Splits a module's text into functions: name -> body lines. */
function functions(text) {
  const out = new Map();
  let cur = null;
  for (const line of text.split('\n')) {
    const m = line.match(/^  \(func \$(\S+)/);
    if (m) {
      cur = [];
      out.set(m[1], cur);
    } else if (/^  \(/.test(line)) {
      cur = null;
    } else if (cur) {
      cur.push(line.trim());
    }
  }
  return out;
}

// Imported globals of translated modules (see crates/wwt/src/codegen.rs).
const GUEST_CHECK = 'global.get 2';
const CODE_BITMAP = 'global.get 3';

/** Marks fault paths: `if` bodies that end in a call to the fault import. */
function coldLines(lines) {
  const cold = new Array(lines.length).fill(false);
  const ifs = [];
  lines.forEach((l, i) => {
    const op = l.split(/\s+/)[0];
    if (['if', 'block', 'loop'].includes(op)) ifs.push([op, i]);
    else if (op === 'end') {
      const [kind, start] = ifs.pop() ?? [];
      if (kind === 'if' && lines[i - 1] === 'unreachable' && lines[i - 2] === 'call 0') {
        for (let k = start + 1; k < i; k++) cold[k] = true;
      }
    }
  });
  return cold;
}

function count(lines) {
  const cold = coldLines(lines);
  lines = lines.filter((_, i) => !cold[i]);
  const c = {
    'fault-path instructions': cold.filter(Boolean).length,
    instructions: 0,
    'local.get/set/tee': 0,
    const: 0,
    loads: 0,
    stores: 0,
    'branches (br, br_if, if)': 0,
    br_table: 0,
    calls: 0,
    'indirect calls': 0,
    'guest-limit checks': 0,
    'code-write checks': 0,
    'other (arithmetic etc.)': 0,
  };
  for (const l of lines) {
    const op = l.split(/\s+/)[0];
    if (!op || op.startsWith(';;') || op.startsWith('(local') || ['end', 'else', 'block', 'loop'].includes(op)) continue;
    c.instructions++;
    if (l === GUEST_CHECK) c['guest-limit checks']++;
    else if (l === CODE_BITMAP) c['code-write checks']++;
    else if (/^local\./.test(op)) c['local.get/set/tee']++;
    else if (/\.const$/.test(op)) c.const++;
    else if (/\.load/.test(op)) c.loads++;
    else if (/\.store/.test(op)) c.stores++;
    else if (op === 'br_table') c.br_table++;
    else if (['br', 'br_if', 'if'].includes(op)) c['branches (br, br_if, if)']++;
    else if (/call_indirect$/.test(op)) c['indirect calls']++;
    else if (/^(return_)?call$/.test(op)) c.calls++;
    else c['other (arithmetic etc.)']++;
  }
  return c;
}

mkdirSync(outDir, { recursive: true });
const ours = functions(wat(exe));
const theirs = functions(wat(emccWasm));
const ir = execFileSync(wwt, ['ir', exe], { maxBuffer: 256 << 20 }).toString();

for (const want of names) {
  const a = [...ours.keys()].filter((k) => k.includes(want));
  const b = [...theirs.keys()].filter((k) => k.includes(want));
  if (!a.length && !b.length) {
    console.log(`\n${want}: no such function in either module`);
    continue;
  }
  const cols = [...a.map((k) => ['wwt', k, ours.get(k)]), ...b.map((k) => ['emcc', k, theirs.get(k)])];
  const counts = cols.map(([, , body]) => count(body));
  console.log(`\n== ${want}`);
  const w = Math.max(...cols.map(([t, k]) => `${t} ${k}`.length), 8);
  console.log(`${''.padEnd(26)}${cols.map(([t, k]) => `${t} ${k}`.padStart(w + 2)).join('')}`);
  for (const key of Object.keys(counts[0])) {
    console.log(`${key.padEnd(26)}${counts.map((c) => String(c[key]).padStart(w + 2)).join('')}`);
  }
  for (const [t, k, body] of cols) {
    const file = join(outDir, `${k.replace(/[^\w@.-]/g, '_')}.${t}.wat`);
    writeFileSync(file, body.join('\n') + '\n');
    console.log(`  ${t} listing: ${file}`);
  }
  // x86 and IR of the translated functions.
  for (const k of a) {
    const addr = k.match(/@([0-9a-f]+)$|^x86_([0-9a-f]+)$/);
    if (!addr) continue;
    const hex = addr[1] ?? addr[2];
    const start = ir.indexOf(`function 0x${hex}:`);
    const end = ir.indexOf('\nfunction ', start + 1);
    if (start >= 0) {
      const file = join(outDir, `${k.replace(/[^\w@.-]/g, '_')}.ir`);
      writeFileSync(file, ir.slice(start, end < 0 ? undefined : end) + '\n');
      console.log(`  IR: ${file}`);
    }
    const dis = spawnSync('i686-w64-mingw32-objdump', ['-d', '--no-show-raw-insn', `--disassemble=${k.split('@')[0]}`, exe], {
      encoding: 'utf8',
    });
    if (dis.status === 0 && dis.stdout) {
      const file = join(outDir, `${k.replace(/[^\w@.-]/g, '_')}.x86.s`);
      writeFileSync(file, dis.stdout);
      console.log(`  x86: ${file}`);
    }
  }
}
