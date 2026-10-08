//! The functions called as translated code calls them: arguments on a guest
//! stack, results in the CPU struct. Expected comparison results are from
//! Wine's kernel32 tests (`dlls/kernel32/tests/locale.c`) and from Wine
//! itself; the differential test against Wine's own code is
//! `tests/wine/strings.mjs`.

use super::*;

const CPU: u32 = 0x2_0000;
const STACK: u32 = 0x3_0000;
const TEB: u32 = 0x5_0000;
const DATA: u32 = 0x6_0000;
const SORT: u32 = 0x7_0000;
const NLS: u32 = 0x10_0000;
const SCRATCH: u32 = 0x200_0000;
const LIMIT: u32 = 0x300_0000;
const RET: u32 = 0x40_1234;

fn setup() {
    init(LIMIT, SCRATCH, 1 << 20);
    st(CPU + CPU_FS_BASE, TEB);
}

/// Calls `f` with these stack arguments; returns (next eip, eax, bytes
/// popped), or None when it declined (and then checks nothing changed).
fn call(f: impl Fn(u32) -> u32, args: &[u32]) -> Option<(u32, u32, u32)> {
    let esp = STACK - 4 * args.len() as u32 - 4;
    st(esp, RET);
    for (i, &a) in args.iter().enumerate() {
        st(esp + 4 + 4 * i as u32, a);
    }
    st(CPU + CPU_ESP, esp);
    st(CPU + CPU_EAX, 0xdead);
    let next = f(CPU);
    if next == DECLINE {
        assert_eq!((ld(CPU + CPU_ESP), ld(CPU + CPU_EAX)), (esp, 0xdead));
        return None;
    }
    Some((next, ld(CPU + CPU_EAX), ld(CPU + CPU_ESP) - esp))
}

/// A NUL-terminated byte string at `at`.
fn put_str(at: u32, s: &[u8]) -> u32 {
    mem::put(at, s);
    st8(at + s.len() as u32, 0);
    at
}

/// A NUL-terminated UTF-16 string at `at`.
fn put_wstr(at: u32, s: &str) -> u32 {
    let mut b: Vec<u8> = s.encode_utf16().flat_map(u16::to_le_bytes).collect();
    b.extend([0, 0]);
    mem::put(at, &b);
    at
}

#[test]
fn c_strings() {
    setup();
    let a = put_str(DATA, b"hello, world");
    let b = put_str(DATA + 0x100, b"hello, there");
    assert_eq!(call(|c| strlen(c), &[a]), Some((RET, 12, 4)));
    assert_eq!(call(|c| strlen(c), &[a + 5]), Some((RET, 7, 4)));
    assert_eq!(call(|c| strcmp(c), &[a, b]).unwrap().1, 1);
    assert_eq!(call(|c| strcmp(c), &[b, a]).unwrap().1, -1i32 as u32);
    assert_eq!(call(|c| strcmp(c), &[a, a]).unwrap().1, 0);
    assert_eq!(call(|c| memcmp(c), &[a, b, 7]).unwrap().1, 0);
    assert_eq!(call(|c| memcmp(c), &[a, b, 8]).unwrap().1, 1);
    assert_eq!(call(|c| memcmp(c), &[0, 1, 0]).unwrap().1, 0);
    assert_eq!(call(|c| strchr(c), &[a, b'w' as u32]).unwrap().1, a + 7);
    assert_eq!(
        call(|c| strchr(c), &[a, 0x100 | b'w' as u32]).unwrap().1,
        a + 7
    );
    assert_eq!(call(|c| strchr(c), &[a, 0]).unwrap().1, a + 12);
    assert_eq!(call(|c| strchr(c), &[a, b'z' as u32]).unwrap().1, 0);
    assert_eq!(call(|c| memchr(c), &[a, b'o' as u32, 4]).unwrap().1, 0);
    assert_eq!(call(|c| memchr(c), &[a, b'o' as u32, 5]).unwrap().1, a + 4);
    let reject = put_str(DATA + 0x200, b"xw ");
    assert_eq!(call(|c| strcspn(c), &[a, reject]).unwrap().1, 6);
    let w = put_wstr(DATA + 0x300, "wide\u{8000}x");
    assert_eq!(call(|c| wcslen(c), &[w]).unwrap().1, 6);
    assert_eq!(call(|c| wcschr(c), &[w, 0x1_8000]).unwrap().1, w + 8);
    assert_eq!(call(|c| wcschr(c), &[w, 0]).unwrap().1, w + 12);
}

#[test]
fn c_strings_decline_where_x86_faults() {
    setup();
    let a = put_str(DATA, b"abc");
    for p in [0, 1, 0xfff0, LIMIT, LIMIT - 0x8000, 0xffff_fff0] {
        assert_eq!(call(|c| strlen(c), &[p]), None);
        assert_eq!(call(|c| strcmp(c), &[a, p]), None);
        assert_eq!(call(|c| memcmp(c), &[a, p, 4]), None);
        assert_eq!(call(|c| strcspn(c), &[a, p]), None);
    }
    // A string that runs into the end of guest memory.
    let end = LIMIT - 0x10000;
    for i in 0..64 {
        st8(end - 64 + i, b'x' as u32);
    }
    assert_eq!(call(|c| strlen(c), &[end - 64]), None);
    assert_eq!(call(|c| wcslen(c), &[end - 64]), None);
    assert_eq!(call(|c| memchr(c), &[end - 64, 0, 65]), None);
    assert_eq!(call(|c| memchr(c), &[end - 64, 0, 64]).unwrap().1, 0);
    st8(end - 1, 0);
    assert_eq!(call(|c| strlen(c), &[end - 64]).unwrap().1, 63);
}

