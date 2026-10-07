// Side-by-side assemblies, installed the way wineboot does
// (dlls/setupapi/fakedll.c): a DLL that carries a WINE_MANIFEST resource is
// an assembly. Its manifest goes to C:\windows\winsxs\manifests and its
// files to C:\windows\winsxs\<assembly>\. Wine's comctl32_v6.dll, for one,
// becomes Microsoft.Windows.Common-Controls 6.0. Programs whose manifests
// ask for it (Notepad does) get its window classes (Edit, Button, ...).

import { parsePe } from './host.mjs';

const RT_MANIFEST = 24;

/** The WINE_MANIFEST resources of a PE image, as text. */
export function wineManifests(bytes) {
  const info = parsePe(bytes);
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const opt = dv.getUint32(0x3c, true) + 24;
  // Data directory 2 (resources); the optional header's directories start at 96.
  const rva = dv.getUint32(opt + 96 + 2 * 8, true);
  if (!rva) return [];
  const off = (r) => {
    const s = info.sections.find((s) => r >= s.virtual_address && r < s.virtual_address + Math.max(s.virtual_size, s.raw_size));
    return s ? r - s.virtual_address + s.raw_offset : -1;
  };
  const root = off(rva);
  if (root < 0) return [];
  const entries = (dir) => {
    const n = dv.getUint16(root + dir + 12, true) + dv.getUint16(root + dir + 14, true);
    return Array.from({ length: n }, (_, i) => {
      const e = root + dir + 16 + i * 8;
      const name = dv.getUint32(e, true);
      const data = dv.getUint32(e + 4, true);
      let id = name;
      if (name & 0x80000000) {
        const p = root + (name & 0x7fffffff);
        id = '';
        for (let j = 0; j < dv.getUint16(p, true); j++) id += String.fromCharCode(dv.getUint16(p + 2 + j * 2, true));
      }
      return { id, sub: data & 0x80000000 ? data & 0x7fffffff : null, data: data & 0x7fffffff };
    });
  };
  const out = [];
  for (const type of entries(0)) {
    if (type.id !== RT_MANIFEST || type.sub === null) continue;
    for (const name of entries(type.sub)) {
      if (typeof name.id !== 'string' || !name.id.startsWith('WINE_MANIFEST') || name.sub === null) continue;
      for (const lang of entries(name.sub)) {
        const d = root + lang.data;
        const at = off(dv.getUint32(d, true));
        if (at >= 0) out.push(new TextDecoder().decode(bytes.subarray(at, at + dv.getUint32(d + 4, true))));
      }
    }
  }
  return out;
}

/** <arch>_<name>_<key>_<version>_<lang>_deadbeef, as fakedll.c names assemblies. */
function assemblyDir({ arch, name, key, version, lang }) {
  const part = (s, max) => {
    s = s.toLowerCase();
    if (s.length > max) {
      const pos = max >> 1;
      s = s.slice(0, pos - 1) + '..' + s.slice(s.length - pos + 1);
    }
    return s + '_';
  };
  return `${part(arch, 16)}${part(name, 40)}${key}_${version}_${part(lang, 8)}deadbeef`;
}

/**
 * Adds the assemblies found in the DLLs of `files` (DOS path -> bytes) to
 * the virtual C: drive, as wineboot would have.
 */
export function installAssemblies(files) {
  const sys32 = 'c:\\windows\\system32\\';
  for (const [path, bytes] of [...files]) {
    if (!path.startsWith(sys32) || !path.endsWith('.dll')) continue;
    let manifests;
    try {
      manifests = wineManifests(bytes);
    } catch {
      continue;
    }
    for (let text of manifests) {
      const ident = text.match(/<assemblyIdentity\b[^>]*>/)?.[0];
      if (!ident) continue;
      const attr = (n) => ident.match(new RegExp(`\\b${n}="([^"]*)"`))?.[1];
      const a = { name: attr('name'), version: attr('version'), arch: attr('processorArchitecture'), key: attr('publicKeyToken'), lang: attr('language') ?? 'none' };
      if (!a.name || !a.version || a.arch === undefined || !a.key) continue;
      if (!a.arch) {
        // "fixup the architecture" as fakedll.c does.
        a.arch = 'x86';
        text = text.replace(/processorArchitecture=""/, 'processorArchitecture="x86"');
      }
      const dir = assemblyDir(a);
      files.set(`c:\\windows\\winsxs\\manifests\\${dir}.manifest`, new TextEncoder().encode(text));
      // The assembly's files: this DLL when it is the only one (installed
      // under the name the manifest gives it), else the DLLs of those names.
      const names = [...text.matchAll(/<file\b[^>]*\bname="([^"]+)"/g)].map((m) => m[1].toLowerCase());
      for (const file of names) {
        const src = names.length === 1 ? bytes : files.get(sys32 + file);
        if (src) files.set(`c:\\windows\\winsxs\\${dir}\\${file}`, src);
      }
    }
  }
}
