// COM classes and other registry entries, installed the way wineboot does
// when it runs each DLL's DllRegisterServer: Wine's DLLs carry their
// registrations as WINE_REGISTRY resources, ATL registrar scripts such as
//
//   HKCR { NoRemove CLSID { '{BCDE0395-...}' = s 'MMDeviceEnumerator class'
//          { InprocServer32 = s '%MODULE%' { val ThreadingModel = s 'Both' } } } }
//
// The host reads them from the system DLLs at start-up and writes the keys
// through Wine's own registry calls (Wine's Unix side, in-process
// wineserver), so CoCreateInstance finds classes such as mmdevapi's device
// enumerator, which winmm and DirectSound need.

import { parsePe } from './host.mjs';

const ROOTS = {
  HKCR: '\\Registry\\Machine\\Software\\Classes',
  HKEY_CLASSES_ROOT: '\\Registry\\Machine\\Software\\Classes',
  HKLM: '\\Registry\\Machine',
  HKEY_LOCAL_MACHINE: '\\Registry\\Machine',
};

/** The WINE_REGISTRY resources of a PE image, as text. */
export function registryScripts(bytes) {
  const info = parsePe(bytes);
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const opt = dv.getUint32(0x3c, true) + 24;
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
    if (type.id !== 'WINE_REGISTRY' || type.sub === null) continue;
    for (const name of entries(type.sub)) {
      if (name.sub === null) continue;
      for (const lang of entries(name.sub)) {
        const d = root + lang.data;
        const at = off(dv.getUint32(d, true));
        if (at >= 0) out.push(new TextDecoder('latin1').decode(bytes.subarray(at, at + dv.getUint32(d + 4, true))));
      }
    }
  }
  return out;
}

