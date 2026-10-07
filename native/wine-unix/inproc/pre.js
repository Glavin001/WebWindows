// Emscripten's thread-state helper (pulled in by -sSHARED_MEMORY) refers to
// this flag, which only builds with wasm workers define. This module runs
// on the thread that loads it.
var ENVIRONMENT_IS_WASM_WORKER = false;
