// The registry a Wine prefix starts with: wineboot installs wine.inf's
// DefaultInstall section (DefaultInstall.ntx86 for an i386 prefix), whose
// AddReg sections write the standard keys every Windows has (the Windows
// version in HKLM\Software\Microsoft\Windows NT\CurrentVersion, fonts, OLE,
// sessions, services...). The runtime has no wineboot, so the bundle reads
// the same sections from Wine's own wine.inf (runtime/node/wine-bundle.mjs)
// and the host writes them at start-up (./registry-setup.mjs), before the
// DLLs' registrations, as wineboot does.
//
// This follows setupapi's INF parsing and AddReg semantics (Wine's
// dlls/setupapi/parser.c and install.c): sections, line continuations,
// quoted fields, %strings% and directory ids, Needs= and the value types of
// FLG_ADDREG_* flags. Only registry operations: copying files and
// registering DLLs are done elsewhere.

// FLG_ADDREG_* (setupapi.h)
const BINVALUETYPE = 0x1;
const NOCLOBBER = 0x2;
const DELVAL = 0x4;
const APPEND = 0x8;
const KEYONLY = 0x10;
const TYPE_MASK = 0xffff0000 | BINVALUETYPE;
const TYPES = new Map([
  [0x00000000, 1], // TYPE_SZ: REG_SZ
  [0x00010000, 7], // TYPE_MULTI_SZ
  [0x00020000, 2], // TYPE_EXPAND_SZ
  [0x00000001, 3], // TYPE_BINARY
  [0x00010001, 4], // TYPE_DWORD
  [0x00020001, 0], // TYPE_NONE
]);

/** Directory ids (setupapi's DIRID_*) for a prefix with Windows in C:\windows. */
export const DIRIDS = {
  10: 'C:\\windows',
  11: 'C:\\windows\\system32',
  12: 'C:\\windows\\system32\\drivers',
  17: 'C:\\windows\\inf',
  18: 'C:\\windows\\help',
  20: 'C:\\windows\\Fonts',
  24: 'C:\\',
  25: 'C:\\windows',
  30: 'C:\\',
  50: 'C:\\windows\\system',
  51: 'C:\\windows\\system32\\spool',
  52: 'C:\\windows\\system32\\spool\\drivers',
  53: 'C:\\users\\Public',
  55: 'C:\\windows\\system32\\spool\\prtprocs',
  16422: 'C:\\Program Files',
  16424: 'C:\\windows\\system32',
  16425: 'C:\\windows\\system32',
  16426: 'C:\\Program Files',
  16427: 'C:\\Program Files\\Common Files',
  16428: 'C:\\Program Files\\Common Files',
};

/** An INF file as sections of lines, each line its fields (setupapi's parser). */
export function parseInf(text) {
  const sections = new Map();
  let cur = null;
  // Join continuation lines ("\" at the end, outside quotes).
  const lines = [];
  let acc = '';
  for (const raw of text.split(/\r?\n/)) {
    let line = stripComment(raw);
    if (/\\\s*$/.test(line)) {
      acc += line.replace(/\\\s*$/, '');
      continue;
    }
    lines.push(acc + line);
    acc = '';
  }
  if (acc) lines.push(acc);
  for (const line of lines) {
    const t = line.trim();
    if (!t) continue;
    const m = /^\[([^\]]+)\]/.exec(t);
    if (m) {
      cur = m[1].trim().toLowerCase();
      if (!sections.has(cur)) sections.set(cur, []);
      continue;
    }
    if (cur) sections.get(cur).push(splitFields(t));
  }
  return sections;
}

/** Drops a `;` comment that is not inside quotes. */
function stripComment(line) {
  let q = false;
  for (let i = 0; i < line.length; i++) {
    if (line[i] === '"') q = !q;
    else if (line[i] === ';' && !q) return line.slice(0, i);
  }
  return line;
}

/** `key = a, "b, c", d` -> { key, fields: [...] } (quotes removed, "" is a quote). */
function splitFields(line) {
  let key = null;
  let rest = line;
  const eq = findUnquoted(line, '=');
  if (eq >= 0) {
    key = line.slice(0, eq).trim();
    rest = line.slice(eq + 1);
  }
  const fields = [];
  let cur = '';
  let q = false;
  let quoted = false;
  for (let i = 0; i < rest.length; i++) {
    const c = rest[i];
    if (c === '"') {
      if (q && rest[i + 1] === '"') {
        cur += '"';
        i++;
      } else {
        q = !q;
        quoted = true;
      }
    } else if (c === ',' && !q) {
      fields.push(quoted ? cur : cur.trim());
      cur = '';
      quoted = false;
    } else cur += c;
  }
  fields.push(quoted ? cur : cur.trim());
  return { key, fields };
}