/** Tokens of a registrar script: words, quoted strings, braces and '='. */
function tokens(text) {
  const out = [];
  let i = 0;
  while (i < text.length) {
    const c = text[i];
    if (/\s/.test(c) || c === '\0') i++;
    else if (c === '{' || c === '}' || c === '=') out.push(text[i++]);
    else if (c === "'") {
      let s = '';
      i++;
      while (i < text.length) {
        if (text[i] === "'" && text[i + 1] === "'") (s += "'"), (i += 2);
        else if (text[i] === "'") break;
        else s += text[i++];
      }
      i++;
      out.push({ str: s });
    } else {
      let w = '';
      while (i < text.length && !/[\s{}=']/.test(text[i])) w += text[i++];
      out.push(w);
    }
  }
  return out;
}

/**
 * The keys and values a script sets: [{path, values: [{name, type, data}]}],
 * with %MODULE% and %SystemRoot% replaced.
 */
export function parseScript(text, replace) {
  const t = tokens(text);
  let i = 0;
  const word = (x) => (typeof x === 'object' ? x.str : x);
  const subst = (s) => s.replace(/%(\w+)%/g, (m, k) => replace[k] ?? m);
  const keys = [];
  const value = () => {
    // s 'text' | d 'number' | d number
    const type = t[i++];
    const v = word(t[i++]);
    if (type === 's' || type === 'e') return { type: 1, data: subst(v) };
    if (type === 'd') return { type: 4, data: Number(v) >>> 0 };
    return null;
  };
  const block = (path) => {
    // items until '}'
    while (i < t.length && t[i] !== '}') {
      let w = t[i];
      if (w === 'NoRemove' || w === 'ForceRemove') w = t[++i];
      if (w === 'Delete') {
        i++;
        continue;
      }
      if (w === 'val') {
        const name = subst(word(t[++i]));
        i++;
        if (t[i] === '=') i++;
        const v = value();
        if (v) keys.push({ path, values: [{ name, ...v }] });
        continue;
      }
      const name = subst(word(w));
      i++;
      const key = `${path}\\${name}`;
      keys.push({ path: key, values: [] });
      if (t[i] === '=') {
        i++;
        const v = value();
        if (v) keys.push({ path: key, values: [{ name: '', ...v }] });
      }
      if (t[i] === '{') {
        i++;
        block(key);
        i++; // '}'
      }
    }
  };
  while (i < t.length) {
    let w = t[i];
    if (w === 'NoRemove' || w === 'ForceRemove') w = t[++i];
    const root = ROOTS[word(w)];
    i++;
    if (t[i] !== '{') break;
    i++;
    if (root) block(root);
    else {
      // A root this host does not keep (HKCU): skip its block.
      let depth = 1;
      while (i < t.length && depth) depth += t[i] === '{' ? 1 : t[i] === '}' ? -1 : 0, i++;
      continue;
    }
    i++; // '}'
  }
  return keys;
}

/** Writes the registrations of every system DLL that has some; returns how many keys. */
export function installRegistrations(h, files) {
  const M = h.unix.M;
  const call = (name, ...args) => h.unix.syscalls.get(name)(...args) >>> 0;
  const mem = M._malloc(0x10000) >>> 0;
  const enc = (at, s) => {
    for (let k = 0; k < s.length; k++) h.m.u16[(at >>> 1) + k] = s.charCodeAt(k);
    h.m.u16[(at >>> 1) + s.length] = 0;
    return s.length * 2;
  };
  // Scratch layout: [0] handle, [8] UNICODE_STRING, [16] OBJECT_ATTRIBUTES,
  // [64] name text, [0x8000] value data.
  const ustr = (s, buf) => {
    const n = enc(buf, s);
    h.w32(mem + 8, n | ((n + 2) << 16));
    h.w32(mem + 12, buf);
    return mem + 8;
  };
  // Creates a key and the keys above it (the registry starts empty);
  // returns an open handle, or 0.
  const made = new Set();
  const createKey = (key) => {
    const parts = key.split('\\');
    let handle = 0;
    for (let n = key.startsWith('\\Registry\\Machine') ? 3 : parts.length; n <= parts.length; n++) {
      const sub = parts.slice(0, n).join('\\');
      if (n < parts.length && made.has(sub.toLowerCase())) continue;
      // OBJECT_ATTRIBUTES {Length, RootDirectory, ObjectName, Attributes, sd, qos}
      const oa = mem + 16;
      h.m.u8.fill(0, oa, oa + 24);
      h.w32(oa, 24);
      h.w32(oa + 8, ustr(sub, mem + 64));
      h.w32(oa + 12, 0x40); // OBJ_CASE_INSENSITIVE
      const st = call('NtCreateKey', mem, 0xf003f, oa, 0, 0, 0, 0);
      if (st) {
        h.log(`registry: creating ${sub} failed: ${st.toString(16)}`);
        return 0;
      }
      made.add(sub.toLowerCase());
      if (n < parts.length) call('NtClose', h.u32(mem));
      else handle = h.u32(mem);
    }
    return handle;
  };
  let count = 0;
  for (const [path, bytes] of files) {
    if (!/^c:\\windows\\system32\\[^\\]+\.(dll|drv|ocx|exe)$/i.test(path)) continue;
    let scripts;
    try {
      scripts = registryScripts(bytes);
    } catch {
      continue;
    }
    if (!scripts.length) continue;
    const module = path.replace(/^c:/i, 'C:').replace(/\\windows\\system32\\/i, '\\windows\\system32\\');
    for (const text of scripts) {
      for (const { path: key, values } of parseScript(text, { MODULE: module, SystemRoot: 'C:\\windows' })) {
        const handle = createKey(key);
        if (!handle) continue;
        for (const v of values) {
          const name = ustr(v.name, mem + 64);
          let size;
          if (v.type === 1) size = enc(mem + 0x8000, v.data) + 2;
          else (h.w32(mem + 0x8000, v.data), (size = 4));
          call('NtSetValueKey', handle, name, 0, v.type, mem + 0x8000, size);
        }
        call('NtClose', handle);
        count++;
      }
    }
  }
  M._free(mem);
  return count;
}
