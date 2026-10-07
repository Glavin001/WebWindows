/* Jump tables, function pointers, recursion, indirect calls. */
#include <stdio.h>

static int classify(int x) {
    switch (x) {
    case 0: return 10;
    case 1: return 21;
    case 2: return 32;
    case 3: return 43;
    case 4: return 54;
    case 5: return 65;
    case 6: return 76;
    case 7: return 87;
    case 9: return 99;
    case 12: return 120;
    default: return -1;
    }
}

static const char *name(int x) {
    switch (x & 7) {
    case 0: return "zero"; case 1: return "one"; case 2: return "two"; case 3: return "three";
    case 4: return "four"; case 5: return "five"; case 6: return "six"; default: return "seven";
    }
}

typedef int (*op_fn)(int, int);
static int add(int a, int b) { return a + b; }
static int sub(int a, int b) { return a - b; }
static int mul(int a, int b) { return a * b; }
static int divi(int a, int b) { return b ? a / b : 0; }
static op_fn ops[] = { add, sub, mul, divi };

static int ackermann(int m, int n) {
    if (m == 0) return n + 1;
    if (n == 0) return ackermann(m - 1, 1);
    return ackermann(m - 1, ackermann(m, n - 1));
}

static unsigned collatz(unsigned n) {
    unsigned steps = 0;
    while (n != 1) { n = (n & 1) ? 3 * n + 1 : n / 2; steps++; }
    return steps;
}

int main(void) {
    int sum = 0;
    for (int i = -2; i < 16; i++) sum = sum * 31 + classify(i);
    printf("classify: %d\n", sum);
    for (int i = 0; i < 10; i++) printf("%s ", name(i * 3));
    printf("\n");
    int acc = 7;
    for (int i = 0; i < 40; i++) acc = ops[i & 3](acc, i + 1) & 0xffff;
    printf("ops: %d\n", acc);
    printf("ackermann(2, 3) = %d\n", ackermann(2, 3));
    unsigned best = 0, arg = 0;
    for (unsigned n = 1; n < 3000; n++) { unsigned s = collatz(n); if (s > best) { best = s; arg = n; } }
    printf("collatz: %u steps for %u\n", best, arg);
    return sum & 0x7f;
}
