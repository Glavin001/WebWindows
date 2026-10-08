// Import thunks resolved in the address lookup.
//
// Wine's kernel32 exports many functions as one-instruction thunks into
// kernelbase or ntdll (`mov edi, edi; push ebp; mov ebp, esp; pop ebp;
// jmp [import]`), and MinGW programs call their imports through stubs
// (`jmp [import]`). Translated, each is a function of its own: a call
// through one costs a second call, with the CPU state written back and
// reloaded around it. Once the import slot is filled, the thunk does
// nothing but continue at the slot's target with the same stack, so its
// lookup entry can name the target's translation directly.
//
// This stays correct when the thunk's code is patched (a hot-patch hook
// writing `jmp` over `mov edi, edi`): code writes clear the page's lookup
// entries, and the next call there translates the new code. The entries are
// refreshed whenever the loader restores an image's protection after
// filling its import table (NtProtectVirtualMemory), so a slot the loader
// rewrites is followed; a slot a program rewrites later is not.

const MOV_EDI_PROLOGUE = [0x8b, 0xff, 0x55, 0x8b, 0xec, 0x5d];

/** The import slot `addr` jumps through, if it is a thunk. */
function thunkSlot(u8, dv, addr) {
  let p = addr;
  if (MOV_EDI_PROLOGUE.every((b, i) => u8[p + i] === b)) p += MOV_EDI_PROLOGUE.length;
  if (u8[p] !== 0xff || u8[p + 1] !== 0x25) return 0;
  return dv.getUint32(p + 2, true);
}

/** Points the lookup entries of the thunks in [lo, hi) at their targets. */
export function aliasImportThunks(m, lo, hi) {
  let n = 0;
  for (const a of m.entriesIn(lo, hi)) {
    const slot = thunkSlot(m.u8, m.dv, a);
    if (!slot || slot < lo || slot + 4 > hi) continue;
    const target = m.dv.getUint32(slot, true);
    const index = target && target !== a ? m.lookup(target) : 0;
    if (index && m.lookup(a) !== index) {
      m.register(a, index);
      n++;
    }
  }
  return n;
}
