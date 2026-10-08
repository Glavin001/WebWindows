// Emscripten's thread-state helper (pulled in by -sSHARED_MEMORY) refers to
// this flag, which only builds with wasm workers define. This module runs
// on the thread that loads it.
var ENVIRONMENT_IS_WASM_WORKER = false;

// Pointers from C: negative i32s above 2 GB on wasm32, BigInts on wasm64.
// ptr() makes either an address (a Number).
function ptr(p) {
  return typeof p === 'bigint' ? Number(p) : p >>> 0;
}
