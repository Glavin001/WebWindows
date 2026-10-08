// ntdll's heap as native WebAssembly (crates/wwt-heap). ntdll translated
// with `wwt translate --native-heap` imports RtlAllocateHeap and the rest of
// the heap functions (wwt::builtin::NATIVE_HEAP) instead of running Wine's
// own heap; this module provides them. It works in the machine's memory:
// heaps live in guest memory, their regions come from the Wine host's
// virtual memory, and its few process-wide values sit in the null region.

/** The translator option that makes ntdll use the native heap. */
export const NATIVE_HEAP_FLAG = '--native-heap';

/**
 * Compiles the heap module (wwt_heap.wasm). The Rust toolchain links it
 * against a plain imported memory; the machine's memory is shared, so the
 * import is rewritten to match (shared, up to 4 GB) first.
 */
export function compileNativeHeap(bytes) {
  return new WebAssembly.Module(shareMemoryImport(new Uint8Array(bytes)));
}

/**
 * Instantiates the heap for a Wine host and offers its functions to the
 * translated modules the machine loads from now on. `init` must run before
 * ntdll does.
 * @param {import('../runtime.mjs').Machine} machine
 * @param {WebAssembly.Module} module  from compileNativeHeap
 * @param {import('./vm.mjs').VirtualMemory} vm
 */
export function attachNativeHeap(machine, module, vm) {
  const instance = new WebAssembly.Instance(module, {
    env: {
      memory: machine.memory,
      heap_vm_alloc: (size, prot) => {
        const base = vm.reserve(0, size >>> 0, { prot, name: 'heap' });
        if (base) vm.commit(base, size >>> 0, prot);
        return base;
      },
      heap_vm_free: (base) => {
        vm.release(base >>> 0);
      },
    },
  });
  const x = instance.exports;
  if (x.__data_end.value > 0x8000) throw new Error('wwt_heap: its stack and data overlap its state at 0x8000');
  for (const [name, f] of Object.entries(x)) {
    if (name.startsWith('Rtl') || name === '_heap_thread_detach') machine.natives[name] = f;
  }
  return {
    /** ntdll is mapped: where RtlRaiseStatus is (HEAP_GENERATE_EXCEPTIONS). */
    init: (raiseStatus) => x.wwt_heap_init(raiseStatus, machine.guestLimit),
  };
}

function readLeb(b, p) {
  let v = 0;
  let shift = 0;
  for (;;) {
    const c = b[p++];
    v += (c & 0x7f) * 2 ** shift;
    shift += 7;
    if (!(c & 0x80)) return [v, p];
  }
}

function leb(v) {
  const out = [];
  do {
    let c = v % 128;
    v = Math.floor(v / 128);
    if (v) c |= 0x80;
    out.push(c);
  } while (v);
  return out;
}

/**
 * The module with its memory import made shared, 1 to 65536 pages (Rust
 * modules linked against a plain imported memory, for the machine's).
 */
export function shareMemoryImport(b) {
  let p = 8;
  while (p < b.length) {
    const id = b[p];
    const [size, body] = readLeb(b, p + 1);
    const end = body + size;
    if (id === 2) {
      let [count, q] = readLeb(b, body);
      const out = [...leb(count)];
      let from = q;
      for (; count > 0; count--) {
        for (let s = 0; s < 2; s++) {
          const [n, at] = readLeb(b, q);
          q = at + n;
        }
        const kind = b[q++];
        if (kind === 2) {
          out.push(...b.subarray(from, q), 0x03, ...leb(1), ...leb(65536));
          const flags = b[q++];
          q = readLeb(b, q)[1];
          if (flags & 1) q = readLeb(b, q)[1];
          from = q;
          continue;
        }
        if (kind === 0) q = readLeb(b, q)[1];
        else if (kind === 1) {
          const flags = b[q + 1];
          q = readLeb(b, q + 2)[1];
          if (flags & 1) q = readLeb(b, q)[1];
        } else if (kind === 3) q += 2;
        else throw new Error(`native module: unexpected import kind ${kind}`);
      }
      out.push(...b.subarray(from, end));
      return new Uint8Array([...b.subarray(0, p), 2, ...leb(out.length), ...out, ...b.subarray(end)]);
    }
    p = end;
  }
  throw new Error('native module: no memory import');
}
