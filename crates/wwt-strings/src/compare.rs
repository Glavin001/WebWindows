//! `compare_string` from Wine's `dlls/kernelbase/locale.c`: linguistic
//! string comparison with the sort tables of `sortdefault.nls`, which
//! kernelbase has loaded (its static `sort`), for the sort of the user's
//! locale (`current_locale_sort`).
//!
//! The two strings' sort keys are built as Wine builds them, character by
//! character: primary weights are compared as they are produced, and the
//! secondary keys (diacritics, case, punctuation, kana extras) afterwards.
//! The code follows Wine's function by function, down to its byte
//! arithmetic and the length limits of each key, so that the results are
//! the same. One quirk is not reproduced: Wine compares the primary keys in
//! their 32-byte static buffers, and a key that outgrows one (a single
//! character with more than 32 bytes of primary weights) is compared from
//! stale bytes; such a comparison is declined.
//!
//! The keys live in scratch memory outside guest memory; strings whose keys
//! do not fit are declined.

use crate::mem::*;

const NORM_IGNORECASE: u32 = 0x1;
const NORM_IGNORENONSPACE: u32 = 0x2;
const NORM_IGNORESYMBOLS: u32 = 0x4;
const SORT_DIGITSASNUMBERS: u32 = 0x8;
const LINGUISTIC_IGNORECASE: u32 = 0x10;
const LINGUISTIC_IGNOREDIACRITIC: u32 = 0x20;
const SORT_STRINGSORT: u32 = 0x1000;
const NORM_IGNOREKANATYPE: u32 = 0x10000;
const NORM_IGNOREWIDTH: u32 = 0x20000;
const NORM_LINGUISTIC_CASING: u32 = 0x800_0000;
const LOCALE_USE_CP_ACP: u32 = 0x4000_0000;

/// The flags `CompareStringEx` accepts (0x10000000 is related to diacritics
/// in Arabic, Japanese and Hebrew, and changes nothing in Wine).
pub const SUPPORTED_FLAGS: u32 = NORM_IGNORECASE
    | NORM_IGNORENONSPACE
    | NORM_IGNORESYMBOLS
    | SORT_STRINGSORT
    | NORM_IGNOREKANATYPE
    | NORM_IGNOREWIDTH
    | NORM_LINGUISTIC_CASING
    | LINGUISTIC_IGNORECASE
    | LINGUISTIC_IGNOREDIACRITIC
    | SORT_DIGITSASNUMBERS
    | 0x1000_0000
    | LOCALE_USE_CP_ACP;

/// Wine's `sort` (32-bit layout).
const SORT_GUID_COUNT: u32 = 4;
const SORT_EXP_COUNT: u32 = 8;
const SORT_COMPR_COUNT: u32 = 12;
const SORT_KEYS: u32 = 16;
const SORT_GUIDS: u32 = 32;
const SORT_EXPANSIONS: u32 = 36;
const SORT_COMPRESSIONS: u32 = 40;
const SORT_COMPR_DATA: u32 = 44;
const SORT_JAMO: u32 = 48;

/// `struct sortguid`.
const SORTGUID_SIZE: u32 = 36;
const SORTGUID_FLAGS: u32 = 16;
const SORTGUID_COMPR: u32 = 20;
const SORTGUID_EXCEPT: u32 = 24;
const SORTGUID_LING_EXCEPT: u32 = 28;

const FLAG_HAS_3_BYTE_WEIGHTS: u32 = 0x01;
const FLAG_REVERSEDIACRITICS: u32 = 0x10;

/// `struct sort_compression`: offset, minchar, maxchar, len[8].
const COMPRESSION_SIZE: u32 = 24;

/// `struct jamo_sort`: is_old, leading, vowel, trailing, weight, pad.
const JAMO_SIZE: u32 = 8;

