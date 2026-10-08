/* Differential test of the native string and locale functions
 * (crates/wwt-strings): prints the results of many calls, which must not
 * change when the native functions are turned off (tests/wine/strings.mjs).
 * Built with MinGW; run on translated Wine. */

#include <windows.h>
#include <ctype.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <wchar.h>

static const WCHAR *strs[] = {
    L"", L"a", L"A", L"b", L"B", L"ab", L"aB", L"Ab", L"abc", L"abd", L"abcd",
    L"Item 1 of 7: Odd", L"ITEM 1 OF 7: ODD", L"item 10", L"item 9", L"item 09", L"item 009",
    L"a-b", L"ab-", L"-ab", L"a b", L"a'b", L"ab'", L"co-op", L"coop", L"co op",
    L"\x00e9", L"e\x0301", L"E\x0301", L"\x00c9", L"e", L"\x00e8\x00e9", L"\x00e9\x00e8",
    L"\x00e6", L"ae", L"AE", L"\x00df", L"ss", L"SS", L"\x0153", L"oe", L"\xfb01", L"fi",
    L"\x30a2", L"\x3042", L"\xff71", L"\x30a2\x30fc", L"\x3042\x30fc", L"\x30ab\x30fd", L"\x304b\x309d",
    L"\x30a2\x30a2", L"\x3041", L"\x30a1", L"\x30fc", L"\x309d",
    L"\x1100\x1161", L"\x1100\x1161\x11a8", L"\xac00", L"\xac01", L"\x1100\x1176", L"\x1113\x1161", L"\x115f\x1161",
    L"\x4e00", L"\x4e01", L"\x3400", L"\x3401", L"\xe000", L"\xe001",
    L"\x05d0", L"\x05d1", L"\x0627", L"\x0628", L"\x064b", L"\x0627\x064b",
    L"0", L"1", L"10", L"2", L"00", L"\x0660", L"\x0661\x0660", L"\xff11", L"12345678901234567890123",
    L"!", L"?", L"\x00a1", L"#1", L"$", L"\x2010", L"\x0300", L"a\x0300\x0301", L"\x200b", L"a\x200b",
    L"\x0430", L"\x0410", L"\x03b1", L"\x0391", L"\x0131", L"I", L"i", L"\x0130",
    L"\xd800\xdc00", L"\xd800", L"\xdc00", L"\xfffe", L"\xffff",
    /* Compressions in some locales' sorts (hu, cs, da, es traditional). */
    L"cs", L"csa", L"cz", L"dzs", L"ccs", L"CS", L"Cs", L"ch", L"cha", L"chb", L"h", L"aa", L"Aa",
    L"\x00e5", L"ll", L"lla", L"lz", L"ny", L"nz", L"ggy", L"\x0131i", L"I\x0307",
};
#define NSTRS (sizeof strs / sizeof strs[0])

static const DWORD flag_sets[] = {
    0, NORM_IGNORECASE, NORM_IGNORENONSPACE, NORM_IGNORESYMBOLS, SORT_STRINGSORT,
    NORM_IGNOREKANATYPE, NORM_IGNOREWIDTH, NORM_IGNOREKANATYPE | NORM_IGNOREWIDTH,
    LINGUISTIC_IGNORECASE, LINGUISTIC_IGNOREDIACRITIC, SORT_DIGITSASNUMBERS,
    NORM_LINGUISTIC_CASING, NORM_LINGUISTIC_CASING | NORM_IGNORECASE, NORM_IGNORECASE | NORM_IGNORENONSPACE | NORM_IGNORESYMBOLS,
    SORT_STRINGSORT | NORM_IGNORECASE, 0x10000000, LOCALE_USE_CP_ACP, 0x80, 0x40,
};
#define NFLAGS (sizeof flag_sets / sizeof flag_sets[0])

/* Characters random strings are made of: the interesting ones above. */
static const WCHAR pool[] = L"aAbBeEcsCSzdhlyn\x0301\x00e9\x00c9\x00e6\x00df-'!0129\x0660\x30a2\x3042\xff71\x30fc\x309d\x30fd"
                            L"\x1100\x1161\x11a8\x1176\x115f\xac00\x4e00\x3400\xe000\x05d0\x0627\x064b\x200b\x0131I"
                            L"\x0430\x03b1\xd800\xdc00 i\x0130\x00e8\x00ea\x0300\x30ab\x304b\x30ad\x30f3\x3063\x30c3";

static unsigned rng = 1;
static unsigned next(void) { return rng = rng * 1103515245 + 12345, rng >> 16; }

