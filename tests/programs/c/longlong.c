/* 64-bit integer arithmetic (libgcc helpers), shifts and mixed widths. */
#include <stdio.h>
#include <stdint.h>

static uint64_t xs = 88172645463325252ULL;
static uint64_t next(void) { xs ^= xs << 13; xs ^= xs >> 7; xs ^= xs << 17; return xs; }

int main(void) {
    uint64_t h = 1469598103934665603ULL;
    int64_t s = 0;
    for (int i = 0; i < 2000; i++) {
        uint64_t a = next(), b = next() | 1;
        int64_t sa = (int64_t)a, sb = (int64_t)(b >> (i & 31)) | 1;
        h = (h ^ (a / b)) * 1099511628211ULL;
        h ^= a % b;
        h += (uint64_t)(sa / sb) ^ (uint64_t)(sa % sb);
        h ^= a >> (i & 63);
        h += a << (i % 63);
        s += (int64_t)(int32_t)a * (int32_t)(b >> 32);
        s ^= sa >> (i & 63);
    }
    printf("hash %08x%08x\n", (unsigned)(h >> 32), (unsigned)h);
    printf("sum %lld\n", (long long)s);
    unsigned long long big = 0xfedcba9876543210ULL;
    printf("%llu %llx %lld\n", big / 1000000007ULL, big * 3, (long long)big >> 7);
    return (int)(h & 0x3f);
}