const CASE_FULLWIDTH: u32 = 0x01;
const CASE_FULLSIZE: u32 = 0x02;
const CASE_SUBSCRIPT: u32 = 0x08;
const CASE_UPPER: u32 = 0x10;
const CASE_KATAKANA: u32 = 0x20;
const CASE_COMPR_2: u32 = 0x40;
const CASE_COMPR_4: u32 = 0x80;
const CASE_COMPR_6: u32 = 0xc0;

const SCRIPT_UNSORTABLE: u32 = 0;
const SCRIPT_NONSPACE_MARK: u32 = 1;
const SCRIPT_EXPANSION: u32 = 2;
const SCRIPT_EASTASIA_SPECIAL: u32 = 3;
const SCRIPT_JAMO_SPECIAL: u32 = 4;
const SCRIPT_EXTENSION_A: u32 = 5;
const SCRIPT_PUNCTUATION: u32 = 6;
const SCRIPT_SYMBOL_6: u32 = 12;
const SCRIPT_DIGIT: u32 = 13;
const SCRIPT_KANA: u32 = 34;
const SCRIPT_HEBREW: u32 = 40;
const SCRIPT_ARABIC: u32 = 41;
const SCRIPT_PUA_FIRST: u32 = 169;
const SCRIPT_PUA_LAST: u32 = 175;
const SCRIPT_CJK_FIRST: u32 = 192;
const SCRIPT_CJK_LAST: u32 = 239;

/// Wine's static buffer for a primary key.
const PRIMARY_SIZE: u32 = 32;

/// Expansions of expansions, before giving up on the table.
const MAX_EXPANSIONS: u32 = 64;

/// `union char_weights`: primary, script, diacritic and case bytes.
#[derive(Clone, Copy)]
struct Weights(u32);

impl Weights {
    #[inline(always)]
    fn primary(self) -> u32 {
        self.0 & 0xff
    }
    #[inline(always)]
    fn script(self) -> u32 {
        self.0 >> 8 & 0xff
    }
    #[inline(always)]
    fn diacritic(self) -> u32 {
        self.0 >> 16 & 0xff
    }
    #[inline(always)]
    fn case(self) -> u32 {
        self.0 >> 24
    }
    #[inline(always)]
    fn set_primary(&mut self, v: u32) {
        self.0 = self.0 & !0xff | v & 0xff;
    }
    #[inline(always)]
    fn set_script(&mut self, v: u32) {
        self.0 = self.0 & !0xff00 | (v & 0xff) << 8;
    }
    #[inline(always)]
    fn set_diacritic(&mut self, v: u32) {
        self.0 = self.0 & !0xff_0000 | (v & 0xff) << 16;
    }
    #[inline(always)]
    fn set_case(&mut self, v: u32) {
        self.0 = self.0 & 0xff_ffff | v << 24;
    }
}

/// `struct sortkey`: a key being built, at most `max` bytes (later bytes are
/// dropped, as in Wine). Only primary keys have a smaller buffer (`size`).
#[derive(Clone, Copy, Default)]
struct Key {
    buf: u32,
    size: u32,
    len: u32,
    max: u32,
    /// Outgrew its buffer: the comparison is declined.
    over: bool,
}

impl Key {
    #[inline(always)]
    fn append(&mut self, v: u32) {
        if self.len >= self.max {
            return;
        }
        if self.len >= self.size {
            self.over = true;
            return;
        }
        st8(self.buf + self.len, v);
        self.len += 1;
    }

    fn byte(&self, i: u32) -> u32 {
        ld8(self.buf + i)
    }
}

/// `struct sortkey_state`.
#[derive(Default)]
struct State {
    primary: Key,
    diacritic: Key,
    case: Key,
    special: Key,
    extra: [Key; 4],
    primary_pos: u32,
}

/// Bump allocation in the scratch memory.
struct Scratch {
    next: u32,
    end: u32,
}

impl Scratch {
    fn key(&mut self, max: u32) -> Option<Key> {
        if max > self.end - self.next {
            return None;
        }
        let k = Key {
            buf: self.next,
            size: max,
            len: 0,
            max,
            over: false,
        };
        self.next += max;
        Some(k)
    }
}

