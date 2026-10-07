/* Small compute benchmark: sieve, CRC-32, matrix multiply (double),
   quicksort with a comparison function pointer. Prints timings-free
   results; time the whole process externally. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static unsigned crc_table[256];
static void crc_init(void) {
    for (unsigned i = 0; i < 256; i++) {
        unsigned c = i;
        for (int k = 0; k < 8; k++) c = c & 1 ? 0xEDB88320u ^ (c >> 1) : c >> 1;
        crc_table[i] = c;
    }
}
static unsigned crc32(const unsigned char *p, size_t n) {
    unsigned c = ~0u;
    while (n--) c = crc_table[(c ^ *p++) & 0xff] ^ (c >> 8);
    return ~c;
}
static int cmp_int(const void *a, const void *b) { int x = *(const int *)a, y = *(const int *)b; return (x > y) - (x < y); }
static void quick(int *a, int lo, int hi, int (*cmp)(const void *, const void *)) {
    while (lo < hi) {
        int p = a[(lo + hi) / 2], i = lo, j = hi;
        while (i <= j) {
            while (cmp(&a[i], &p) < 0) i++;
            while (cmp(&a[j], &p) > 0) j--;
            if (i <= j) { int t = a[i]; a[i] = a[j]; a[j] = t; i++; j--; }
        }
        if (j - lo < hi - i) { quick(a, lo, j, cmp); lo = i; } else { quick(a, i, hi, cmp); hi = j; }
    }
}

int main(void) {
    enum { N = 4000000 };
    char *sieve = calloc(N, 1);
    int primes = 0;
    for (int r = 0; r < 3; r++) {
        memset(sieve, 0, N);
        primes = 0;
        for (int i = 2; i < N; i++) {
            if (!sieve[i]) { primes++; for (int j = 2 * i; j < N; j += i) sieve[j] = 1; }
        }
    }
    printf("primes %d\n", primes);
    crc_init();
    unsigned char *buf = malloc(1 << 22);
    for (int i = 0; i < 1 << 22; i++) buf[i] = (unsigned char)(i * 2654435761u >> 24);
    unsigned c = 0;
    for (int r = 0; r < 8; r++) c ^= crc32(buf, 1 << 22);
    printf("crc %08x\n", c);
    enum { M = 160 };
    static double A[M][M], B[M][M], C[M][M];
    for (int i = 0; i < M; i++) for (int j = 0; j < M; j++) { A[i][j] = (i * j % 7) * 0.5; B[i][j] = (i + j) % 5 - 2.0; }
    for (int r = 0; r < 4; r++)
        for (int i = 0; i < M; i++) for (int j = 0; j < M; j++) { double s = 0; for (int k = 0; k < M; k++) s += A[i][k] * B[k][j]; C[i][j] = s; }
    double tr = 0;
    for (int i = 0; i < M; i++) tr += C[i][i];
    printf("trace %.1f\n", tr);
    int *arr = malloc(sizeof(int) * 1000000);
    unsigned s = 1;
    for (int i = 0; i < 1000000; i++) { s = s * 1103515245 + 12345; arr[i] = (int)(s >> 1); }
    quick(arr, 0, 999999, cmp_int);
    printf("sorted %d %d\n", arr[0] < arr[1], arr[500000] <= arr[500001]);
    return 0;
}
