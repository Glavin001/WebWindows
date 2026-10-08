/* 64-bit division and remainder, which 32-bit x86 code does in libgcc's
 * __divdi3 and friends: signs, extremes and divisors of every size. */
#include <stdio.h>
#include <stdint.h>

static volatile int64_t sv[] = {
    0, 1, -1, 2, -2, 7, -7, 1000000007, -1000000007, 0x7fffffff, -0x80000000LL,
    0x100000000LL, -0x100000000LL, 0x123456789abcdefLL, -0x123456789abcdefLL,
    INT64_MAX, INT64_MIN, INT64_MIN + 1,
};
static volatile uint64_t uv[] = {
    0, 1, 2, 3, 10, 0xffffffffu, 0x100000000ULL, 0x123456789abcdefULL,
    0xfedcba9876543210ULL, UINT64_MAX, UINT64_MAX - 1, 1ULL << 63,
};

int main(void) {
    uint64_t h = 0;
    int n = sizeof sv / sizeof sv[0], m = sizeof uv / sizeof uv[0];
    for (int i = 0; i < n; i++)
        for (int j = 0; j < n; j++) {
            int64_t a = sv[i], b = sv[j];
            if (b == 0 || (a == INT64_MIN && b == -1)) continue;
            int64_t q = a / b, r = a % b;
            h = h * 1000003 ^ (uint64_t)q;
            h = h * 1000003 ^ (uint64_t)r;
            if (i < 6 && j < 6) printf("%lld / %lld = %lld rem %lld\n", (long long)a, (long long)b, (long long)q, (long long)r);
        }
    for (int i = 0; i < m; i++)
        for (int j = 0; j < m; j++) {
            uint64_t a = uv[i], b = uv[j];
            if (b == 0) continue;
            h = h * 1000003 ^ (a / b);
            h = h * 1000003 ^ (a % b);
        }
    /* The most negative number % -1 is defined (0); its quotient is not. */
    printf("min %% -1 = %lld\n", (long long)(sv[16] % sv[2]));
    printf("hash %llx\n", (unsigned long long)h);
    return 0;
}