impl State {
    /// `init_sortkey_state`.
    fn new(flags: u32, n: u32, primary_buf: u32, scratch: &mut Scratch) -> Option<State> {
        let mut s = State {
            primary: Key {
                buf: primary_buf,
                size: PRIMARY_SIZE,
                len: 0,
                max: n.checked_mul(8)?,
                over: false,
            },
            ..Default::default()
        };
        let n3 = n.checked_mul(3)?;
        s.case = scratch.key(n3)?;
        s.special = scratch.key(n.checked_mul(4)?)?;
        s.extra[2] = scratch.key(n)?;
        s.extra[3] = scratch.key(n)?;
        if flags & NORM_IGNORENONSPACE == 0 {
            s.diacritic = scratch.key(n3)?;
            s.extra[0] = scratch.key(n)?;
            s.extra[1] = scratch.key(n)?;
        }
        Some(s)
    }
}

/// Character `i` of the string at `src`.
#[inline(always)]
fn ch(src: u32, i: i32) -> u32 {
    ld16(src + 2 * i as u32)
}

struct Cmp {
    keys: u32,
    expansions: u32,
    exp_count: u32,
    compressions: u32,
    compr_count: u32,
    compr_data: u32,
    jamo: u32,
    /// The sort's flags and compression table.
    sort_flags: u32,
    compr: u32,
    flags: u32,
    case_mask: u32,
    except: u32,
    compr_tables: Option<[u32; 8]>,
    /// The tables are not what they should be: decline.
    bad: bool,
}

impl Cmp {
    /// `get_char_weights`.
    #[inline(always)]
    fn weights(&self, c: u32) -> Weights {
        Weights(if self.except != 0 {
            let row = ld(self.keys + 4 * (self.except + (c >> 8)));
            ld(self.keys + 4 * (row.wrapping_add(c & 0xff)))
        } else {
            ld(self.keys + 4 * c)
        })
    }

    /// `append_normal_weights`.
    #[inline(always)]
    fn normal_weights(&self, s: &mut State, mut w: Weights) {
        s.primary.append(w.script());
        s.primary.append(w.primary());
        let script = w.script();
        if (SCRIPT_PUA_FIRST..=SCRIPT_PUA_LAST).contains(&script)
            || (self.sort_flags & FLAG_HAS_3_BYTE_WEIGHTS != 0
                && (SCRIPT_CJK_FIRST..=SCRIPT_CJK_LAST).contains(&script))
        {
            s.primary.append(w.diacritic());
            s.case.append(w.case());
            return;
        }
        if script <= SCRIPT_ARABIC && script != SCRIPT_HEBREW {
            if self.flags & LINGUISTIC_IGNOREDIACRITIC != 0 {
                w.set_diacritic(2);
            }
            if self.flags & LINGUISTIC_IGNORECASE != 0 {
                w.set_case(2);
            }
        }
        s.diacritic.append(w.diacritic());
        s.case.append(w.case());
    }

    /// `append_nonspace_weights`.
    fn nonspace_weights(&self, key: &mut Key, w: Weights) {
        let d = if self.flags & LINGUISTIC_IGNOREDIACRITIC != 0 {
            2
        } else {
            w.diacritic()
        };
        if key.len != 0 {
            let at = key.buf + key.len - 1;
            st8(at, ld8(at) + d);
        } else {
            key.append(d);
        }
    }

    /// `append_expansion_weights`, comparing.
    fn expansion_weights(&self, s: &mut State, w: Weights) {
        match w.script() {
            SCRIPT_UNSORTABLE => {}
            SCRIPT_NONSPACE_MARK => self.nonspace_weights(&mut s.diacritic, w),
            _ => self.normal_weights(s, w),
        }
    }

