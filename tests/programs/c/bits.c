/* Bit manipulation: builtins, rotates, byte swaps, bitfields. */
#include <stdio.h>
#include <stdint.h>

struct bf { unsigned a : 3; unsigned b : 7; signed c : 5; unsigned d : 17; };

static uint32_t rotl(uint32_t x, int n) { return (x << n) | (x >> ((32 - n) & 31)); }

int main(void) {
    uint32_t x = 0x12345678;
    unsigned acc = 0;
    for (int i = 0; i < 32; i++) {
        uint32_t v = x * (i + 1) ^ (x >> i);
        acc += __builtin_popcount(v) + (v ? __builtin_ctz(v) + __builtin_clz(v) : 0);
        acc ^= rotl(v, i) + __builtin_bswap32(v);
        acc += __builtin_parity(v);
    }
    printf("acc %u\n", acc);
    struct bf s = { 5, 100, -7, 99999 };
    s.a += 3; s.c -= 9; s.d *= 3;
    printf("bf %u %u %d %u\n", s.a, s.b, s.c, s.d);
    uint16_t h = 0xabcd;
    int8_t c = -100;
    printf("small %u %d %d\n", (unsigned)(uint16_t)(h << 3), c * 3, (int8_t)(c - 100));
    return acc & 0xff;
}
