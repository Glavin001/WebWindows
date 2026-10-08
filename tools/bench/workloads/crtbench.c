/* C runtime string functions, one timing line each ("name checksum
 * seconds", as tools/bench/ab.mjs --wine reads), on short and longer
 * strings: for A/B tests of their native implementations
 * (crates/wwt-strings), e.g.
 *
 *   i686-w64-mingw32-gcc -O2 crtbench.c -o crtbench.exe
 *   node tools/bench/ab.mjs --wine "crtbench.exe" wine=WWT_NATIVE_STRINGS=0 native=
 */

#include <windows.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <wchar.h>

static LARGE_INTEGER freq;

static double now(void)
{
    LARGE_INTEGER t;
    QueryPerformanceCounter(&t);
    return (double)t.QuadPart / freq.QuadPart;
}

static char s[4][256];
static WCHAR w[4][256];
/* Lengths: short (8) and longer (100) strings. */
static const int lens[] = {8, 100};

#define BENCH(name, n, expr)                                          \
    do {                                                              \
        unsigned sum = 0;                                             \
        double t0 = now();                                            \
        for (int i = 0; i < (n); i++) {                               \
            int k = i & 1;                                            \
            sum += (unsigned)(expr);                                  \
        }                                                             \
        printf("%-12s %12u %8.3f\n", name, sum, now() - t0);          \
    } while (0)

int main(int argc, char **argv)
{
    int n = 1000000 * (argc > 1 ? atoi(argv[1]) : 1);
    QueryPerformanceFrequency(&freq);
    for (int k = 0; k < 2; k++) {
        for (int i = 0; i < lens[k]; i++) {
            s[k][i] = s[k + 2][i] = 'a' + i % 26;
            w[k][i] = w[k + 2][i] = 'a' + i % 26;
        }
        s[k + 2][lens[k] - 1] = 'Z';
    }
    BENCH("strlen", n, strlen(s[k]) + k);
    BENCH("wcslen", n, wcslen(w[k]) + k);
    BENCH("memcmp", n, memcmp(s[k], s[k + 2], lens[k]) + 2);
    BENCH("strcmp", n, strcmp(s[k], s[k + 2]) + 2);
    BENCH("strchr", n, strchr(s[k], 'Z' - k) == NULL);
    BENCH("wcschr", n, wcschr(w[k], L'Z') == NULL);
    BENCH("memchr", n, memchr(s[k], 'Z', lens[k]) == NULL);
    BENCH("strcspn", n, strcspn(s[k], "XYZ;,"));
    BENCH("lstrcmpiW", n / 4, lstrcmpiW(w[k], w[k + 2]) + 2);
    BENCH("lstrcmpW", n / 4, lstrcmpW(w[k], w[k + 2]) + 2);
    BENCH("CompareStrA", n / 4, CompareStringA(LOCALE_USER_DEFAULT, 0, s[k], -1, s[k + 2], -1));
    return 0;
}