function findUnquoted(s, ch) {
  let q = false;
  for (let i = 0; i < s.length; i++) {
    if (s[i] === '"') q = !q;
    else if (s[i] === ch && !q) return i;
  }
  return -1;
}

/** Replaces %name% from [Strings] and %dirid% from DIRIDS (%% is a %). */
function substitute(s, strings, dirids) {
  return s.replace(/%([^%]*)%/g, (all, name) => {
    if (name === '') return '%';
    const k = name.toLowerCase();
    if (strings.has(k)) return strings.get(k);
    if (/^\d+$/.test(name) && dirids[name] !== undefined) return dirids[name];
    return all;
  });
}

const ROOTS = {
  HKLM: '\\Registry\\Machine',
  HKEY_LOCAL_MACHINE: '\\Registry\\Machine',
  HKCR: '\\Registry\\Machine\\Software\\Classes',
  HKEY_CLASSES_ROOT: '\\Registry\\Machine\\Software\\Classes',
  HKU: '\\Registry\\User',
  HKEY_USERS: '\\Registry\\User',
};

/**
 * The registry writes of installing `section` (and the sections it Needs=):
 * [{ key, name, type, data, noclobber }], `key` an NT path (HKCU under
 * `userKey`), `data` a Uint8Array (null for a key alone), in the order
 * setupapi applies them.
 */
export function infRegistryWrites(text, section, { userKey, dirids = DIRIDS } = {}) {
  const inf = parseInf(text);
  const strings = new Map();
  for (const { key, fields } of inf.get('strings') ?? []) if (key) strings.set(key.toLowerCase(), fields.join(','));
  const roots = { ...ROOTS, HKCU: userKey, HKEY_CURRENT_USER: userKey };
  const out = [];
  const done = new Set();
  const install = (name) => {
    const s = name.toLowerCase();
    if (done.has(s) || !inf.has(s)) return;
    done.add(s);
    const lines = inf.get(s);
    const directive = (d) => lines.filter((l) => l.key?.toLowerCase() === d).flatMap((l) => l.fields).filter(Boolean);
    for (const need of directive('needs')) install(need);
    for (const reg of directive('addreg')) addReg(reg);
  };
  const addReg = (name) => {
    for (const { fields } of inf.get(name.toLowerCase()) ?? []) {
      const f = fields.map((x) => substitute(x, strings, dirids));
      const root = roots[f[0]?.toUpperCase()];
      // HKR is the device or service being installed: not wineboot's.
      if (!root) continue;
      const key = f[1] ? `${root}\\${f[1]}` : root;
      const flags = f[3] ? Number(f[3]) >>> 0 : 0;
      // No value and no flags: the key alone, as KEYONLY.
      if ((f.length <= 3 && !f[2]) || flags & (KEYONLY | DELVAL | APPEND)) {
        // A key alone (DELVAL and APPEND do not occur in a fresh install's sections).
        if (!(flags & (DELVAL | APPEND))) out.push({ key, name: null, type: 0, data: null, noclobber: false });
        continue;
      }
      const t = (flags & TYPE_MASK) >>> 0;
      const type = TYPES.has(t) ? TYPES.get(t) : flags >>> 16;
      const name = f[2] ?? '';
      const values = f.slice(4);
      out.push({ key, name, type, data: encode(type, flags, values), noclobber: !!(flags & NOCLOBBER) });
    }
  };
  install(section);
  return out;
}

/** A value's bytes as setupapi stores them. */
function encode(type, flags, values) {
  const utf16 = (s) => {
    const b = new Uint8Array((s.length + 1) * 2);
    for (let i = 0; i < s.length; i++) (b[2 * i] = s.charCodeAt(i) & 0xff), (b[2 * i + 1] = s.charCodeAt(i) >> 8);
    return b;
  };
  if (type === 1 || type === 2) return utf16(values[0] ?? '');
  // A symbolic link's target, without the terminating null.
  if (type === 6) return utf16(values[0] ?? '').slice(0, -2);
  if (type === 7) {
    const parts = values.filter((v) => v !== '').map(utf16);
    const n = parts.reduce((s, p) => s + p.length, 0) + 2;
    const b = new Uint8Array(n);
    let o = 0;
    for (const p of parts) (b.set(p, o), (o += p.length));
    return b;
  }
  if (type === 4 && !(flags & BINVALUETYPE && values.length > 1)) {
    const v = Number(values[0] ?? 0) >>> 0;
    return new Uint8Array([v & 0xff, (v >> 8) & 0xff, (v >> 16) & 0xff, v >>> 24]);
  }
  // Binary (and other types given as bytes): hex byte fields.
  return Uint8Array.from(values.filter((v) => v !== '').map((v) => parseInt(v, 16) & 0xff));
}