static void out(int r)
{
    if (r) putchar('0' + r);
    else printf("[0:%lu]", GetLastError());
}

/* Translated Wine stops at a fault (there is no exception dispatch), so
 * faults are tested one per run: the runner checks that each stops at the
 * same instruction and address either way. */
#define TRY(call) printf("%ld ", (long)(call))

typedef size_t (__cdecl *strlen_fn)(const char *);
typedef size_t (__cdecl *wcslen_fn)(const WCHAR *);
typedef int (__cdecl *memcmp_fn)(const void *, const void *, size_t);
typedef int (__cdecl *strcmp_fn)(const char *, const char *);
typedef char *(__cdecl *strchr_fn)(const char *, int);
typedef WCHAR *(__cdecl *wcschr_fn)(const WCHAR *, WCHAR);
typedef void *(__cdecl *memchr_fn)(const void *, int, size_t);
typedef size_t (__cdecl *strcspn_fn)(const char *, const char *);

static void crt(const char *dll)
{
    HMODULE m = LoadLibraryA(dll);
    strlen_fn p_strlen = (void *)GetProcAddress(m, "strlen");
    wcslen_fn p_wcslen = (void *)GetProcAddress(m, "wcslen");
    memcmp_fn p_memcmp = (void *)GetProcAddress(m, "memcmp");
    strcmp_fn p_strcmp = (void *)GetProcAddress(m, "strcmp");
    strchr_fn p_strchr = (void *)GetProcAddress(m, "strchr");
    wcschr_fn p_wcschr = (void *)GetProcAddress(m, "wcschr");
    memchr_fn p_memchr = (void *)GetProcAddress(m, "memchr");
    strcspn_fn p_strcspn = (void *)GetProcAddress(m, "strcspn");
    static char a[64], b[64];
    char *page;
    const char *bad[] = {NULL, (char *)1, (char *)0xfff0, (char *)0x7fffffff, (char *)0xfffffff0};

    printf("%s:\n", dll);
    strcpy(a, "hello, world");
    strcpy(b, "hello, there");
    for (int i = 0; i <= 13; i++) {
        TRY(p_strlen(a + i));
        TRY(p_memcmp(a, b, i));
        TRY(p_memcmp(b + 7, a + 7, i));
        TRY(p_strcmp(a + i, b + i));
        TRY(p_strchr(a, "hlo, wrdx\0"[i % 10]) - a);
        TRY(p_memchr(a, 'w', i) ? (char *)p_memchr(a, 'w', i) - a : -1);
        TRY(p_strcspn(a + (i % 5), ",x d" + (i % 4)));
        if (p_wcslen) TRY(p_wcslen(strs[i]));
        if (p_wcschr) TRY(p_wcschr(L"hello\x8000", L"lo\x8000z"[i % 4] | (i << 16)) ? 1 : 0);
        putchar('\n');
    }
    for (int i = 0; i < 256; i++) {
        a[0] = i;
        b[0] = 255 - i;
        a[1] = b[1] = 0;
        printf("%d%d%d", p_strcmp(a, b) + 1, p_memcmp(a, b, 2) + 1, p_strchr("\x80\xff" "abc", i) != NULL);
    }
    putchar('\n');
    /* Calls that read nothing. */
    for (unsigned i = 0; i < sizeof bad / sizeof bad[0]; i++) {
        TRY(p_memcmp(bad[i], a, 0));
        TRY(p_memchr(bad[i], 0, 0) != NULL);
        putchar('\n');
    }
    /* Strings that end before an unreadable page. */
    page = VirtualAlloc(NULL, 0x20000, MEM_RESERVE, PAGE_NOACCESS);
    VirtualAlloc(page, 0x10000, MEM_COMMIT, PAGE_READWRITE);
    memset(page, 'x', 0x10000);
    page[0xffff] = 0;
    TRY(p_strlen(page + 0xfff0));
    TRY(p_strcmp(page + 0xfff0, page + 0xffe0));
    TRY(p_memcmp(page, page + 0x10, 0x10000 - 0x10));
    TRY(p_memchr(page, 'y', 0x10000) != NULL);
    putchar('\n');
    VirtualFree(page, 0, MEM_RELEASE);
}

/* The address of a static symbol of a loaded module, from the COFF symbol
 * table of its file (Wine's DLLs keep theirs), or NULL. */