    /// `find_compression`: the weights after the matching entry of `table`.
    fn find_compression(&self, src: u32, table: u32, count: u32, len: u32) -> Option<u32> {
        let elem = 2 + len + (len & 1);
        let (mut min, mut max) = (0i32, count as i32 - 1);
        while min <= max {
            let pos = (min + max) / 2;
            let res = wcsncmp(src, table + 2 * pos as u32 * elem, len);
            if res == 0 {
                return Some(table + 2 * (pos as u32 + 1) * elem - 4);
            }
            if res > 0 {
                min = pos + 1;
            } else {
                max = pos - 1;
            }
        }
        None
    }

    /// `get_compression_weights`: the number of extra characters used.
    fn compression_weights(&mut self, src: u32, srclen: i32, w: &mut Weights) -> i32 {
        if self.compr >= self.compr_count {
            return 0;
        }
        let compr = self.compressions + self.compr * COMPRESSION_SIZE;
        let size = w.case() & CASE_COMPR_6;
        let mut maxlen: i32 = match size {
            CASE_COMPR_6 => 8,
            CASE_COMPR_4 => 5,
            CASE_COMPR_2 => 3,
            _ => 1,
        };
        maxlen = maxlen.min(srclen);
        let (minchar, maxchar) = (ld16(compr + 4), ld16(compr + 6));
        let mut i = 0;
        while i < maxlen {
            let c = ch(src, i);
            if c < minchar || c > maxchar {
                break;
            }
            i += 1;
        }
        maxlen = i;
        let tables = match self.compr_tables {
            Some(t) => t,
            None => {
                let mut t = [0u32; 8];
                t[0] = self.compr_data + 2 * ld(compr);
                for i in 1..8 {
                    let n = ld16(compr + 8 + 2 * (i as u32 - 1));
                    let size = 2 + (i as u32 + 1) + ((i as u32 + 1) & 1);
                    t[i] = t[i - 1] + 2 * n * size;
                }
                self.compr_tables = Some(t);
                t
            }
        };
        let mut i = maxlen - 2;
        while i >= 0 {
            let count = ld16(compr + 8 + 2 * i as u32);
            if let Some(r) = self.find_compression(src, tables[i as usize], count, i as u32 + 2) {
                *w = Weights(ld(r));
                return i + 1;
            }
            i -= 1;
        }
        0
    }

    /// `append_extra_kana_weights`.
    fn extra_kana_weights(
        &self,
        keys: &mut [Key; 4],
        src: u32,
        mut pos: i32,
        w: &mut Weights,
    ) -> bool {
        let mut extra1 = 3;
        let mut case_weight = w.case();
        if w.primary() <= 1 {
            let mut found = false;
            while pos > 0 {
                pos -= 1;
                let mut prev = self.weights(ch(src, pos));
                let script = prev.script();
                if script == SCRIPT_UNSORTABLE || script == SCRIPT_NONSPACE_MARK {
                    continue;
                }
                if script == SCRIPT_EXPANSION {
                    return false;
                }
                if script != SCRIPT_EASTASIA_SPECIAL {
                    *w = prev;
                    return true;
                }
                if prev.primary() <= 1 {
                    continue;
                }
                case_weight = prev.case() & self.case_mask;
                if w.primary() == 1 {
                    // Prolonged sound mark.
                    prev.set_primary(prev.primary() & 0x87);
                    case_weight &= !CASE_FULLWIDTH;
                    case_weight |= w.case() & CASE_FULLWIDTH;
                }
                extra1 = 4 + w.primary();
                w.set_primary(prev.primary());
                found = true;
                break;
            }
            if !found {
                return false;
            }
        }
        keys[0].append(0xc4 | (case_weight & CASE_FULLSIZE));
        keys[1].append(extra1);
        keys[2].append(0xc4 | (case_weight & CASE_KATAKANA));
        keys[3].append(0xc4 | (case_weight & CASE_FULLWIDTH));
        w.set_script(SCRIPT_KANA);
        true
    }

