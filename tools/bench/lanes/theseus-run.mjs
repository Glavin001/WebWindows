// Runs a Theseus wasm build of a console program in Node, as its web
// worker does: shared memory, console_write to stdout, then main().
import { readFileSync } from 'node:fs';
const dir = new URL('.', import.meta.url);
const name = process.argv[2] ?? 'coremark';
const memory = new WebAssembly.Memory({ initial: (2 << 20) / 65536, maximum: (1024 << 20) / 65536, shared: true });
globalThis.send_to_host = (func, args) => {
  if (func === 'console_write') {
    const [ptr, len] = args;
    process.stdout.write(new Uint8Array(memory.buffer, ptr, len).slice());
  } else {
    process.stderr.write(`[host] ${func}\n`);
  }
};
const exe = await import(new URL(`${name}.js`, dir));
await exe.default({ module_or_path: readFileSync(new URL(`${name}_bg.wasm`, dir)), memory });
exe.main();
