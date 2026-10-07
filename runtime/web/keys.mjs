// Browser key codes (KeyboardEvent.code, the physical key) to Windows
// virtual-key codes and scan codes (set 1), as a US keyboard reports them.
// Win32u turns them into characters with its keyboard layout.

const KEYEVENTF_EXTENDEDKEY = 1;

/** code -> [virtual key, scan code, extended] */
const KEYS = {
  Escape: [0x1b, 0x01], Backspace: [0x08, 0x0e], Tab: [0x09, 0x0f], Enter: [0x0d, 0x1c], Space: [0x20, 0x39],
  ShiftLeft: [0xa0, 0x2a], ShiftRight: [0xa1, 0x36], ControlLeft: [0xa2, 0x1d], ControlRight: [0xa3, 0x1d, 1],
  AltLeft: [0xa4, 0x38], AltRight: [0xa5, 0x38, 1], MetaLeft: [0x5b, 0x5b, 1], MetaRight: [0x5c, 0x5c, 1],
  ContextMenu: [0x5d, 0x5d, 1], CapsLock: [0x14, 0x3a], NumLock: [0x90, 0x45, 1], ScrollLock: [0x91, 0x46],
  Pause: [0x13, 0x45], PrintScreen: [0x2c, 0x37, 1],
  Insert: [0x2d, 0x52, 1], Delete: [0x2e, 0x53, 1], Home: [0x24, 0x47, 1], End: [0x23, 0x4f, 1],
  PageUp: [0x21, 0x49, 1], PageDown: [0x22, 0x51, 1],
  ArrowLeft: [0x25, 0x4b, 1], ArrowUp: [0x26, 0x48, 1], ArrowRight: [0x27, 0x4d, 1], ArrowDown: [0x28, 0x50, 1],
  Minus: [0xbd, 0x0c], Equal: [0xbb, 0x0d], BracketLeft: [0xdb, 0x1a], BracketRight: [0xdd, 0x1b],
  Backslash: [0xdc, 0x2b], Semicolon: [0xba, 0x27], Quote: [0xde, 0x28], Backquote: [0xc0, 0x29],
  Comma: [0xbc, 0x33], Period: [0xbe, 0x34], Slash: [0xbf, 0x35], IntlBackslash: [0xe2, 0x56],
  NumpadDivide: [0x6f, 0x35, 1], NumpadMultiply: [0x6a, 0x37], NumpadSubtract: [0x6d, 0x4a],
  NumpadAdd: [0x6b, 0x4e], NumpadEnter: [0x0d, 0x1c, 1], NumpadDecimal: [0x6e, 0x53],
};
'QWERTYUIOP'.split('').forEach((c, i) => (KEYS[`Key${c}`] = [c.charCodeAt(0), 0x10 + i]));
'ASDFGHJKL'.split('').forEach((c, i) => (KEYS[`Key${c}`] = [c.charCodeAt(0), 0x1e + i]));
'ZXCVBNM'.split('').forEach((c, i) => (KEYS[`Key${c}`] = [c.charCodeAt(0), 0x2c + i]));
'1234567890'.split('').forEach((c, i) => (KEYS[`Digit${c}`] = [c.charCodeAt(0), 0x02 + i]));
for (let i = 0; i < 12; i++) KEYS[`F${i + 1}`] = [0x70 + i, i < 10 ? 0x3b + i : 0x57 + i - 10];
[0x52, 0x4f, 0x50, 0x51, 0x4b, 0x4c, 0x4d, 0x47, 0x48, 0x49].forEach((scan, i) => (KEYS[`Numpad${i}`] = [0x60 + i, scan]));

/** The key for a KeyboardEvent: {vk, scan, flags}, or null when Windows has no such key. */
export function windowsKey(code) {
  const k = KEYS[code];
  return k ? { vk: k[0], scan: k[1], flags: k[2] ? KEYEVENTF_EXTENDEDKEY : 0 } : null;
}

export const KEYEVENTF_KEYUP = 2;