    /// `append_hangul_weights`: the number of extra characters used.
    fn hangul_weights(&self, key: &mut Key, src: u32, srclen: i32) -> i32 {
        let jamo = |i: i32, field: u32| ld8(self.jamo + i as u32 * JAMO_SIZE + field);
        let (is_old, leading, vowel, trailing, weight) = (0, 1, 2, 3, 4);
        let mut leading_idx: i32 = 0x115f - 0x1100;
        let mut vowel_idx: i32 = 0x1160 - 0x1100;
        let mut trailing_idx: i32 = -1;
        let mut pos = 0;
        let c = ch(src, pos) as i32;
        if (0x1100..=0x115f).contains(&c) {
            leading_idx = c - 0x1100;
            pos += 1;
        } else if (0xa960..=0xa97c).contains(&c) {
            leading_idx = c - (0xa960 - 0x100);
            pos += 1;
        }
        if srclen > pos {
            let c = ch(src, pos) as i32;
            if (0x1160..=0x11a7).contains(&c) {
                vowel_idx = c - 0x1100;
                pos += 1;
            } else if (0xd7b0..=0xd7c6).contains(&c) {
                vowel_idx = c - (0xd7b0 - 0x11d);
                pos += 1;
            }
        }
        if srclen > pos {
            let c = ch(src, pos) as i32;
            if (0x11a8..=0x11ff).contains(&c) {
                trailing_idx = c - 0x1100;
                pos += 1;
            } else if (0xd7cb..=0xd7fb).contains(&c) {
                trailing_idx = c - (0xd7cb - 0x134);
                pos += 1;
            }
        }
        if jamo(leading_idx, is_old) == 0
            && jamo(vowel_idx, is_old) == 0
            && (trailing_idx == -1 || jamo(trailing_idx, is_old) == 0)
        {
            // Not old Hangul: only the leading character; the vowel and
            // trailing ones are the next characters' business.
            pos = 1;
            vowel_idx = 0x1160 - 0x1100;
            trailing_idx = -1;
        }
        let leading_off = jamo(leading_idx, leading).max(jamo(vowel_idx, leading));
        let vowel_off = jamo(leading_idx, vowel).max(jamo(vowel_idx, vowel));
        let mut trailing_off = jamo(leading_idx, trailing).max(jamo(vowel_idx, trailing));
        if trailing_idx != -1 {
            trailing_off = trailing_off.max(jamo(trailing_idx, trailing));
        }
        let mut composed = (0xac00 + (leading_off * 21 + vowel_off) * 28 + trailing_off) & 0xffff;
        let mut filler_mask = 0;
        if leading_idx == 0x115f - 0x1100 || vowel_idx == 0x1160 - 0x1100 {
            filler_mask = 0x80;
            composed = composed.wrapping_sub(1) & 0xffff;
        }
        if composed < 0xac00 {
            composed = 0x3260;
        }
        let w = self.weights(composed);
        key.append(w.script());
        key.append(w.primary());
        key.append(0xff);
        key.append(jamo(leading_idx, weight) | filler_mask);
        key.append(0xff);
        key.append(jamo(vowel_idx, weight));
        key.append(0xff);
        key.append(if trailing_idx != -1 {
            jamo(trailing_idx, weight)
        } else {
            2
        });
        pos - 1
    }