#[test]
fn tls() {
    setup();
    st(TEB + TEB_LAST_ERROR, 5);
    st(TEB + TEB_TLS_SLOTS + 4 * 7, 0x1234);
    assert_eq!(call(|c| tls_get_value(c), &[7]), Some((RET, 0x1234, 8)));
    assert_eq!(ld(TEB + TEB_LAST_ERROR), 0);
    assert_eq!(call(|c| tls_get_value(c), &[64]), None);
    let index = DATA;
    st(index, 7);
    st(TEB + TEB_LAST_ERROR, 5);
    assert_eq!(
        call(|cpu| msvcrt_get_thread_data(cpu, index), &[]),
        Some((RET, 0x1234, 4))
    );
    assert_eq!(ld(TEB + TEB_LAST_ERROR), 5);
    st(index, 8);
    assert_eq!(call(|cpu| msvcrt_get_thread_data(cpu, index), &[]), None);
}

/// Loads `sortdefault.nls` as kernelbase's `load_sortdefault_nls` does,
/// filling its `sort` at SORT; false when Wine's sources are not here.
fn load_sorts() -> bool {
    let src = std::env::var("WINE_SRC").unwrap_or_else(|_| "/opt/wine-src/wine-11.0".into());
    let Ok(file) = std::fs::read(format!("{src}/nls/sortdefault.nls")) else {
        eprintln!("skipped: no {src}/nls/sortdefault.nls");
        return false;
    };
    mem::put(NLS, &file);
    let h = NLS;
    st(SORT + 16, h + ld(h)); // keys
    st(SORT + 20, h + ld(h + 4)); // casemap
    let ctype = h + ld(h + 8);
    st(SORT + 24, ctype + 4);
    st(SORT + 28, ctype + ld16(ctype + 2) + 2);
    let table = h + ld(h + 12);
    st(SORT, ld(table));
    st(SORT + 4, ld(table + 4));
    let guids = table + 8;
    st(SORT + 32, guids);
    let table = guids + 36 * ld(table + 4);
    st(SORT + 8, ld(table));
    let expansions = table + 4;
    st(SORT + 36, expansions);
    let table = expansions + 4 * ld(table);
    let count = ld(table);
    st(SORT + 12, count);
    let compressions = table + 4;
    st(SORT + 40, compressions);
    let data = compressions + 24 * count;
    st(SORT + 44, data);
    let last = compressions + 24 * (count - 1);
    let mut table = data + 2 * ld(last);
    for i in 0..7 {
        table += 4 * ld16(last + 8 + 2 * i) * ((i + 5) / 2);
    }
    table += 4 * (1 + ld(table) / 2);
    st(SORT + 48, table + 4);
    true
}

/// CompareStringEx(locale NULL) with the default sort as the current one.
fn compare(flags: u32, a: &str, b: &str) -> Option<u32> {
    let current = DATA + 0x800;
    st(current, ld(SORT + 32));
    let (s1, s2) = (put_wstr(DATA, a), put_wstr(DATA + 0x400, b));
    let r = call(
        |cpu| compare_string_ex(cpu, SORT, current),
        &[0, flags, s1, -1i32 as u32, s2, -1i32 as u32, 0, 0, 0],
    )?;
    assert_eq!((r.0, r.2), (RET, 40));
    Some(r.1)
}

#[test]
fn compare_strings() {
    if !load_sorts() {
        return;
    }
    setup();
    const LT: Option<u32> = Some(1);
    const EQ: Option<u32> = Some(2);
    const GT: Option<u32> = Some(3);
    assert_eq!(compare(0, "'o", "-o"), LT);
    assert_eq!(compare(0x1000, "'o", "-o"), LT);
    assert_eq!(compare(0, "'", "-"), LT);
    assert_eq!(compare(0, "`o", "/m"), GT);
    assert_eq!(compare(0x1000, "/m", "`o"), LT);
    assert_eq!(compare(0, "`o", "-m"), LT);
    assert_eq!(compare(0x1000, "`o", "-m"), GT);
    assert_eq!(compare(1, "#", "."), LT);
    assert_eq!(compare(1, "_", "."), GT);
    assert_eq!(compare(0, "Salut", "Salute"), LT);
    assert_eq!(compare(0, "Salut", "saLuT"), GT);
    assert_eq!(compare(1, "Salut", "saLuT"), EQ);
    assert_eq!(compare(0, "a", "\u{e1}"), LT);
    assert_eq!(compare(2, "a", "\u{e1}"), EQ);
    assert_eq!(compare(0, "\u{e6}", "ae"), EQ);
    assert_eq!(compare(0, "item 10", "item 9"), LT);
    assert_eq!(compare(8, "item 10", "item 9"), GT);
    // Flags CompareStringEx rejects, and named locales, are declined.
    assert_eq!(compare(0x80, "a", "b"), None);
    let (s1, s2, current) = (
        put_wstr(DATA, "a"),
        put_wstr(DATA + 0x400, "b"),
        DATA + 0x800,
    );
    assert_eq!(
        call(
            |cpu| compare_string_ex(cpu, SORT, current),
            &[DATA + 0x600, 0, s1, 1, s2, 1, 0, 0, 0]
        ),
        None
    );
    assert_eq!(
        call(
            |cpu| compare_string_ex(cpu, SORT, current),
            &[0, 0, 0, 1, s2, 1, 0, 0, 0]
        ),
        None
    );
    // A current sort that is not one of the table's.
    st(current, ld(SORT + 32) + 4);
    assert_eq!(
        call(
            |cpu| compare_string_ex(cpu, SORT, current),
            &[0, 0, s1, 1, s2, 1, 0, 0, 0]
        ),
        None
    );
}
