// The exports of opengl32.dll on WebGL 2: those of Wine's opengl32.spec that
// gl4es (its static library's symbols, from nm) or wgl.c implement.
//
//   node native/opengl32-webgl/gen-def.mjs opengl32.spec wgl.c gl4es-symbols.txt > opengl32.def
import { readFileSync } from 'node:fs';

const [spec, wgl, symbols] = process.argv.slice(2);
const have = new Set();
for (const m of readFileSync(symbols, 'latin1').matchAll(/ T _(\w+?)(?:@\d+)?$/gm)) have.add(m[1]);
for (const m of readFileSync(wgl, 'latin1').matchAll(/\bWINAPI\s+(wgl\w+)\s*\(/g)) have.add(m[1]);
const names = [...readFileSync(spec, 'latin1').matchAll(/^@\s+stdcall\s+(\w+)\(/gm)].map((m) => m[1]);
const missing = names.filter((n) => !have.has(n));
if (missing.length) console.error(`opengl32: not exported (no implementation): ${missing.join(' ')}`);
process.stdout.write(`LIBRARY opengl32.dll\nEXPORTS\n${names.filter((n) => have.has(n)).map((n) => `    ${n}\n`).join('')}`);