    /// `append_weights`: the number of characters used.
    fn append_weights(&mut self, s: &mut State, src: u32, srclen: i32, pos: i32) -> i32 {
        let mut w = self.weights(ch(src, pos));
        let mut idx = w.0 >> 16 & 0x3fff;
        let mut ret = 1;
        if w.case() & CASE_COMPR_6 != 0 {
            ret += self.compression_weights(src + 2 * pos as u32, srclen - pos, &mut w);
        }
        w.set_case(w.case() & self.case_mask);
        let flags = self.flags;
        match w.script() {
            SCRIPT_UNSORTABLE => {}
            SCRIPT_NONSPACE_MARK => self.nonspace_weights(&mut s.diacritic, w),
            SCRIPT_EXPANSION => {
                let mut n = 0;
                while w.script() == SCRIPT_EXPANSION {
                    n += 1;
                    if idx >= self.exp_count || n > MAX_EXPANSIONS {
                        self.bad = true;
                        return ret;
                    }
                    let e = self.expansions + 4 * idx;
                    w = self.weights(ld16(e));
                    w.set_case(w.case() & self.case_mask);
                    self.expansion_weights(s, w);
                    w = self.weights(ld16(e + 2));
                    idx = w.0 >> 16;
                    w.set_case(w.case() & self.case_mask);
                }
                self.expansion_weights(s, w);
            }
            SCRIPT_EASTASIA_SPECIAL => {
                if !self.extra_kana_weights(&mut s.extra, src, pos, &mut w) {
                    s.primary.append(0xff);
                    s.primary.append(0xff);
                } else {
                    w.set_case(2);
                    self.normal_weights(s, w);
                }
            }
            SCRIPT_JAMO_SPECIAL => {
                ret += self.hangul_weights(&mut s.primary, src + 2 * pos as u32, srclen - pos);
                s.diacritic.append(2);
                s.case.append(2);
            }
            SCRIPT_EXTENSION_A => {
                s.primary.append(0xfd);
                s.primary.append(0xff);
                s.primary.append(w.primary());
                s.primary.append(w.diacritic());
                s.diacritic.append(2);
                s.case.append(2);
            }
            SCRIPT_PUNCTUATION
                if flags & NORM_IGNORESYMBOLS == 0 && flags & SORT_STRINGSORT == 0 =>
            {
                // A position: Wine's `short len`.
                let len = 0u32
                    .wrapping_sub((s.primary.len + s.primary_pos) / 2)
                    .wrapping_sub(1);
                if flags & LINGUISTIC_IGNORECASE != 0 {
                    w.set_case(2);
                }
                if flags & LINGUISTIC_IGNOREDIACRITIC != 0 {
                    w.set_diacritic(2);
                }
                s.special.append(len >> 8);
                s.special.append(len);
                s.special.append(w.primary());
                s.special.append(w.case() | w.diacritic() << 3);
            }
            SCRIPT_PUNCTUATION..=SCRIPT_SYMBOL_6 => {
                if flags & NORM_IGNORESYMBOLS == 0 {
                    s.primary.append(w.script());
                    s.primary.append(w.primary());
                    s.diacritic.append(w.diacritic());
                    s.case.append(w.case());
                }
            }
            SCRIPT_DIGIT if flags & SORT_DIGITSASNUMBERS != 0 && digit_zero(ch(src, pos)) != 0 => {
                ret += digit_weights(&mut s.primary, src + 2 * pos as u32, srclen - pos);
                s.diacritic.append(w.diacritic());
                s.case.append(w.case());
            }
            _ => self.normal_weights(s, w),
        }
        ret
    }

    /// `remove_unneeded_weights`: whether the kana extras count.
    fn remove_unneeded_weights(&self, s: &mut State) -> bool {
        const IGNORE: [u32; 4] = [
            0xc4 | CASE_FULLSIZE,
            0x03,
            0xc4 | CASE_KATAKANA,
            0xc4 | CASE_FULLWIDTH,
        ];
        if self.sort_flags & FLAG_REVERSEDIACRITICS != 0 {
            let k = &s.diacritic;
            for i in 0..k.len / 2 {
                let (a, b) = (k.buf + i, k.buf + k.len - i - 1);
                let t = ld8(b);
                st8(b, ld8(a));
                st8(a, t);
            }
        }
        trim(&mut s.diacritic, |b| b <= 2);
        trim(&mut s.case, |b| b <= 2);
        if s.extra[2].len == 0 {
            return false;
        }
        for (k, ignore) in s.extra.iter_mut().zip(IGNORE) {
            trim(k, |b| b == ignore);
        }
        true
    }
}

