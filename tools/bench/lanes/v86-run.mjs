// Boots a multiboot kernel on v86 in Node and times it from the host:
// the serial lines "@@START" and "@@STOP" bracket the measured part.
//   V86_DIR=<npm install dir> V86_BIOS=<v86 repo>/bios node v86-run.mjs kernel
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
const [kernel] = process.argv.slice(2);
const dir = process.env.V86_DIR;
const { V86 } = await import(pathToFileURL(createRequire(join(dir, 'x.js')).resolve('v86')));
const bios = pathToFileURL(process.env.V86_BIOS + '/');
const buf = (f) => { const b = readFileSync(f); return b.buffer.slice(b.byteOffset, b.byteOffset + b.byteLength); };
const emulator = new V86({
  wasm_path: join(dir, 'node_modules/v86/build/v86.wasm'),
  bios: { buffer: buf(new URL('seabios.bin', bios)) },
  vga_bios: { buffer: buf(new URL('vgabios.bin', bios)) },
  multiboot: { buffer: buf(kernel) },
  memory_size: 64 << 20,
  vga_memory_size: 2 << 20,
  autostart: true,
  disable_keyboard: true,
  disable_mouse: true,
  disable_speaker: true,
});
let line = '';
let t0 = 0;
const boot = performance.now();
emulator.add_listener('serial0-output-byte', (b) => {
  const c = String.fromCharCode(b);
  if (c !== '\n') { line += c; return; }
  if (line === '@@START') t0 = performance.now();
  else if (line === '@@STOP') console.log(`host time (ms): ${(performance.now() - t0).toFixed(0)}`);
  else if (line === '@@END') { emulator.destroy(); process.exit(0); }
  else console.log(line);
  line = '';
});
setTimeout(() => { console.error(`timeout; boot+run ${(performance.now() - boot).toFixed(0)} ms`); process.exit(1); }, 30 * 60 * 1000);
