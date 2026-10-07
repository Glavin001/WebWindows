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
  return base;
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
