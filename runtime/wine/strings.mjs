// Wine's hot string and locale functions as native WebAssembly
// (crates/wwt-strings). Wine's DLLs translated with
// `wwt translate --native-strings` try these first (kernelbase's
// CompareStringEx, the C string functions of ntdll, msvcrt and ucrtbase,
// TlsGetValue and the C runtime's per-thread data; wwt::builtin::NATIVE_TRY),
// and run their translated code when one declines. The module works in the machine's memory: its data and stack in
// the guest's null region, its scratch memory in the native region above the
// guest limit.

import { shareMemoryImport } from './heap.mjs';

/** The translator option that makes Wine's DLLs try the native functions. */
export const NATIVE_STRINGS_FLAG = '--native-strings';

/** The DLLs that have functions to try (the flag changes nothing else). */
export const NATIVE_STRINGS_DLLS = ['ntdll.dll', 'kernel32.dll', 'kernelbase.dll', 'msvcrt.dll', 'ucrtbase.dll'];

/** Scratch memory for sort keys: two strings of up to ~9,000 characters. */
const SCRATCH = 256 << 10;

/** Compiles the module (wwt_strings.wasm), its memory import made shared. */
export function compileNativeStrings(bytes) {
  return new WebAssembly.Module(shareMemoryImport(new Uint8Array(bytes)));
}

/**
 * Instantiates the module in the machine and offers its functions to the
 * translated modules the machine loads from now on.
 * @param {import('../runtime.mjs').Machine} machine  initialized
 * @param {WebAssembly.Module} module  from compileNativeStrings
 */
export function attachNativeStrings(machine, module) {
  const instance = new WebAssembly.Instance(module, { env: { memory: machine.memory } });
  const x = instance.exports;
  // Data from 0x9000 up, then the stack: above the native heap's state
  // (0x8000) and the module's own (0x8100), below the end of the null region.
  if (x.__heap_base.value > 0x10000) throw new Error('wwt_strings: its data and stack exceed the null region');
  for (const [name, f] of Object.entries(x)) {
    if (!name.startsWith('__') && !name.startsWith('wwt_')) machine.natives[name] = f;
  }
  x.wwt_strings_init(machine.guestLimit, machine.nativeAlloc(SCRATCH, 0x10000), SCRATCH);
}