/// Drops the bytes at the end of `k` that `unneeded` matches.
fn trim(k: &mut Key, unneeded: impl Fn(u32) -> bool) {
    while k.len > 0 && unneeded(k.byte(k.len - 1)) {
        k.len -= 1;
    }
}

/// ntdll's `wcsncmp`.
fn wcsncmp(s1: u32, s2: u32, mut n: u32) -> i32 {
    if n == 0 {
        return 0;
    }
    let (mut a, mut b) = (s1, s2);
    loop {
        n -= 1;
        if n == 0 || ld16(a) == 0 || ld16(a) != ld16(b) {
            break;
        }
        a += 2;
        b += 2;
    }
    ld16(a) as i32 - ld16(b) as i32
}

/// `compare_sortkeys`.
fn compare_keys(k1: &Key, k2: &Key, shorter_wins: bool) -> i32 {
    let r = mem_sign(k1.buf, k2.buf, k1.len.min(k2.len));
    if r != 0 {
        return r;
    }
    if shorter_wins {
        k2.len as i32 - k1.len as i32
    } else {
        k1.len as i32 - k2.len as i32
    }
}

fn mem_sign(a: u32, b: u32, n: u32) -> i32 {
    for i in 0..n {
        let (x, y) = (ld8(a + i), ld8(b + i));
        if x != y {
            return if x < y { -1 } else { 1 };
        }
    }
    0
}

/// `get_digit_zero_char`: the zero of the digit range containing `c`.
fn digit_zero(c: u32) -> u32 {
    const ZEROES: [u32; 28] = [
        0x0030, 0x0660, 0x06f0, 0x0966, 0x09e6, 0x0a66, 0x0ae6, 0x0b66, 0x0be6, 0x0c66, 0x0ce6,
        0x0d66, 0x0e50, 0x0ed0, 0x0f20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x1bb0, 0x1c40,
        0x1c50, 0xa620, 0xa8d0, 0xa900, 0xaa50, 0xff10,
    ];
    let (mut min, mut max) = (0i32, ZEROES.len() as i32 - 1);
    while min <= max {
        let pos = (min + max) / 2;
        let z = ZEROES[pos as usize];
        if z <= c && z + 9 >= c {
            return z;
        }
        if z < c {
            min = pos + 1;
        } else {
            max = pos - 1;
        }
    }
    0
}

/// `append_digit_weights` (SORT_DIGITSASNUMBERS), for a digit at `src`: the
/// number of extra characters used.
fn digit_weights(key: &mut Key, src: u32, srclen: i32) -> i32 {
    let zero = digit_zero(ch(src, 0));
    let mut values = [0u32; 19];
    values[0] = ch(src, 0) - zero;
    let mut len = 1;
    while len < values.len() && (len as i32) < srclen {
        let c = ch(src, len as i32);
        if c < zero || c > zero + 9 {
            break;
        }
        values[len] = c - zero;
        len += 1;
    }
    let lzero = values[..len].iter().position(|&v| v != 0).unwrap_or(len);
    key.append(SCRIPT_DIGIT);
    key.append(2);
    key.append((2 + len - lzero) as u32);
    let mut val = 2;
    for (i, &v) in values.iter().enumerate().take(len).skip(lzero) {
        if (len - i) % 2 != 0 {
            key.append((val << 4) + v + 2);
        } else {
            val = v + 2;
        }
    }
    key.append(0xfe - lzero as u32);
    len as i32 - 1
}

