// Maps a PE image into guest memory using the layout the translator stored
// in the module's metadata (so the runtime never parses PE headers itself).

export function mapImage(machine, exeBytes, image) {
  const bytes = exeBytes instanceof Uint8Array ? exeBytes : new Uint8Array(exeBytes);
  const base = image.image_base;
  const { u8 } = machine;
  if (base + image.size_of_image > machine.guestLimit) {
    throw new Error(`image at ${base.toString(16)} does not fit below the guest limit`);
  }
  u8.fill(0, base, base + image.size_of_image);
  u8.set(bytes.subarray(0, Math.min(image.size_of_headers, bytes.length)), base);
  for (const s of image.sections) {
    const mem = s.virtual_size || s.raw_size;
    const n = Math.min(s.raw_size, mem, Math.max(0, bytes.length - s.raw_offset));
    if (n > 0) u8.set(bytes.subarray(s.raw_offset, s.raw_offset + n), base + s.virtual_address);
  }
  // 64-bit images preferred above 4 GB are translated for a base below it.
  const preferred = BigInt(image.preferred_base ?? base);
  if (preferred !== BigInt(base)) applyRelocations(machine, bytes, base, preferred);
  return base;
}

/**
 * Applies the image's base relocations (HIGHLOW and DIR64) for loading at
 * `base` instead of `preferred`.
 */
export function applyRelocations(machine, bytes, base, preferred) {
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const pe = dv.getUint32(0x3c, true);
  const opt = pe + 24;
  const wide = dv.getUint16(opt, true) === 0x20b;
  const dirs = opt + (wide ? 112 : 96);
  const relocRva = dv.getUint32(dirs + 5 * 8, true);
  const relocSize = dv.getUint32(dirs + 5 * 8 + 4, true);
  if (!relocRva) return;
  const delta = BigInt(base) - preferred;
  const m = machine.dv;
  // The relocation table, read from the mapped image.
  let off = 0;
  while (off + 8 <= relocSize) {
    const page = m.getUint32(base + relocRva + off, true);
    const size = m.getUint32(base + relocRva + off + 4, true);
    if (size < 8) break;
    for (let i = 0; i < (size - 8) / 2; i++) {
      const e = m.getUint16(base + relocRva + off + 8 + i * 2, true);
      const at = base + page + (e & 0xfff);
      if (e >> 12 === 3) m.setUint32(at, Number(BigInt.asUintN(32, BigInt(m.getUint32(at, true)) + delta)), true);
      else if (e >> 12 === 10) m.setBigUint64(at, BigInt.asUintN(64, m.getBigUint64(at, true) + delta), true);
    }
    off += size;
  }
}

/** The PE machine field: 0x14c (i386) or 0x8664 (AMD64); 0 if not a PE. */
export function peMachine(bytes) {
  const b = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
  if (b.length < 0x40 || b[0] !== 0x4d || b[1] !== 0x5a) return 0;
  const dv = new DataView(b.buffer, b.byteOffset, b.byteLength);
  const pe = dv.getUint32(0x3c, true);
  if (pe + 6 > b.length || dv.getUint32(pe, true) !== 0x4550) return 0;
  return dv.getUint16(pe + 4, true);
}

/** 'x64' for an AMD64 image, 'x86' otherwise. */
export function peArch(bytes) {
  return peMachine(bytes) === 0x8664 ? 'x64' : 'x86';
}
