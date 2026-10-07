/* Floating point: x87 arithmetic, conversions, comparisons, libm. */
#include <stdio.h>
#include <math.h>

static double horner(const double *c, int n, double x) {
    double r = 0;
    for (int i = n - 1; i >= 0; i--) r = r * x + c[i];
    return r;
}

int main(void) {
    const double c[] = { 1.0, -0.5, 0.25, -0.125, 0.0625 };
    double acc = 0;
    for (int i = 0; i < 100; i++) acc += horner(c, 5, i * 0.01);
    printf("horner %.9f\n", acc);
    float f = 1.0f;
    for (int i = 0; i < 30; i++) f = f * 1.1f + 0.5f;
    printf("float %.4f\n", f);
    double s = 0;
    for (int i = 1; i <= 1000; i++) s += 1.0 / ((double)i * i);
    printf("basel %.12f\n", s);
    printf("sqrt %.10f floor %.1f ceil %.1f fabs %.2f\n", sqrt(2.0), floor(-2.5), ceil(-2.5), fabs(-3.25));
    int conv = 0;
    for (int i = -50; i < 50; i++) { double d = i * 0.37; conv += (int)d + (int)(d * 3); }
    printf("conv %d\n", conv);
    long long big = (long long)(1e15 / 7.0);
    printf("big %lld\n", big);
    unsigned u = (unsigned)3e9;
    printf("u %u\n", u);
    int cmp = 0;
    for (int i = 0; i < 20; i++) { double a = i * 0.1, b = 1.0 - a; cmp = cmp * 3 + (a < b) + 2 * (a == b); }
    printf("cmp %d\n", cmp);
    double z = 0.0;
    printf("nan %d inf %d\n", (z / z) != (z / z), 1.0 / z > 1e308);
    return (int)(s * 10);
}