/// `compare_string` for the sort `sortid` (an entry of the `sort` tables at
/// `sort`): negative, zero or positive, or `None` to decline.
#[allow(clippy::too_many_arguments)]
pub fn compare(
    sort: u32,
    sortid: u32,
    flags: u32,
    src1: u32,
    len1: u32,
    src2: u32,
    len2: u32,
    (scratch, scratch_size): (u32, u32),
) -> Option<i32> {
    // The sort must be one of the table's.
    let (guids, guid_count) = (ld(sort + SORT_GUIDS), ld(sort + SORT_GUID_COUNT));
    let off = sortid.wrapping_sub(guids);
    if sortid == 0 || off % SORTGUID_SIZE != 0 || off / SORTGUID_SIZE >= guid_count {
        return None;
    }
    let mut case_mask = 0x3f;
    if flags & NORM_IGNORECASE != 0 {
        case_mask &= !(CASE_UPPER | CASE_SUBSCRIPT);
    }
    if flags & NORM_IGNOREWIDTH != 0 {
        case_mask &= !CASE_FULLWIDTH;
    }
    if flags & NORM_IGNOREKANATYPE != 0 {
        case_mask &= !CASE_KATAKANA;
    }
    let mut except = ld(sortid + SORTGUID_EXCEPT);
    let ling_except = ld(sortid + SORTGUID_LING_EXCEPT);
    if flags & NORM_LINGUISTIC_CASING != 0 && except != 0 && ling_except != 0 {
        except = ling_except;
    }
    let mut c = Cmp {
        keys: ld(sort + SORT_KEYS),
        expansions: ld(sort + SORT_EXPANSIONS),
        exp_count: ld(sort + SORT_EXP_COUNT),
        compressions: ld(sort + SORT_COMPRESSIONS),
        compr_count: ld(sort + SORT_COMPR_COUNT),
        compr_data: ld(sort + SORT_COMPR_DATA),
        jamo: ld(sort + SORT_JAMO),
        sort_flags: ld(sortid + SORTGUID_FLAGS),
        compr: ld(sortid + SORTGUID_COMPR),
        flags,
        case_mask,
        except,
        compr_tables: None,
        bad: false,
    };
    let (primary1, primary2) = (scratch, scratch + PRIMARY_SIZE);
    let mut alloc = Scratch {
        next: scratch + 2 * PRIMARY_SIZE,
        end: scratch + scratch_size,
    };
    let mut s1 = State::new(flags, len1, primary1, &mut alloc)?;
    let mut s2 = State::new(flags, len2, primary2, &mut alloc)?;
    let (n1, n2) = (len1 as i32, len2 as i32);
    let (mut pos1, mut pos2) = (0, 0);
    let ret = 'done: {
        while pos1 < n1 || pos2 < n2 {
            while pos1 < n1 && s1.primary.len == 0 {
                pos1 += c.append_weights(&mut s1, src1, n1, pos1);
            }
            while pos2 < n2 && s2.primary.len == 0 {
                pos2 += c.append_weights(&mut s2, src2, n2, pos2);
            }
            if c.bad || s1.primary.over || s2.primary.over {
                return None;
            }
            let len = s1.primary.len.min(s2.primary.len);
            if len == 0 {
                break;
            }
            let r = mem_sign(primary1, primary2, len);
            if r != 0 {
                break 'done r;
            }
            copy(primary1, primary1 + len, s1.primary.len - len);
            copy(primary2, primary2 + len, s2.primary.len - len);
            s1.primary.len -= len;
            s2.primary.len -= len;
            s1.primary_pos += len;
            s2.primary_pos += len;
        }
        let r = s1.primary.len as i32 - s2.primary.len as i32;
        if r != 0 {
            break 'done r;
        }
        let extra1 = c.remove_unneeded_weights(&mut s1);
        let extra2 = c.remove_unneeded_weights(&mut s2);
        let r = compare_keys(&s1.diacritic, &s2.diacritic, false);
        if r != 0 {
            break 'done r;
        }
        let r = compare_keys(&s1.case, &s2.case, false);
        if r != 0 {
            break 'done r;
        }
        if extra1 && extra2 {
            for i in 0..4 {
                let r = compare_keys(&s1.extra[i], &s2.extra[i], i != 1);
                if r != 0 {
                    break 'done r;
                }
            }
        } else if extra1 != extra2 {
            break 'done extra1 as i32 - extra2 as i32;
        }
        compare_keys(&s1.special, &s2.special, false)
    };
    Some(ret)
}