static void *symbol(const char *dll, const char *name)
{
    static BYTE file[8 << 20];
    char path[MAX_PATH];
    HMODULE m = GetModuleHandleA(dll);
    DWORD size = 0;
    HANDLE h;
    BYTE *pe, *sym, *strtab;
    IMAGE_SECTION_HEADER *sec;

    if (!m || !GetModuleFileNameA(m, path, sizeof path)) return NULL;
    h = CreateFileA(path, GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
    if (h == INVALID_HANDLE_VALUE) return NULL;
    ReadFile(h, file, sizeof file, &size, NULL);
    CloseHandle(h);
    pe = file + *(DWORD *)(file + 0x3c);
    sym = file + *(DWORD *)(pe + 12);
    strtab = sym + 18 * *(DWORD *)(pe + 16);
    sec = (IMAGE_SECTION_HEADER *)(pe + 24 + *(WORD *)(pe + 20));
    for (DWORD i = 0; *(DWORD *)(pe + 12) && i < *(DWORD *)(pe + 16); i += 1 + sym[i * 18 + 17]) {
        BYTE *e = sym + i * 18;
        char short_name[9] = {0};
        const char *n = short_name;
        short secnum = *(short *)(e + 12);
        if (*(DWORD *)e) memcpy(short_name, e, 8);
        else n = (char *)strtab + *(DWORD *)(e + 4);
        if (secnum > 0 && !strcmp(n, name)) return (BYTE *)m + sec[secnum - 1].VirtualAddress + *(DWORD *)(e + 8);
    }
    return NULL;
}

/* The user's sort is the default one here (there is no registry to choose
 * another), so the comparisons are repeated with kernelbase's
 * current_locale_sort set to each sort in turn: with compressions,
 * exceptions, reversed diacritics. Both implementations read it. */
static void sorts(void)
{
    BYTE *sort = symbol("kernelbase.dll", "_sort");
    BYTE **current = symbol("kernelbase.dll", "_current_locale_sort");
    BYTE *saved, *guids;
    DWORD count;
    static WCHAR s1[16], s2[16];

    if (!sort || !current) {
        printf("no symbols\n");
        return;
    }
    saved = *current;
    count = *(DWORD *)(sort + 4);
    guids = *(BYTE **)(sort + 32);
    printf("%lu sorts, current %ld\n", count, (long)(saved - guids) / 36);
    for (DWORD k = 0; k < count; k++) {
        *current = guids + 36 * k;
        printf("%lu %08lx %lu %lu %lu: ", k, *(DWORD *)(*current + 16), *(DWORD *)(*current + 20),
               *(DWORD *)(*current + 24), *(DWORD *)(*current + 28));
        for (unsigned f = 0; f < NFLAGS; f++) {
            for (int p = 0; p < 60; p++) {
                int n1 = next() % 8, n2 = next() % 8;
                for (int i = 0; i < n1; i++) s1[i] = pool[next() % (sizeof pool / 2 - 1)];
                /* Mostly near misses: two characters swapped, or one replaced. */
                switch (next() % 3) {
                case 0:
                    for (int i = 0; i < n2; i++) s2[i] = pool[next() % (sizeof pool / 2 - 1)];
                    break;
                case 1:
                    memcpy(s2, s1, sizeof s1);
                    n2 = n1;
                    if (n2 > 1) {
                        int i = next() % (n2 - 1);
                        WCHAR c = s2[i];
                        s2[i] = s2[i + 1];
                        s2[i + 1] = c;
                    }
                    break;
                default:
                    memcpy(s2, s1, sizeof s1);
                    n2 = n1;
                    if (n2) s2[next() % n2] = pool[next() % (sizeof pool / 2 - 1)];
                }
                out(CompareStringEx(NULL, flag_sets[f], s1, n1, s2, n2, NULL, NULL, 0));
            }
            for (int p = 0; p < 20; p++) {
                const WCHAR *a = strs[next() % NSTRS], *b = strs[next() % NSTRS];
                out(CompareStringEx(NULL, flag_sets[f], a, -1, b, -1, NULL, NULL, 0));
            }
            /* Casing exceptions (Turkish i). */
            if (*(DWORD *)(*current + 24)) {
                static const WCHAR *is[] = {L"i", L"I", L"\x0131", L"\x0130", L"ia", L"Ia", L"\x0131" L"a", L"\x0130" L"a"};
                for (int i = 0; i < 8; i++)
                    for (int j = 0; j < 8; j++) out(CompareStringEx(NULL, flag_sets[f], is[i], -1, is[j], -1, NULL, NULL, 0));
            }
        }
        putchar('\n');
    }
    *current = saved;
}

/* Calls that fault. */
static void fault(int n)
{
    static char a[] = "abc";
    static WCHAR w[] = L"abc";
    char *p = (char *)(n & 0xff00 ? 0xfffffff0 : n & 0x10 ? 1 : 0);
    HMODULE m = LoadLibraryA(n & 0x20 ? "msvcrt.dll" : "ntdll.dll");
    strlen_fn p_strlen = (void *)GetProcAddress(m, "strlen");
    wcslen_fn p_wcslen = (void *)GetProcAddress(m, "wcslen");
    memcmp_fn p_memcmp = (void *)GetProcAddress(m, "memcmp");
    strcmp_fn p_strcmp = (void *)GetProcAddress(m, "strcmp");
    strchr_fn p_strchr = (void *)GetProcAddress(m, "strchr");
    memchr_fn p_memchr = (void *)GetProcAddress(m, "memchr");
    strcspn_fn p_strcspn = (void *)GetProcAddress(m, "strcspn");

    switch (n & 0xf) {
    case 0: TRY(p_strlen(p)); break;
    case 1: TRY(p_wcslen((WCHAR *)p)); break;
    case 2: TRY(p_memcmp(a, p, 4)); break;
    case 3: TRY(p_strcmp(a, p)); break;
    case 4: TRY(p_strchr(p, 'z') != NULL); break;
    case 5: TRY(p_memchr(p, 'z', 32) != NULL); break;
    case 6: TRY(p_strcspn(a, p)); break;
    case 7: TRY(p_strcspn(p, a)); break;
    case 8: TRY(CompareStringEx(NULL, 0, w, -1, (WCHAR *)p, 3, NULL, NULL, 0)); break;
    case 9: TRY(CompareStringEx(NULL, 0, (WCHAR *)p, 40, w, 3, NULL, NULL, 0)); break;
    case 10: TRY(CompareStringW(LOCALE_USER_DEFAULT, 0, w, -1, (WCHAR *)p, -1)); break;
    }
    printf("no fault\n");
}

int main(int argc, char **argv)
{
    static WCHAR s1[64], s2[64];
    static char a1[64], a2[64];

    printf("locale %04lx\n", GetUserDefaultLCID());
    if (argc > 2 && !strcmp(argv[1], "fault")) {
        fault(strtol(argv[2], NULL, 0));
        return 0;
    }

    printf("pairs:\n");
    for (unsigned f = 0; f < NFLAGS; f++) {
        DWORD flags = flag_sets[f];
        printf("%08lx ", flags);
        for (unsigned i = 0; i < NSTRS; i++)
            for (unsigned j = 0; j < NSTRS; j++) {
                SetLastError(0xdead);
                out(CompareStringEx(NULL, flags, strs[i], -1, strs[j], -1, NULL, NULL, 0));
            }
        putchar('\n');
    }
    printf("random:\n");
    for (unsigned f = 0; f < NFLAGS + 8; f++) {
        DWORD flags = f < NFLAGS ? flag_sets[f] : flag_sets[next() % NFLAGS] | flag_sets[next() % NFLAGS] | flag_sets[next() % NFLAGS];
        printf("%08lx ", flags);
        for (int k = 0; k < 1500; k++) {
            int n1 = next() % 12, n2 = next() % 12;
            for (int i = 0; i < n1; i++) s1[i] = pool[next() % (sizeof pool / 2 - 1)];
            if (next() % 3 == 0) {
                memcpy(s2, s1, sizeof s1);
                n2 = n1;
                if (n2) s2[next() % n2] = pool[next() % (sizeof pool / 2 - 1)];
            } else
                for (int i = 0; i < n2; i++) s2[i] = pool[next() % (sizeof pool / 2 - 1)];
            s1[n1] = s2[n2] = 0;
            SetLastError(0xdead);
            out(CompareStringEx(NULL, flags, s1, n1, s2, n2, NULL, NULL, 0));
        }
        putchar('\n');
    }
    printf("apis:\n");
    for (unsigned i = 0; i < NSTRS; i += 3)
        for (unsigned j = 0; j < NSTRS; j += 2) {
            out(CompareStringW(LOCALE_USER_DEFAULT, NORM_IGNORECASE, strs[i], -1, strs[j], -1));
            out(CompareStringW(LOCALE_SYSTEM_DEFAULT, 0, strs[i], -1, strs[j], -1));
            out(CompareStringW(MAKELCID(MAKELANGID(LANG_ENGLISH, SUBLANG_ENGLISH_US), SORT_DEFAULT), 0, strs[i], -1, strs[j], -1));
            out(CompareStringW(MAKELCID(MAKELANGID(LANG_GERMAN, SUBLANG_GERMAN), SORT_DEFAULT), 0, strs[i], -1, strs[j], -1));
            out(CompareStringEx(L"sv-SE", 0, strs[i], -1, strs[j], -1, NULL, NULL, 0));
            out(lstrcmpW(strs[i], strs[j]) + 2);
            out(lstrcmpiW(strs[i], strs[j]) + 2);
            WideCharToMultiByte(CP_ACP, 0, strs[i], -1, a1, sizeof a1, NULL, NULL);
            WideCharToMultiByte(CP_ACP, 0, strs[j], -1, a2, sizeof a2, NULL, NULL);
            out(CompareStringA(LOCALE_USER_DEFAULT, 0, a1, -1, a2, -1));
            out(lstrcmpiA(a1, a2) + 2);
        }
    putchar('\n');
    printf("lengths:\n");
    for (int i = -2; i < 8; i++)
        for (int j = -2; j < 8; j++) {
            SetLastError(0xdead);
            out(CompareStringEx(NULL, 0, L"ab\0cd\x00e9" L"f", i, L"ab\0cDef", j, NULL, NULL, 0));
        }
    putchar('\n');
    printf("long:\n");
    {
        /* Long strings: secondary keys of thousands of bytes, and beyond
         * the native scratch memory. */
        static const int lens[] = {40, 1000, 8000, 12000, 60000};
        for (unsigned k = 0; k < sizeof lens / sizeof lens[0]; k++) {
            int n = lens[k];
            WCHAR *l1 = HeapAlloc(GetProcessHeap(), 0, (n + 1) * 2), *l2 = HeapAlloc(GetProcessHeap(), 0, (n + 1) * 2);
            for (int i = 0; i < n; i++) l1[i] = l2[i] = pool[next() % (sizeof pool / 2 - 1)];
            l1[n] = l2[n] = 0;
            for (unsigned f = 0; f < NFLAGS; f++) {
                out(CompareStringEx(NULL, flag_sets[f], l1, -1, l2, -1, NULL, NULL, 0));
                l2[n - 1 - next() % 8] = pool[next() % (sizeof pool / 2 - 1)];
                out(CompareStringEx(NULL, flag_sets[f], l1, n, l2, n, NULL, NULL, 0));
                out(CompareStringEx(NULL, flag_sets[f], l1, n - 1, l2, n, NULL, NULL, 0));
            }
            putchar('\n');
        }
    }
    printf("tls:\n");
    {
        static const DWORD idx[] = {0, 1, 5, 63, 64, 65, 100, 1087, 1088, 5000, 0xffffffff};
        DWORD k = TlsAlloc();
        TlsSetValue(k, (void *)0x1234);
        SetLastError(0xdead);
        void *v = TlsGetValue(k);
        printf("%p %lu ", v, GetLastError());
        for (unsigned i = 0; i < sizeof idx / sizeof idx[0]; i++) {
            SetLastError(0xdead);
            v = TlsGetValue(idx[i]);
            printf("%d %lu ", v != NULL, GetLastError());
        }
        SetLastError(0xbeef);
        k = toupper(0x61);
        printf("%lu %lu\n", k, GetLastError());
    }
    printf("sorts:\n");
    sorts();
    printf("errors:\n");
    {
        NLSVERSIONINFO v = {sizeof v};
        const WCHAR *bad[] = {NULL, (WCHAR *)1, (WCHAR *)0xfff0, (WCHAR *)0x7ffffff0};
        SetLastError(0xdead);
        out(CompareStringEx(NULL, 0, L"a", -1, L"b", -1, &v, NULL, 0));
        out(CompareStringEx(NULL, 0, L"a", -1, L"b", -1, NULL, (void *)1, 0));
        out(CompareStringEx(NULL, 0, L"a", -1, L"b", -1, NULL, NULL, 1));
        out(CompareStringEx(L"xx-bogus", 0, L"a", -1, L"b", -1, NULL, NULL, 0));
        out(CompareStringW(0x12345, 0, L"a", -1, L"b", -1));
        putchar('\n');
        for (unsigned i = 0; i < sizeof bad / sizeof bad[0]; i++) {
            SetLastError(0xdead);
            TRY(CompareStringEx(NULL, 0, L"b", -1, bad[i], 0, NULL, NULL, 0));
            TRY(GetLastError());
            TRY(lstrcmpiW(bad[i], NULL));
            putchar('\n');
        }
    }
    crt("ntdll.dll");
    crt("msvcrt.dll");
    crt("ucrtbase.dll");
    return 0;
}
